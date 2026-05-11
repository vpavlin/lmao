# logos-core-bindings

Rust bindings to the Logos C++ SDK's `LogosAPI`, via a C-callable shim.
The shim owns a Qt event-loop thread and a `LogosAPI` instance; calls
from any Rust thread are dispatched onto it via QtRO. JSON in / JSON
out across the FFI — the Rust crate never sees a Qt type.

This is the foundation crate for the refactor tracked in
[issue #19](https://github.com/vpavlin/lmao/issues/19). Storage,
messaging, and runtime migrations layer on top.

## Two build modes

The crate compiles in either of two modes, picked by `build.rs` based
on the `LOGOS_CPP_SDK_DIR` env var:

| Mode    | Trigger                                  | What works                                |
|---------|------------------------------------------|-------------------------------------------|
| Real    | `LOGOS_CPP_SDK_DIR` points at a checkout | Full `Shim::new` / `Shim::call`           |
| Stub    | env var unset                            | Crate compiles; `Shim::new` returns `Error::NotCompiledIn` |

`cargo build --workspace` on a CI host without Qt6 / Boost / OpenSSL /
the SDK installed picks the stub mode and the workspace stays green.
Set `LOGOS_CPP_SDK_DIR` to enable the real build.

## Real build prerequisites

- Qt6 — `Core`, `Network`, `RemoteObjects`
- Boost.System
- OpenSSL
- nlohmann_json (header-only)
- cmake + ninja (or make)
- bindgen + libclang

The shim is consumed via `cmake-rs` from `build.rs`. Build dir layout
mirrors the experiment under
`experiments/rust-logos-api/rust-shim/` — see that README for run
procedure + gotchas.

## Smoke check

```bash
# Stub build (CI default):
cargo check -p logos-core-bindings

# Real build:
LOGOS_CPP_SDK_DIR=/path/to/logos-cpp-sdk cargo build -p logos-core-bindings
```

## Usage

```rust
use logos_core_bindings::Shim;

let shim = Shim::new("my_consumer")?;          // boots Qt thread + LogosAPI
let json = shim.call(
    "agent",        // target module name
    "info",         // method
    "[]",           // args (JSON array)
    5_000,          // timeout_ms
)?;
println!("{json}");
```

`Shim` is `Send + Sync` — share via `Arc<Mutex<Shim>>` if multiple
callers need it. Each `call` is synchronous from the Rust side; the
Qt thread serialises the underlying `invokeRemoteMethod` invocations.
Wrap calls in `tokio::task::spawn_blocking` if you're in an async
runtime.

## Stop here, see also

- [`experiments/rust-logos-api/`](../../experiments/rust-logos-api/)
  — the original scaffold, including a working `lmao-observatory` TUI
  that polls the shim. Move that to its own crate once the refactor
  graduates from `experiments/`.
- [issue #19](https://github.com/vpavlin/lmao/issues/19) — the full
  roll-out plan this crate is the first step of.
