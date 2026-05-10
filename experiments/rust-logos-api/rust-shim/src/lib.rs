// Safe-ish Rust wrapper over the C ABI shim. Hides the unsafe FFI +
// the C-string / heap-malloc dance behind a Rust-shaped interface.
//
// The wrapper is intentionally tiny — JSON in, JSON out, mirrors the
// shim. Higher-level type-safe methods (`info() -> AgentInfo`,
// `delegate(...) -> impl Future<...>`) are out of scope for the
// experiment; they belong in the actual `refactor/cli-as-remote-consumer`
// crate that this work unblocks.

#![allow(non_camel_case_types, non_snake_case, dead_code)]

use std::ffi::{CStr, CString};
use std::fmt;
use std::os::raw::c_char;

mod ffi {
    include!(concat!(env!("OUT_DIR"), "/bindings.rs"));
}

#[derive(Debug)]
pub enum Error {
    NullModuleName,
    InitFailed,
    NulInArg(&'static str),
    InvalidUtf8,
    /// The shim returned a JSON error response. Caller can either
    /// branch on this `String` (raw JSON) or parse it themselves.
    /// The shim's error shapes mirror the agent module's:
    ///   `{"error": "..."}`
    ///   `{"kind": "error", "message": "..."}`
    ShimError(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::NullModuleName => write!(f, "module_name was null"),
            Error::InitFailed => write!(f, "logos_shim_new returned null"),
            Error::NulInArg(name) => write!(f, "interior NUL in arg: {name}"),
            Error::InvalidUtf8 => write!(f, "shim returned non-UTF-8 string"),
            Error::ShimError(s) => write!(f, "shim error: {s}"),
        }
    }
}

impl std::error::Error for Error {}

/// Owns a C++ shim instance + its Qt event-loop thread. Drop joins
/// the thread.
pub struct Shim {
    inner: *mut ffi::LogosShim,
}

// Safe: the C++ shim is internally synchronised; calls from any Rust
// thread are dispatched onto the Qt thread via QueuedConnection.
unsafe impl Send for Shim {}
unsafe impl Sync for Shim {}

impl Shim {
    /// Spin up a Qt event-loop thread + LogosAPI instance.
    /// `module_name` is the label other modules see in the registry —
    /// pick something distinct from real module names.
    pub fn new(module_name: &str) -> Result<Self, Error> {
        let cname =
            CString::new(module_name).map_err(|_| Error::NulInArg("module_name"))?;
        // SAFETY: cname lives for the duration of this call.
        let inner = unsafe { ffi::logos_shim_new(cname.as_ptr()) };
        if inner.is_null() {
            return Err(Error::InitFailed);
        }
        Ok(Self { inner })
    }

    /// Synchronously invoke a method. `args` is a JSON array
    /// (e.g. `"[]"` for no-arg, `"[\"abc\"]"` for one string arg).
    /// Returns the raw JSON response. Errors in the shim layer
    /// (timeout, null target, JSON parse) come back as `Err(ShimError)`
    /// with the JSON body. *Successful* calls that return module-side
    /// errors (e.g. `agent.info()` when the daemon isn't running) are
    /// returned as `Ok(json)`; the caller decides how to interpret.
    pub fn call(
        &self,
        target_module: &str,
        method: &str,
        args_json: &str,
        timeout_ms: i32,
    ) -> Result<String, Error> {
        let target = CString::new(target_module).map_err(|_| Error::NulInArg("target"))?;
        let meth = CString::new(method).map_err(|_| Error::NulInArg("method"))?;
        let args = CString::new(args_json).map_err(|_| Error::NulInArg("args_json"))?;

        // SAFETY: all CStrings live for the call; the returned pointer
        // is heap-allocated by the shim and we free it via
        // logos_shim_free_str.
        let raw = unsafe {
            ffi::logos_shim_call(
                self.inner,
                target.as_ptr(),
                meth.as_ptr(),
                args.as_ptr(),
                timeout_ms,
            )
        };
        if raw.is_null() {
            return Err(Error::ShimError(
                "{\"error\":\"shim returned null pointer\"}".into(),
            ));
        }
        let s = unsafe { CStr::from_ptr(raw) }
            .to_str()
            .map(str::to_owned)
            .map_err(|_| Error::InvalidUtf8);
        unsafe { ffi::logos_shim_free_str(raw as *mut c_char) };
        s
    }
}

impl Drop for Shim {
    fn drop(&mut self) {
        if !self.inner.is_null() {
            // SAFETY: we own the pointer and only drop once.
            unsafe { ffi::logos_shim_destroy(self.inner) };
            self.inner = std::ptr::null_mut();
        }
    }
}
