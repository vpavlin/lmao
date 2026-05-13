//! `StorageBackend` impl that proxies to Basecamp's `storage_module`
//! through the Logos C++ SDK shim in `logos-core-bindings`.
//!
//! Compared to the embedded-`libstorage` backend, this:
//! - Doesn't link `libstorage_module_blob.a` into our binary —
//!   storage lives in its own logos_host subprocess, owned by
//!   logos-core. No duplicate Codex node when running inside Basecamp.
//! - Shares its node + data dir with every other Basecamp module that
//!   uses storage, so CIDs uploaded here are visible across the app.
//! - Identity / data-dir / discovery-port concerns move to storage_module's
//!   `init(cfgJson)` — the agent module no longer owns those flags.
//!
//! Trade-off: every call goes through QtRO IPC (~ms per round-trip).
//! Fine for audit-log uploads + occasional fetches; not what you'd
//! pick for a hot-path read.
//!
//! Upload path uses the chunk API
//!   uploadInit → uploadChunk* → uploadFinalize → CID
//! because it's fully synchronous on the C++ side and doesn't need the
//! event-subscription channel (which we'd otherwise need to catch
//! `storageUploadDone` from the async `uploadUrl` flavour).
//!
//! Download uses `downloadChunks(cid, false, chunkSize, filepath)` to write
//! to a tempfile. **downloadChunks starts streaming asynchronously** — the
//! method returns `{success: true, value: cid}` immediately, and the actual
//! write completes later via a `storageDownloadDone` event. We subscribe to
//! that event via `Shim::listen` + `Shim::poll_event` before initiating the
//! download, and block until the done event arrives.

use std::sync::Arc;

use async_trait::async_trait;
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use logos_core_bindings::Shim;
use serde_json::Value;

use crate::{StorageBackend, StorageError};

/// 1 MiB per upload chunk. Round number; matches storage_module's docs
/// recommendation and keeps the JSON payload size reasonable
/// (~1.33 MiB after base64 encoding plus QString overhead).
const CHUNK_BYTES: usize = 1 << 20;
/// 30 s timeout for normal storage_module method invocations
/// (init / spr / chunk uploads). DHT walks below.
const SHORT_TIMEOUT_MS: i32 = 30_000;
/// 90 s timeout for fetch / downloadFile — Codex DHT lookups for a
/// CID with no local cache can take tens of seconds.
const LONG_TIMEOUT_MS: i32 = 90_000;

/// Storage backend that drives Basecamp's `storage_module`.
pub struct StorageModuleBackend {
    shim: Arc<Shim>,
}

impl StorageModuleBackend {
    /// Build a backend over a pre-created shim. The shim is shared with
    /// other consumers of LogosAPI in the same process — passing it in
    /// rather than constructing privately means a single Qt event loop
    /// thread services all backends.
    pub fn new(shim: Arc<Shim>) -> Self {
        Self { shim }
    }

    fn call(&self, method: &str, args_json: &str, timeout_ms: i32) -> Result<Value, StorageError> {
        let json = self
            .shim
            .call("storage_module", method, args_json, timeout_ms)
            .map_err(|e| StorageError::Http(format!("storage_module call {method}: {e}")))?;
        serde_json::from_str::<Value>(&json)
            .map_err(|e| StorageError::Http(format!("storage_module {method} bad JSON: {e}")))
    }

    /// Extract a string field from a `StdLogosResult` response, surfacing
    /// the daemon's own error shape (`{"kind":"error","message":...}` /
    /// `{"error":...}`) as `StorageError::Http`.
    fn extract_value(v: Value, field: &str) -> Result<String, StorageError> {
        // Some shim builds return the bare value as a JSON string literal
        // (not wrapped in an object). Handle that first.
        if let Some(s) = v.as_str() {
            return Ok(s.to_owned());
        }
        if let Some(err) = v.get("message").and_then(Value::as_str) {
            if v.get("kind").and_then(Value::as_str) == Some("error") {
                return Err(StorageError::Http(err.into()));
            }
        }
        if let Some(err) = v.get("error").and_then(Value::as_str) {
            return Err(StorageError::Http(err.into()));
        }
        v.get(field)
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| {
                StorageError::Http(format!("missing `{field}` in storage_module response: {v}"))
            })
    }
}

