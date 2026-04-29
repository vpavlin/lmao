//! Native [`Transport`] backed by `liblogosdelivery`.
//!
//! Embeds a Logos Messaging node in-process via the `liblogosdelivery` C
//! FFI. The node config takes a `preset` (e.g. `"logos.dev"`) which
//! auto-wires entry nodes, cluster ID, and sharding — no manual peer
//! configuration required.
//!
//! The FFI is callback-based; this module bridges to async Rust two ways:
//!
//! - **Per-call callbacks** ([`call_trampoline`]) — one-shot futures that
//!   resolve when the lib reports completion of `create_node` / `start_node`
//!   / `subscribe` / `send` / etc.
//! - **Event callback** ([`event_trampoline`]) — single hot callback
//!   registered for the lifetime of the node; dispatches `message_received`
//!   events into per-content-topic [`mpsc::Sender`]s.
//!
//! Build prerequisite: `liblogosdelivery.so` on the linker search path.
//! Set `LIBLOGOSDELIVERY_LIB_DIR` so `build.rs` can find it.

use crate::logos_delivery_sys::*;
use crate::{Result, Transport, TransportError};
use async_trait::async_trait;
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::ffi::{c_char, c_int, c_void, CString};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};
use tokio::sync::mpsc;

const CHANNEL_BUFFER: usize = 256;

/// Configuration for the embedded Logos Messaging node.
///
/// Field names match `WakuNodeConf` in the upstream Nim project. Use
/// [`NodeConfig::logos_dev`] for the typical demo configuration.
#[derive(Debug, Serialize, Default, Clone)]
pub struct NodeConfig {
    #[serde(rename = "logLevel", skip_serializing_if = "Option::is_none")]
    pub log_level: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    /// Network preset — `"logos.dev"`, `"twn"`, etc. The preset auto-wires
    /// entry nodes, cluster ID, sharding, and content-topic → pubsub
    /// translation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preset: Option<String>,
    #[serde(rename = "tcpPort", skip_serializing_if = "Option::is_none")]
    pub tcp_port: Option<u16>,
    #[serde(rename = "discv5UdpPort", skip_serializing_if = "Option::is_none")]
    pub discv5_udp_port: Option<u16>,
}

impl NodeConfig {
    /// The standard demo configuration: connect to the `logos.dev` fleet
    /// in Core mode with INFO logs.
    pub fn logos_dev() -> Self {
        Self {
            log_level: Some("INFO".to_string()),
            mode: Some("Core".to_string()),
            preset: Some("logos.dev".to_string()),
            tcp_port: None,
            discv5_udp_port: None,
        }
    }
}

// ---------- per-call future bridge -----------------------------------------

struct CallState {
    result: Option<std::result::Result<String, String>>,
    waker: Option<Waker>,
}

struct CallFuture {
    state: Arc<Mutex<CallState>>,
}

impl Future for CallFuture {
    type Output = std::result::Result<String, String>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut state = self.state.lock().unwrap();
        if let Some(r) = state.result.take() {
            Poll::Ready(r)
        } else {
            state.waker = Some(cx.waker().clone());
            Poll::Pending
        }
    }
}

extern "C" fn call_trampoline(
    ret: c_int,
    msg: *const c_char,
    len: usize,
    user_data: *mut c_void,
) {
    if user_data.is_null() {
        return;
    }
    // SAFETY: `user_data` was produced by `Arc::into_raw` in `make_call_ud`
    // and is consumed exactly once here.
    let state = unsafe { Arc::from_raw(user_data as *const Mutex<CallState>) };
    let text = if msg.is_null() || len == 0 {
        String::new()
    } else {
        // SAFETY: `msg`/`len` describe a valid byte buffer for the duration
        // of this callback per the FFI contract.
        let slice = unsafe { std::slice::from_raw_parts(msg as *const u8, len) };
        String::from_utf8_lossy(slice).into_owned()
    };
    let value = if ret == RET_OK { Ok(text) } else { Err(text) };
    let mut guard = state.lock().unwrap();
    guard.result = Some(value);
    if let Some(w) = guard.waker.take() {
        w.wake();
    }
}

/// Build a fresh `CallState` and the `*mut c_void` pointer to hand to C.
/// The C side owns one strong ref; the returned `Arc` owns the other.
fn make_call_ud() -> (Arc<Mutex<CallState>>, *mut c_void) {
    let state = Arc::new(Mutex::new(CallState {
        result: None,
        waker: None,
    }));
    let raw = Arc::into_raw(state.clone()) as *mut c_void;
    (state, raw)
}

/// Reclaim the call-site `Arc` after a synchronous failure (rc != RET_OK)
/// where the C side will not invoke the callback.
unsafe fn drop_call_ud(ud: *mut c_void) {
    if !ud.is_null() {
        let _ = unsafe { Arc::from_raw(ud as *const Mutex<CallState>) };
    }
}

