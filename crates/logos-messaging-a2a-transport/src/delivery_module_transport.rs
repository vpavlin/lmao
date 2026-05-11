//! `Transport` impl that proxies to Basecamp's `delivery_module`
//! through the Logos C++ SDK shim in `logos-core-bindings`.
//!
//! Compared to the embedded-`liblogosdelivery` transport, this:
//! - Doesn't link `liblogosdelivery.so` into our binary —
//!   the Waku node lives in its own logos_host subprocess, owned by
//!   logos-core. No duplicate Waku node when running inside Basecamp.
//! - Shares the node + its peer mesh with every other Basecamp module
//!   that uses messaging.
//! - Node config + transport-bind concerns move to delivery_module's
//!   `createNode(cfgJson)` — the agent module no longer owns those.
//!
//! ## Event plumbing
//!
//! `delivery_module` emits `messageReceived` as a QVariantList of
//! `[messageHash, contentTopic, payload_b64, timestampStr]`. Through
//! the shim's `onEvent` bridge, that arrives as the JSON document
//! `{"module": "delivery_module", "event": "messageReceived",
//!   "data": ["<hash>", "<topic>", "<b64>", "<ts>"]}`. A dedicated
//! blocking poll task drains the shim event queue, parses each
//! envelope, base64-decodes the payload, and forwards to the
//! per-topic subscriber channel.
//!
//! ## Caveat: single consumer of the shim event queue
//!
//! `Shim::poll_event` is a process-global drain across every
//! `(module, event)` pair previously passed to `Shim::listen`.
//! While this transport's poll task is running, nothing else in the
//! process can call `poll_event` without racing for events. If you
//! need additional listeners (e.g. for `messageSent` confirmations
//! or for storage upload progress), fan them out from this single
//! poll loop rather than spawning a second one.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use logos_core_bindings::Shim;
use serde_json::Value;
use tokio::sync::mpsc;

use crate::{Result, Transport, TransportError};

const MODULE: &str = "delivery_module";
const EVENT_MESSAGE_RECEIVED: &str = "messageReceived";

/// 30 s timeout for normal delivery_module method invocations
/// (send / subscribe / unsubscribe).
const SHORT_TIMEOUT_MS: i32 = 30_000;
/// 120 s timeout for `createNode` + `start` — Waku startup can take
/// 15-90 s depending on cluster fetch + bootstrap dial latency. The
/// shim call is synchronous, so the caller's first publish/subscribe
/// can't fire until this returns.
const STARTUP_TIMEOUT_MS: i32 = 120_000;
/// Per-iteration poll timeout for the event-drain task. Short so the
/// loop reacts promptly to shutdown signals; long enough not to spin.
const POLL_TIMEOUT_MS: i32 = 250;

type SubMap = Arc<Mutex<HashMap<String, mpsc::Sender<Vec<u8>>>>>;

/// Transport that drives Basecamp's `delivery_module`.
pub struct DeliveryModuleTransport {
    shim: Arc<Shim>,
    subscriptions: SubMap,
    /// Held to signal the poll task to exit on Drop.
    shutdown_tx: Option<tokio::sync::oneshot::Sender<()>>,
}

impl DeliveryModuleTransport {
    /// Boot the transport: register on `delivery_module`, call
    /// `createNode(cfgJson)` + `start()`, subscribe to the
    /// `messageReceived` event, and spawn the poll task.
    ///
    /// `cfg_json` is the same JSON string the delivery_module's
    /// `createNode` expects — see its `getAvailableConfigs()` output
    /// for the schema (cluster id, pubsub topics, port bindings, …).
    pub async fn new(shim: Arc<Shim>, cfg_json: &str) -> Result<Self> {
        let backend = shim.clone();
        let cfg_owned = cfg_json.to_owned();

        // createNode + start are synchronous; run on the blocking pool.
        let setup = tokio::task::spawn_blocking(move || -> Result<()> {
            let create_args = serde_json::to_string(&serde_json::json!([cfg_owned]))
                .map_err(|e| TransportError::Transport(format!("createNode args: {e}")))?;
            let resp = call(&backend, "createNode", &create_args, STARTUP_TIMEOUT_MS)?;
            // delivery_module's createNode returns a plain `bool` —
            // serialised as either `true` or an error object.
            check_bool(&resp, "createNode")?;

            let resp = call(&backend, "start", "[]", STARTUP_TIMEOUT_MS)?;
            check_bool(&resp, "start")?;

            backend
                .listen(MODULE, EVENT_MESSAGE_RECEIVED)
                .map_err(|e| TransportError::Transport(format!("listen: {e}")))?;
            Ok(())
        })
        .await
        .map_err(|e| TransportError::Transport(format!("join: {e}")))?;
        setup?;

        let subscriptions: SubMap = Arc::new(Mutex::new(HashMap::new()));
        let (shutdown_tx, mut shutdown_rx) = tokio::sync::oneshot::channel::<()>();

        let poll_shim = shim.clone();
        let poll_subs = subscriptions.clone();
        tokio::task::spawn_blocking(move || {
            loop {
                // Cheap, non-blocking shutdown check.
                if let Ok(()) | Err(tokio::sync::oneshot::error::TryRecvError::Closed) =
                    shutdown_rx.try_recv()
                {
                    break;
                }
                match poll_shim.poll_event(POLL_TIMEOUT_MS) {
                    Ok(None) => continue,
                    Ok(Some(json)) => dispatch_event(&json, &poll_subs),
                    Err(e) => {
                        tracing::warn!(error = %e, "delivery_module poll_event error; backing off");
                        std::thread::sleep(std::time::Duration::from_millis(500));
                    }
                }
            }
        });

        Ok(Self {
            shim,
            subscriptions,
            shutdown_tx: Some(shutdown_tx),
        })
    }
}

