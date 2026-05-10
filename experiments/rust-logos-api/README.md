# Rust ↔ LogosAPI experiment

Goal: validate that a thin client process can attach to a running
`logoscore --mode 0` daemon over Qt Remote Objects and synchronously
invoke a method on a loaded module — specifically, that we can call
`agent.info()` and get JSON back.

If the answer is yes, this becomes the design for **workstream 3
option A** in [issue #19](https://github.com/vpavlin/lmao/issues/19):
the standalone `lmao` CLI is a Remote-mode QtRO consumer of a
separately-running `logoscore` (or of an already-running Basecamp
process). Same IPC pattern Basecamp uses internally; bonus that
`lmao` from a terminal can attach to a live Basecamp and share its
agent instance + mesh node + identity.

The experiment is two phases. Phase A is the quick spike; Phase B is
the Rust scaffold that wraps Phase A's findings in a `bindgen`able
shim. **Phase A first** — there's no point Rustifying anything until
the C++ consumer pattern itself works.

---

## Phase A — pure C++ probe

`probe-cpp/main.cpp` is a single-file binary. It:

1. Sets `LogosMode::Remote` (default, but explicit).
2. Constructs `LogosAPI("agent_info_probe")`.
3. Gets a client for the `agent` module.
4. Calls `invokeRemoteMethod("info", Timeout(10_000))`.
5. Prints the resulting JSON to stdout.

### Build

```bash
cd experiments/rust-logos-api/probe-cpp

cmake -B build -G Ninja \
    -DLOGOS_CPP_SDK_DIR="$WORKSPACE/repos/logos-cpp-sdk"
cmake --build build
```

(Adjust `LOGOS_CPP_SDK_DIR` to wherever `logos-co/logos-cpp-sdk` is
checked out. Qt6 needs to be on `CMAKE_PREFIX_PATH`; in the workspace
that comes from the SDK's flake.)

### Run

In one terminal — start a Remote-mode `logoscore` with the agent
module loaded:

```bash
# Same instance id has to flow to both processes via env.
export LOGOS_INSTANCE_ID=$(uuidgen | tr -d - | head -c12)

logoscore --mode 0 \
    -m ~/.local/share/Logos/LogosBasecamp/modules \
    -l delivery_module,storage_module,accounts_module,agent
```

In another terminal — same `LOGOS_INSTANCE_ID`:

```bash
export LOGOS_INSTANCE_ID=<same as above>

./build/agent_info_probe
```

Expected output (something like):

```
agent.info() → {"name":"…","pubkey":"…","capabilities":[…],…}
{"name":"…","pubkey":"…","capabilities":[…],…}
```

(First line via `qInfo`, second line via `fputs(stdout)` so you can
pipe `./build/agent_info_probe | jq .`.)

### Outcomes — what we learn

| Result | Means | Next step |
|---|---|---|
| Prints valid JSON ✓ | Remote-mode QtRO consumer works from a non-host process. The bindings story for Rust is a mechanical wrap. | Move to Phase B. Open the messaging-migration PR with confidence. |
| `getClient` returns null | QtRO registry not reachable. | Check `LOGOS_INSTANCE_ID` matches in both shells; check logoscore's stderr for "Registry host created at:". |
| `invokeRemoteMethod` returns invalid `QVariant` | Method dispatch failed. | Confirm `agent` module is in the load list logoscore reported. Confirm the method exists in the module's `getMethods()` (it should after PR #15 + the API freeze in #20). |
| Compile fails before we get here | SDK / Qt mismatch on the host. | Surface the specific error; we'd need to revisit the SDK-as-CMake-package story before going further. |

---

## Phase B — Rust shim (TODO, blocked on Phase A)

When Phase A is green, Phase B layers on:

```
experiments/rust-logos-api/
├── README.md             # this file
├── probe-cpp/            # Phase A — done
│   ├── CMakeLists.txt
│   └── main.cpp
└── (TODO Phase B)
    ├── Cargo.toml         # standalone crate, NOT in workspace members
    ├── build.rs           # invokes shim/CMakeLists.txt via the `cmake` crate;
    │                      # `bindgen`s shim/shim.h into Rust.
    ├── src/main.rs        # equivalent of probe-cpp/main.cpp, in Rust
    └── shim/              # C-callable wrapper around LogosAPI
        ├── CMakeLists.txt
        ├── shim.h         # extern "C" interface:
        │                  #   logos_shim_new(module_name) -> *mut Shim
        │                  #   logos_shim_call(s, target, method, args_json) -> *mut c_char
        │                  #   logos_shim_free(s)
        │                  #   logos_shim_free_str(p)
        └── shim.cpp        # owns a QCoreApplication on a dedicated thread,
                            # marshals calls onto it via QMetaObject::invokeMethod,
                            # converts QVariantList ↔ JSON over the boundary
```

The shim is needed because `bindgen` doesn't usefully chew through
`logos_api.h` directly — Qt classes (QObject, QString, QVariantList)
need MOC-generated metadata that lives outside the headers, and QtRO
needs an event loop running on a Qt-owned thread. So the shim:

- runs a `QCoreApplication` on a dedicated thread for the lifetime of
  the process;
- exposes a small C ABI of (init, call, free, destroy);
- does the QVariant↔JSON conversion at the boundary so the Rust side
  speaks JSON strings and never touches a Qt type.

Once this lands, replacing `crates/logos-messaging-a2a-cli/src/daemon/`
with calls to a generic `LogosClient::call("agent", method, args)` is
mechanical.

---

## Status

- [x] Phase A scaffolded — needs someone with Qt6 + the SDK in their
      shell to actually compile + run it.
- [ ] Phase A green-lights
- [ ] Phase B scaffolded
- [ ] Phase B green-lights → open the actual `refactor/cli-as-remote-consumer`
      branch tracked in #19's roll-out