#[async_trait]
impl StorageBackend for StorageModuleBackend {
    async fn upload(&self, data: Vec<u8>) -> Result<String, StorageError> {
        let backend = self.shim.clone();
        let me = StorageModuleBackend { shim: backend };
        // Move the synchronous shim work onto a blocking pool so the
        // tokio executor stays free.
        tokio::task::spawn_blocking(move || {
            // 1. uploadInit("audit.log", chunkSize) → { value: "<sessionId>" }
            let filename = "audit.log";
            let init_args = format!("[\"{}\", {}]", filename, CHUNK_BYTES as i64);
            let resp = me.call("uploadInit", &init_args, SHORT_TIMEOUT_MS)?;
            let session_id = Self::extract_value(resp, "value")?;

            // 2. uploadChunk(sessionId, chunk) for each chunk.
            // The second arg is QByteArray on the C++ side; use the shim's
            // {"__base64__": "<b64>"} convention so it arrives as raw bytes.
            for chunk in data.chunks(CHUNK_BYTES) {
                let b64 = B64.encode(chunk);
                let chunk_args = serde_json::to_string(&serde_json::json!(
                    [session_id, {"__base64__": b64}]
                )).map_err(|e| StorageError::Http(format!("chunk args: {e}")))?;
                let r = me.call("uploadChunk", &chunk_args, SHORT_TIMEOUT_MS)?;
                if let Some(msg) = r.get("message").and_then(Value::as_str) {
                    if r.get("kind").and_then(Value::as_str) == Some("error") {
                        // Best-effort cancel + return.
                        let _ = me.call(
                            "uploadCancel",
                            &serde_json::to_string(&serde_json::json!([session_id]))
                                .unwrap_or_default(),
                            SHORT_TIMEOUT_MS,
                        );
                        return Err(StorageError::Http(format!("uploadChunk: {msg}")));
                    }
                }
            }

            // 3. uploadFinalize(sessionId) → { value: "<cid>" }
            let fin_args = serde_json::to_string(&serde_json::json!([session_id]))
                .map_err(|e| StorageError::Http(format!("finalize args: {e}")))?;
            let resp = me.call("uploadFinalize", &fin_args, SHORT_TIMEOUT_MS)?;
            Self::extract_value(resp, "value")
        })
        .await
        .map_err(|e| StorageError::Http(format!("join: {e}")))?
    }

    async fn download(&self, cid: &str) -> Result<Vec<u8>, StorageError> {
        let backend = self.shim.clone();
        let me = StorageModuleBackend { shim: backend };
        let cid = cid.to_owned();
        tokio::task::spawn_blocking(move || -> Result<Vec<u8>, StorageError> {
            // Subscribe to the done event before starting the download so we
            // don't miss a fast completion. De-duped by the shim (safe to call
            // repeatedly on the same shim instance).
            me.shim.listen("storage_module", "storageDownloadDone")
                .map_err(|e| StorageError::Http(format!("listen: {e}")))?;

            // downloadChunks(cid, local=false, chunkSize, filepath) writes to
            // the file ASYNCHRONOUSLY and returns {success: true, value: cid}
            // immediately. Actual completion comes via `storageDownloadDone`.
            let tmp = tempfile::NamedTempFile::new()
                .map_err(|e| StorageError::Http(format!("tempfile: {e}")))?;
            let path = tmp.path().to_string_lossy().to_string();
            let args = serde_json::to_string(
                &serde_json::json!([cid, false, CHUNK_BYTES as i64, path])
            ).map_err(|e| StorageError::Http(format!("download args: {e}")))?;
            let resp = me.call("downloadChunks", &args, LONG_TIMEOUT_MS)?;
            // downloadChunks returns an error immediately if it can't even start.
            if let Some(err) = resp.get("error").and_then(Value::as_str) {
                return Err(StorageError::Http(format!("downloadChunks: {err}")));
            }
            if let Some(msg) = resp.get("message").and_then(Value::as_str) {
                if resp.get("kind").and_then(Value::as_str) == Some("error") {
                    return Err(StorageError::Http(format!("downloadChunks: {msg}")));
                }
            }

            // Poll for the storageDownloadDone event for this CID.
            // data[0]=success, data[1]=cid, data[2]=bytes_downloaded
            loop {
                let ev = me.shim.poll_event(LONG_TIMEOUT_MS)
                    .map_err(|e| StorageError::Http(format!("poll_event: {e}")))?;
                match ev {
                    None => {
                        return Err(StorageError::Http(
                            format!("storageDownloadDone timed out for {cid}")
                        ));
                    }
                    Some(json) => {
                        let v: Value = serde_json::from_str(&json)
                            .map_err(|e| StorageError::Http(format!("event JSON: {e}")))?;
                        if v.get("event").and_then(Value::as_str)
                            != Some("storageDownloadDone")
                        {
                            continue; // skip unrelated events
                        }
                        let data = v.get("data").and_then(Value::as_array);
                        let success = data
                            .and_then(|d| d.first())
                            .and_then(Value::as_bool)
                            .unwrap_or(false);
                        let event_cid = data
                            .and_then(|d| d.get(1))
                            .and_then(Value::as_str)
                            .unwrap_or("");
                        if event_cid != cid {
                            continue; // event for a different CID
                        }
                        if !success {
                            return Err(StorageError::Http(
                                format!("storageDownloadDone reported failure for {cid}")
                            ));
                        }
                        break; // done!
                    }
                }
            }

            std::fs::read(tmp.path())
                .map_err(|e| StorageError::Http(format!("read back tempfile: {e}")))
        })
        .await
        .map_err(|e| StorageError::Http(format!("join: {e}")))?
    }
}