// ---------- event callback (hot path) --------------------------------------

struct EventState {
    senders: Arc<Mutex<HashMap<String, mpsc::Sender<Vec<u8>>>>>,
}

#[derive(Deserialize)]
struct ReceivedMessage {
    #[serde(rename = "contentTopic")]
    content_topic: String,
    /// Inbound payloads arrive as JSON byte arrays (e.g. `[72, 105]`).
    payload: Vec<u8>,
}

#[derive(Deserialize)]
struct InboundEvent {
    #[serde(rename = "eventType")]
    event_type: String,
    #[serde(default)]
    message: Option<ReceivedMessage>,
}

extern "C" fn event_trampoline(
    ret: c_int,
    msg: *const c_char,
    len: usize,
    user_data: *mut c_void,
) {
    if ret != RET_OK || msg.is_null() || len == 0 || user_data.is_null() {
        return;
    }
    // SAFETY: `user_data` was set by `set_event_callback` to a pointer
    // produced by `Arc::into_raw(EventState)` and is kept alive for the
    // node's lifetime by the transport.
    let state: &EventState = unsafe { &*(user_data as *const EventState) };
    let slice = unsafe { std::slice::from_raw_parts(msg as *const u8, len) };
    let json = match std::str::from_utf8(slice) {
        Ok(s) => s,
        Err(_) => return,
    };
    let event: InboundEvent = match serde_json::from_str(json) {
        Ok(e) => e,
        Err(_) => return,
    };
    if event.event_type != "message_received" {
        return;
    }
    let Some(received) = event.message else { return };
    if let Ok(senders) = state.senders.lock() {
        if let Some(tx) = senders.get(&received.content_topic) {
            let _ = tx.try_send(received.payload);
        }
    }
}

// ---------- transport wrapper ----------------------------------------------

/// Native Logos Messaging transport.
///
/// Created via [`LogosDeliveryTransport::new`]; call
/// [`LogosDeliveryTransport::shutdown`] before dropping. Failing to call
/// `shutdown` leaks one `Arc<EventState>` per transport instance and may
/// leave the embedded node running until process exit.
pub struct LogosDeliveryTransport {
    ctx: *mut c_void,
    senders: Arc<Mutex<HashMap<String, mpsc::Sender<Vec<u8>>>>>,
    /// Strong ref kept alongside the C-owned ref; reclaimed in `shutdown`.
    _event_state: Arc<EventState>,
    /// Raw pointer registered with `set_event_callback`; reclaimed in
    /// `shutdown`.
    event_ud: *mut c_void,
}

// SAFETY: `ctx` and `event_ud` are opaque pointers into a Nim runtime that
// serialises all FFI calls through an internal worker thread, making them
// safe to share across Rust threads.
unsafe impl Send for LogosDeliveryTransport {}
unsafe impl Sync for LogosDeliveryTransport {}

impl LogosDeliveryTransport {
    /// Create and start a Logos Messaging node with the given config.
    pub async fn new(config: NodeConfig) -> Result<Self> {
        let cfg_json = serde_json::to_string(&config)?;
        let cfg_c = CString::new(cfg_json).map_err(|e| TransportError::Other(e.to_string()))?;

        // create_node
        let (state, ud) = make_call_ud();
        // SAFETY: cfg_c lives until we await below; trampoline owns `ud`.
        let ctx = unsafe { logosdelivery_create_node(cfg_c.as_ptr(), call_trampoline, ud) };
        if ctx.is_null() {
            unsafe { drop_call_ud(ud) };
            return Err(TransportError::Transport(
                "create_node returned NULL".into(),
            ));
        }
        CallFuture { state }
            .await
            .map_err(|e| TransportError::Transport(format!("create_node failed: {}", e)))?;

        // event callback registration (must happen before start_node so we
        // don't miss early connection events).
        let senders: Arc<Mutex<HashMap<String, mpsc::Sender<Vec<u8>>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let event_state = Arc::new(EventState {
            senders: senders.clone(),
        });
        let event_ud = Arc::into_raw(event_state.clone()) as *mut c_void;
        // SAFETY: event_ud is a valid Arc<EventState> raw pointer kept alive
        // by `_event_state` until `shutdown` reclaims it.
        unsafe {
            logosdelivery_set_event_callback(ctx, event_trampoline, event_ud);
        }

        // start_node
        let (start_state, start_ud) = make_call_ud();
        // SAFETY: trampoline owns `start_ud`.
        let rc = unsafe { logosdelivery_start_node(ctx, call_trampoline, start_ud) };
        if rc != RET_OK {
            unsafe { drop_call_ud(start_ud) };
            return Err(TransportError::Transport(format!(
                "start_node returned rc={}",
                rc
            )));
        }
        CallFuture { state: start_state }
            .await
            .map_err(|e| TransportError::Transport(format!("start_node failed: {}", e)))?;

        Ok(Self {
            ctx,
            senders,
            _event_state: event_state,
            event_ud,
        })
    }

