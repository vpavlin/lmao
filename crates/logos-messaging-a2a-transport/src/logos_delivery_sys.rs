//! Raw FFI bindings for `liblogosdelivery`.
//!
//! Mirrors `liblogosdelivery/liblogosdelivery.h` from
//! <https://github.com/logos-messaging/logos-delivery>. All functions are
//! `unsafe` and require careful management of the opaque `*mut c_void`
//! context returned by [`logosdelivery_create_node`] — see
//! [`crate::logos_delivery`] for the safe wrapper.
//!
//! Build prerequisite: `liblogosdelivery.so` (or `.a`) on the linker search
//! path, set via `LIBLOGOSDELIVERY_LIB_DIR`.

#![allow(non_camel_case_types, dead_code)]

use std::ffi::{c_char, c_int, c_void};

/// Operation succeeded.
pub const RET_OK: c_int = 0;
/// Operation failed (see callback `msg` for details).
pub const RET_ERR: c_int = 1;
/// Caller did not supply a required callback.
pub const RET_MISSING_CALLBACK: c_int = 2;

/// FFI callback signature used by every async-style call and by the event
/// stream. Invoked from the library's worker thread — keep it fast and
/// non-blocking.
pub type FFICallBack = extern "C" fn(
    caller_ret: c_int,
    msg: *const c_char,
    len: usize,
    user_data: *mut c_void,
);

extern "C" {
    /// Create a new node from a JSON config. Returns an opaque context
    /// pointer (or NULL on failure). The result is also reported via the
    /// callback.
    pub fn logosdelivery_create_node(
        config_json: *const c_char,
        callback: FFICallBack,
        user_data: *mut c_void,
    ) -> *mut c_void;

    /// Start the node. Connects to peers per the config (e.g. the
    /// `logos.dev` preset auto-wires entry nodes).
    pub fn logosdelivery_start_node(
        ctx: *mut c_void,
        callback: FFICallBack,
        user_data: *mut c_void,
    ) -> c_int;

    /// Stop the node, draining in-flight work.
    pub fn logosdelivery_stop_node(
        ctx: *mut c_void,
        callback: FFICallBack,
        user_data: *mut c_void,
    ) -> c_int;

    /// Destroy the node and free its resources. Must be called after stop.
    pub fn logosdelivery_destroy(
        ctx: *mut c_void,
        callback: FFICallBack,
        user_data: *mut c_void,
    ) -> c_int;

    /// Subscribe to a content topic. Inbound messages on this topic surface
    /// as `message_received` events on the registered event callback.
    pub fn logosdelivery_subscribe(
        ctx: *mut c_void,
        callback: FFICallBack,
        user_data: *mut c_void,
        content_topic: *const c_char,
    ) -> c_int;

    /// Unsubscribe from a content topic.
    pub fn logosdelivery_unsubscribe(
        ctx: *mut c_void,
        callback: FFICallBack,
        user_data: *mut c_void,
        content_topic: *const c_char,
    ) -> c_int;

    /// Send a message. `message_json` must contain `contentTopic`,
    /// `payload` (base64), and `ephemeral` fields. Acceptance is reported
    /// via the callback; delivery progress arrives as
    /// `message_sent` / `message_propagated` / `message_error` events.
    pub fn logosdelivery_send(
        ctx: *mut c_void,
        callback: FFICallBack,
        user_data: *mut c_void,
        message_json: *const c_char,
    ) -> c_int;

    /// Register the single event callback that receives all asynchronous
    /// events for this node (received messages, send-state transitions,
    /// connection status, …).
    pub fn logosdelivery_set_event_callback(
        ctx: *mut c_void,
        callback: FFICallBack,
        user_data: *mut c_void,
    );
}
