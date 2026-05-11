// C ABI shim over LogosAPI. Phase B of the experiment tracked in
// https://github.com/vpavlin/lmao/issues/19 — see ../../README.md.
//
// The shim owns a QCoreApplication on a dedicated thread, owns a
// `LogosAPI` instance, and serialises calls onto the Qt thread via
// QMetaObject::invokeMethod(Qt::QueuedConnection). Callers from any
// thread (Rust's tokio runtime, plain Rust threads, …) get a
// blocking C-callable interface that deals only in C strings + ints.
//
// JSON in, JSON out. The caller passes args as a JSON array string
// ("[]" for no-arg methods, '["pubkey","text"]' for typed args).
// Inside, the shim parses it to QVariantList, marshals to the Qt
// thread, calls invokeRemoteMethod, serialises the QVariant result
// back to JSON. The caller frees the result with
// `logos_shim_free_str`.
//
// Thread safety: `logos_shim_call` is reentrant — multiple threads
// may call it concurrently. Each call gets its own
// (mutex, condvar, done flag, result) tuple captured by the lambda
// posted to the Qt thread; the Qt thread serialises the actual
// invokeRemoteMethod calls.
//
// Outer timeout: `timeout_ms` IS honored even when the QtRO registry
// is unreachable (which is what bit Phase A — see README). The shim
// adds a small grace window on top of the value passed to
// `Timeout(...)` so the inner SDK timeout fires before the outer one.
//
// Lifetime: `logos_shim_destroy` posts a quit to the Qt event loop
// and joins the thread. After that the handle is invalid; do not
// reuse.

#ifndef LOGOS_SHIM_H
#define LOGOS_SHIM_H

#ifdef __cplusplus
extern "C" {
#endif

typedef struct LogosShim LogosShim;

// Create the shim: spin up the Qt thread, instantiate LogosAPI(module_name).
// `module_name` is what other modules see in the registry — pick a name
// distinct from real modules' names so logoscore-side logs stay readable.
// Returns NULL on failure (e.g. `module_name` is NULL or the Qt thread
// fails to start). Caller must `logos_shim_destroy` to clean up.
LogosShim* logos_shim_new(const char* module_name);

// Synchronously invoke a method on a target module. Blocks for at most
// `timeout_ms` plus a small grace window. Returns a heap-allocated,
// null-terminated JSON string the caller MUST free with
// `logos_shim_free_str`. Always non-NULL on a clean call — on errors,
// returns a JSON object of one of the shapes the agent module already
// emits (`{"error": "..."}` or `{"kind": "error", "message": "..."}`)
// so consumers can use a single parser.
char* logos_shim_call(LogosShim* shim,
                      const char* target_module,
                      const char* method,
                      const char* args_json,
                      int timeout_ms);

// Free a string returned by `logos_shim_call`. Idempotent for NULL.
void logos_shim_free_str(char* s);

// Stop the Qt thread, tear down LogosAPI, free the shim. Idempotent
// for NULL. The handle is invalid after this returns.
void logos_shim_destroy(LogosShim* shim);

#ifdef __cplusplus
}
#endif

#endif  // LOGOS_SHIM_H