impl Drop for DeliveryModuleTransport {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
    }
}

#[async_trait]
impl Transport for DeliveryModuleTransport {
    async fn publish(&self, topic: &str, payload: &[u8]) -> Result<()> {
        let payload_b64 = B64.encode(payload);
        let topic = topic.to_owned();
        let backend = self.shim.clone();
        tokio::task::spawn_blocking(move || -> Result<()> {
            let args = serde_json::to_string(&serde_json::json!([topic, payload_b64]))
                .map_err(|e| TransportError::Transport(format!("send args: {e}")))?;
            let resp = call(&backend, "send", &args, SHORT_TIMEOUT_MS)?;
            // QExpected<QString>: success → {"value": "<hash>"}, error → {"error": ...}
            if let Some(err) = error_message(&resp) {
                return Err(TransportError::Transport(format!("send: {err}")));
            }
            Ok(())
        })
        .await
        .map_err(|e| TransportError::Transport(format!("join: {e}")))??;
        Ok(())
    }

    async fn subscribe(&self, topic: &str) -> Result<mpsc::Receiver<Vec<u8>>> {
        let topic_owned = topic.to_owned();
        let backend = self.shim.clone();
        tokio::task::spawn_blocking(move || -> Result<()> {
            let args = serde_json::to_string(&serde_json::json!([topic_owned]))
                .map_err(|e| TransportError::Transport(format!("subscribe args: {e}")))?;
            let resp = call(&backend, "subscribe", &args, SHORT_TIMEOUT_MS)?;
            check_bool(&resp, "subscribe")?;
            Ok(())
        })
        .await
        .map_err(|e| TransportError::Transport(format!("join: {e}")))??;

        let (tx, rx) = mpsc::channel::<Vec<u8>>(256);
        self.subscriptions
            .lock()
            .unwrap()
            .insert(topic.to_owned(), tx);
        Ok(rx)
    }

    async fn unsubscribe(&self, topic: &str) -> Result<()> {
        self.subscriptions.lock().unwrap().remove(topic);
        let topic = topic.to_owned();
        let backend = self.shim.clone();
        tokio::task::spawn_blocking(move || -> Result<()> {
            let args = serde_json::to_string(&serde_json::json!([topic]))
                .map_err(|e| TransportError::Transport(format!("unsubscribe args: {e}")))?;
            let resp = call(&backend, "unsubscribe", &args, SHORT_TIMEOUT_MS)?;
            check_bool(&resp, "unsubscribe")?;
            Ok(())
        })
        .await
        .map_err(|e| TransportError::Transport(format!("join: {e}")))??;
        Ok(())
    }
}

fn call(shim: &Shim, method: &str, args_json: &str, timeout_ms: i32) -> Result<Value> {
    let raw = shim
        .call(MODULE, method, args_json, timeout_ms)
        .map_err(|e| TransportError::Transport(format!("delivery_module {method}: {e}")))?;
    serde_json::from_str::<Value>(&raw)
        .map_err(|e| TransportError::Transport(format!("delivery_module {method} bad JSON: {e}")))
}

/// Pull the daemon's error string out of a response, if any. Matches
/// both shapes: `{"error": "..."}` and `{"kind": "error", "message": "..."}`.
fn error_message(v: &Value) -> Option<String> {
    if let Some(msg) = v.get("message").and_then(Value::as_str) {
        if v.get("kind").and_then(Value::as_str) == Some("error") {
            return Some(msg.to_owned());
        }
    }
    v.get("error").and_then(Value::as_str).map(str::to_owned)
}

/// For methods that return `bool`. Accept any `true`-looking shape and
/// surface anything else as a transport error.
fn check_bool(v: &Value, method: &str) -> Result<()> {
    if let Some(err) = error_message(v) {
        return Err(TransportError::Transport(format!("{method}: {err}")));
    }
    // The shim wraps a bare `true` as the JSON value `true`.
    if v.as_bool() == Some(true) {
        return Ok(());
    }
    // …but some module versions wrap the success in {"value": true}.
    if v.get("value").and_then(Value::as_bool) == Some(true) {
        return Ok(());
    }
    Err(TransportError::Transport(format!(
        "{method}: unexpected response {v}"
    )))
}