    /// Stop the node, destroy it, and reclaim the event-callback ref count.
    /// Idempotent only in the sense that calling it twice is undefined; do
    /// not.
    pub async fn shutdown(self) -> Result<()> {
        // stop
        let (s, ud) = make_call_ud();
        let rc = unsafe { logosdelivery_stop_node(self.ctx, call_trampoline, ud) };
        if rc == RET_OK {
            let _ = CallFuture { state: s }.await;
        } else {
            unsafe { drop_call_ud(ud) };
        }

        // destroy
        let (s, ud) = make_call_ud();
        let rc = unsafe { logosdelivery_destroy(self.ctx, call_trampoline, ud) };
        if rc == RET_OK {
            let _ = CallFuture { state: s }.await;
        } else {
            unsafe { drop_call_ud(ud) };
        }

        // Reclaim the C-owned Arc<EventState>.
        // SAFETY: event_ud was produced by Arc::into_raw in `new`; this is
        // its single matching from_raw.
        let _ = unsafe { Arc::from_raw(self.event_ud as *const EventState) };

        Ok(())
    }
}

#[derive(Serialize)]
struct OutgoingMessage<'a> {
    #[serde(rename = "contentTopic")]
    content_topic: &'a str,
    /// Base64-encoded payload bytes.
    payload: String,
    ephemeral: bool,
}

#[async_trait]
impl Transport for LogosDeliveryTransport {
    async fn publish(&self, topic: &str, payload: &[u8]) -> Result<()> {
        let msg = OutgoingMessage {
            content_topic: topic,
            payload: B64.encode(payload),
            ephemeral: false,
        };
        let json = serde_json::to_string(&msg)?;
        let cs = CString::new(json).map_err(|e| TransportError::Other(e.to_string()))?;
        let (state, ud) = make_call_ud();
        // SAFETY: cs lives until the await completes; trampoline owns `ud`.
        let rc = unsafe { logosdelivery_send(self.ctx, call_trampoline, ud, cs.as_ptr()) };
        if rc != RET_OK {
            unsafe { drop_call_ud(ud) };
            return Err(TransportError::Transport(format!("send rc={}", rc)));
        }
        CallFuture { state }
            .await
            .map_err(|e| TransportError::Transport(format!("send failed: {}", e)))?;
        Ok(())
    }

    async fn subscribe(&self, topic: &str) -> Result<mpsc::Receiver<Vec<u8>>> {
        let cs = CString::new(topic).map_err(|e| TransportError::Other(e.to_string()))?;
        let (state, ud) = make_call_ud();
        // SAFETY: cs lives until the await completes; trampoline owns `ud`.
        let rc = unsafe { logosdelivery_subscribe(self.ctx, call_trampoline, ud, cs.as_ptr()) };
        if rc != RET_OK {
            unsafe { drop_call_ud(ud) };
            return Err(TransportError::Transport(format!("subscribe rc={}", rc)));
        }
        CallFuture { state }
            .await
            .map_err(|e| TransportError::Transport(format!("subscribe failed: {}", e)))?;

        let (tx, rx) = mpsc::channel(CHANNEL_BUFFER);
        self.senders
            .lock()
            .map_err(|e| TransportError::Transport(format!("lock poisoned: {}", e)))?
            .insert(topic.to_string(), tx);
        Ok(rx)
    }

    async fn unsubscribe(&self, topic: &str) -> Result<()> {
        let cs = CString::new(topic).map_err(|e| TransportError::Other(e.to_string()))?;
        let (state, ud) = make_call_ud();
        // SAFETY: cs lives until the await completes; trampoline owns `ud`.
        let rc = unsafe { logosdelivery_unsubscribe(self.ctx, call_trampoline, ud, cs.as_ptr()) };
        if rc != RET_OK {
            unsafe { drop_call_ud(ud) };
            return Err(TransportError::Transport(format!("unsubscribe rc={}", rc)));
        }
        let _ = CallFuture { state }.await;
        self.senders
            .lock()
            .map_err(|e| TransportError::Transport(format!("lock poisoned: {}", e)))?
            .remove(topic);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_serialises_with_logos_dev_preset() {
        let json = serde_json::to_string(&NodeConfig::logos_dev()).unwrap();
        assert!(json.contains("\"preset\":\"logos.dev\""));
        assert!(json.contains("\"mode\":\"Core\""));
        assert!(json.contains("\"logLevel\":\"INFO\""));
    }

    #[test]
    fn config_omits_unset_ports() {
        let json = serde_json::to_string(&NodeConfig::logos_dev()).unwrap();
        assert!(!json.contains("tcpPort"));
        assert!(!json.contains("discv5UdpPort"));
    }
}
