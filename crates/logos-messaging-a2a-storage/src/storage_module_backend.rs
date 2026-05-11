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
//! `storageUploadDone` from the async `uploadUrl` flavour). Download
//! uses `downloadFile(cid, path, local=false)` to a tempfile and reads
//! the bytes back.

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
                StorageError::Http(format!("missing `{field}` in storage_module response"))
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

            // 2. uploadChunk(sessionId, base64(chunk)) for each chunk.
            for chunk in data.chunks(CHUNK_BYTES) {
                let b64 = B64.encode(chunk);
                let chunk_args = serde_json::to_string(&serde_json::json!([session_id, b64]))
                    .map_err(|e| StorageError::Http(format!("chunk args: {e}")))?;
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
            // downloadFile(cid, path, local=false) — sync per the
            // storage_module docs. Write to a tempfile, read it back.
            let tmp = tempfile::NamedTempFile::new()
                .map_err(|e| StorageError::Http(format!("tempfile: {e}")))?;
            let path = tmp.path().to_string_lossy().to_string();
            let args = serde_json::to_string(&serde_json::json!([cid, path, false]))
                .map_err(|e| StorageError::Http(format!("download args: {e}")))?;
            let resp = me.call("downloadFile", &args, LONG_TIMEOUT_MS)?;
            // Either daemon-side error shape or success.
            if let Some(msg) = resp.get("message").and_then(Value::as_str) {
                if resp.get("kind").and_then(Value::as_str) == Some("error") {
                    return Err(StorageError::Http(format!("downloadFile: {msg}")));
                }
            }
            std::fs::read(tmp.path())
                .map_err(|e| StorageError::Http(format!("read back tempfile: {e}")))
        })
        .await
        .map_err(|e| StorageError::Http(format!("join: {e}")))?
    }
}