/// Parse one event envelope from the shim and forward the payload to
/// the matching topic subscriber. Silently drops malformed envelopes
/// — they're a protocol bug, not something the application can fix.
fn dispatch_event(json: &str, subs: &SubMap) {
    let Ok(env) = serde_json::from_str::<Value>(json) else {
        return;
    };
    if env.get("event").and_then(Value::as_str) != Some(EVENT_MESSAGE_RECEIVED) {
        return;
    }
    let Some(data) = env.get("data").and_then(Value::as_array) else {
        return;
    };
    // Shape: [messageHash, contentTopic, payload_b64, timestampStr]
    let topic = data.get(1).and_then(Value::as_str).unwrap_or("");
    let payload_b64 = data.get(2).and_then(Value::as_str).unwrap_or("");
    if topic.is_empty() {
        return;
    }
    let Ok(payload) = B64.decode(payload_b64) else {
        return;
    };
    let tx = {
        let guard = subs.lock().unwrap();
        guard.get(topic).cloned()
    };
    if let Some(tx) = tx {
        // Best-effort forward — if the receiver dropped or is lagging,
        // skip rather than block the poll task.
        let _ = tx.try_send(payload);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_message_kind_error_shape() {
        let v: Value = serde_json::from_str(r#"{"kind":"error","message":"boom"}"#).unwrap();
        assert_eq!(error_message(&v).as_deref(), Some("boom"));
    }

    #[test]
    fn error_message_plain_error_shape() {
        let v: Value = serde_json::from_str(r#"{"error":"nope"}"#).unwrap();
        assert_eq!(error_message(&v).as_deref(), Some("nope"));
    }

    #[test]
    fn error_message_none_for_success() {
        let v: Value = serde_json::from_str(r#"{"value":"abc"}"#).unwrap();
        assert!(error_message(&v).is_none());
    }

    #[test]
    fn check_bool_accepts_bare_true() {
        let v: Value = serde_json::from_str("true").unwrap();
        assert!(check_bool(&v, "x").is_ok());
    }

    #[test]
    fn check_bool_accepts_wrapped_value_true() {
        let v: Value = serde_json::from_str(r#"{"value":true}"#).unwrap();
        assert!(check_bool(&v, "x").is_ok());
    }

    #[test]
    fn check_bool_rejects_false() {
        let v: Value = serde_json::from_str("false").unwrap();
        assert!(check_bool(&v, "x").is_err());
    }

    #[test]
    fn check_bool_surfaces_error_message() {
        let v: Value = serde_json::from_str(r#"{"kind":"error","message":"bad cfg"}"#).unwrap();
        let err = check_bool(&v, "createNode").unwrap_err();
        assert!(err.to_string().contains("bad cfg"));
    }

    #[tokio::test]
    async fn dispatch_event_forwards_payload_to_matching_topic() {
        let subs: SubMap = Arc::new(Mutex::new(HashMap::new()));
        let (tx, mut rx) = mpsc::channel::<Vec<u8>>(8);
        subs.lock().unwrap().insert("/x/1/foo/proto".into(), tx);

        let payload_b64 = B64.encode(b"hello-from-delivery");
        let envelope = serde_json::json!({
            "module": "delivery_module",
            "event": "messageReceived",
            "data": ["msghash", "/x/1/foo/proto", payload_b64, "1700000000000"],
        })
        .to_string();
        dispatch_event(&envelope, &subs);

        let got = tokio::time::timeout(std::time::Duration::from_millis(50), rx.recv())
            .await
            .expect("payload should arrive")
            .expect("channel still open");
        assert_eq!(got, b"hello-from-delivery");
    }

    #[tokio::test]
    async fn dispatch_event_ignores_unsubscribed_topic() {
        let subs: SubMap = Arc::new(Mutex::new(HashMap::new()));
        let (tx, mut rx) = mpsc::channel::<Vec<u8>>(8);
        subs.lock().unwrap().insert("/x/1/foo/proto".into(), tx);

        let envelope = serde_json::json!({
            "module": "delivery_module",
            "event": "messageReceived",
            "data": ["msghash", "/x/1/other/proto", B64.encode(b"ignored"), "0"],
        })
        .to_string();
        dispatch_event(&envelope, &subs);

        let res = tokio::time::timeout(std::time::Duration::from_millis(20), rx.recv()).await;
        assert!(res.is_err(), "no message expected for unsubscribed topic");
    }

    #[test]
    fn dispatch_event_silently_drops_non_message_event() {
        let subs: SubMap = Arc::new(Mutex::new(HashMap::new()));
        let envelope = serde_json::json!({
            "module": "delivery_module",
            "event": "connectionStateChanged",
            "data": ["connected", "0"],
        })
        .to_string();
        dispatch_event(&envelope, &subs); // no panic, no subscriber, no crash
    }

    #[test]
    fn dispatch_event_silently_drops_malformed_envelope() {
        let subs: SubMap = Arc::new(Mutex::new(HashMap::new()));
        dispatch_event("not-json", &subs);
        dispatch_event(r#"{"module":"delivery_module"}"#, &subs);
    }
}
