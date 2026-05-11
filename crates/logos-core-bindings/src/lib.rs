// Rust bindings to the Logos C++ SDK's LogosAPI, via a C-callable
// shim that lives in `shim/`. JSON in / JSON out; the Rust crate
// never sees a Qt type.
//
// Two build modes, picked by build.rs based on the `LOGOS_CPP_SDK_DIR`
// env var:
//
// - **Real bindings** (env set) — cmake builds the shim + the SDK's
//   `logos_sdk` static archive; bindgen wraps `shim.h`; rustc links
//   both. `Shim::new` boots a Qt event-loop thread + a `LogosAPI`
//   instance; `Shim::call` dispatches synchronously over QtRO.
// - **Stub bindings** (env unset) — `Shim::new` returns
//   `Error::NotCompiledIn` with a hint about how to enable the real
//   build. Lets `cargo check --workspace` pass on a host without
//   Qt6 / Boost / OpenSSL / the SDK installed, which is what CI sees.
//
// build.rs emits `cargo:rustc-cfg=logos_core_real` in the real-build
// case; the two `Shim` impls below sit behind a matching cfg.

#![allow(non_camel_case_types, non_snake_case, dead_code)]

use std::fmt;

#[derive(Debug)]
pub enum Error {
    /// The crate was built without `LOGOS_CPP_SDK_DIR` set — the real
    /// FFI layer was stubbed out. Rebuild with the env var pointing at
    /// a logos-cpp-sdk checkout to enable Shim.
    NotCompiledIn,
    /// The shim's `logos_shim_new` returned NULL (allocation failure
    /// or Qt thread couldn't start).
    InitFailed,
    /// Caller passed a string containing an interior NUL byte.
    NulInArg(&'static str),
    /// Shim returned a byte sequence that isn't UTF-8.
    InvalidUtf8,
    /// The shim returned a JSON error response. The string is the raw
    /// JSON. Shapes mirror the agent module's:
    ///   `{"error": "..."}`
    ///   `{"kind": "error", "message": "..."}`
    ShimError(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::NotCompiledIn => write!(
                f,
                "logos-core-bindings was built without LOGOS_CPP_SDK_DIR — \
                 set the env var to a logos-cpp-sdk checkout and rebuild."
            ),
            Error::InitFailed => write!(f, "logos_shim_new returned null"),
            Error::NulInArg(name) => write!(f, "interior NUL in arg: {name}"),
            Error::InvalidUtf8 => write!(f, "shim returned non-UTF-8 string"),
            Error::ShimError(s) => write!(f, "shim error: {s}"),
        }
    }
}

impl std::error::Error for Error {}

// ── Real impl — only when build.rs found LOGOS_CPP_SDK_DIR ─────────

#[cfg(logos_core_real)]
mod real {
    use super::Error;
    use std::ffi::{CStr, CString};
    use std::os::raw::c_char;

    mod ffi {
        include!(concat!(env!("OUT_DIR"), "/bindings.rs"));
    }

    pub struct Shim {
        inner: *mut ffi::LogosShim,
    }

    // Safe: the C++ shim is internally synchronised; calls from any
    // Rust thread are dispatched onto the Qt thread via QueuedConnection.
    unsafe impl Send for Shim {}
    unsafe impl Sync for Shim {}

    impl Shim {
        pub fn new(module_name: &str) -> Result<Self, Error> {
            let cname = CString::new(module_name).map_err(|_| Error::NulInArg("module_name"))?;
            // SAFETY: cname lives for the call.
            let inner = unsafe { ffi::logos_shim_new(cname.as_ptr()) };
            if inner.is_null() {
                return Err(Error::InitFailed);
            }
            Ok(Self { inner })
        }

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

            // SAFETY: CStrings live for the call; returned pointer is
            // shim-heap-allocated, freed below.
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

        /// Register interest in `event_name` from `module`. After this
        /// returns, matching events get enqueued; drain with `poll_event`.
        /// Repeated calls with the same pair are de-duped by the shim.
        pub fn listen(&self, module: &str, event_name: &str) -> Result<(), Error> {
            let m = CString::new(module).map_err(|_| Error::NulInArg("module"))?;
            let e = CString::new(event_name).map_err(|_| Error::NulInArg("event_name"))?;
            // SAFETY: both CStrings live for the call.
            let ok = unsafe { ffi::logos_shim_listen(self.inner, m.as_ptr(), e.as_ptr()) };
            if ok == 1 {
                Ok(())
            } else {
                Err(Error::ShimError(format!(
                    "{{\"error\":\"listen({module}, {event_name}) failed — module not loaded?\"}}"
                )))
            }
        }

        /// Block up to `timeout_ms` for the next queued event from any
        /// previously-listened (module, event) pair. `Ok(None)` on
        /// timeout, `Ok(Some(json))` on event. JSON shape:
        /// `{"module": "...", "event": "...", "data": [...]}`.
        pub fn poll_event(&self, timeout_ms: i32) -> Result<Option<String>, Error> {
            // SAFETY: returned pointer is shim-heap-allocated or NULL.
            let raw = unsafe { ffi::logos_shim_poll_event(self.inner, timeout_ms) };
            if raw.is_null() {
                return Ok(None);
            }
            let s = unsafe { CStr::from_ptr(raw) }
                .to_str()
                .map(str::to_owned)
                .map_err(|_| Error::InvalidUtf8);
            unsafe { ffi::logos_shim_free_str(raw as *mut c_char) };
            s.map(Some)
        }
    }

    impl Drop for Shim {
        fn drop(&mut self) {
            if !self.inner.is_null() {
                // SAFETY: we own the pointer; only dropped once.
                unsafe { ffi::logos_shim_destroy(self.inner) };
                self.inner = std::ptr::null_mut();
            }
        }
    }
}

// ── Stub impl — when LOGOS_CPP_SDK_DIR wasn't set at build time ─────

#[cfg(not(logos_core_real))]
mod real {
    use super::Error;

    pub struct Shim;

    impl Shim {
        pub fn new(_module_name: &str) -> Result<Self, Error> {
            Err(Error::NotCompiledIn)
        }
        pub fn call(
            &self,
            _target_module: &str,
            _method: &str,
            _args_json: &str,
            _timeout_ms: i32,
        ) -> Result<String, Error> {
            Err(Error::NotCompiledIn)
        }
        pub fn listen(&self, _module: &str, _event_name: &str) -> Result<(), Error> {
            Err(Error::NotCompiledIn)
        }
        pub fn poll_event(&self, _timeout_ms: i32) -> Result<Option<String>, Error> {
            Err(Error::NotCompiledIn)
        }
    }
}

pub use real::Shim;

/// Whether this build of the crate has the real FFI compiled in.
/// `false` when `LOGOS_CPP_SDK_DIR` wasn't set at build time.
pub fn is_real_build() -> bool {
    cfg!(logos_core_real)
}
