// Bindgen entry point — only the shim's narrow C ABI is in scope.
// Qt / SDK headers are explicitly excluded from this surface; the
// shim is exactly the boundary that hides them from Rust.
#include "shim.h"
