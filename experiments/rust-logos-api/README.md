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

## Status: Phase A is green

End-to-end success on linux-x86_64, 2026-05-10:

```json
{
  "kind": "info",
  "name": "basecamp",
  "pubkey": "036174d7bda3afefbecef527548c00da49773543c350fc231d192d8fea6c5f40d7",
  "capabilities": ["text"],
  "uptime_secs": 38,
  "storage_enabled": true,
  "encryption_pubkey": "cd9de6e0d7bba572d050342562c3061be97e77ea3e7d912eff3dd2a29b100f1b",
  "load": {"bucket": "free", "queue_depth": 0, "max_concurrent": 1, "avg_latency_ms": 0}
}
```

The probe ran in a non-host process, talked to a separately-launched
`logoscore --mode 0`, dispatched through QtRO into the agent module's
`info()` Q_INVOKABLE, got JSON back. Bindings story for Phase B is
unblocked.

The exact reproduction (modules-dir setup + the manifest.json gotcha
for `capability_module`) is in the Run section below.

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
nix build
```

`flake.nix` pulls Qt6 via `logos-nix` (so we follow the workspace's
nixpkgs / Qt pin — drift onto a different Qt and QtRO ABI mismatches
look like flaky "registry host not reachable" runtime failures) and
the SDK source via `logos-cpp-sdk`. Output lands at
`./result/bin/agent_info_probe`.

Iterating on `main.cpp` without paying the nix-derivation rebuild on
each tweak:

```bash
nix develop
cmake -B build -G Ninja      # LOGOS_CPP_SDK_DIR is preset by the shellHook
cmake --build build
./build/agent_info_probe     # ditto LOGOS_INSTANCE_ID env required, see Run
```

If you genuinely don't have nix, the bare CMake invocation is
`cmake -B build -G Ninja -DLOGOS_CPP_SDK_DIR=$WORKSPACE/repos/logos-cpp-sdk`
with Qt6 on `CMAKE_PREFIX_PATH` — but you'll have to source those
yourself, which is exactly the friction the flake exists to remove.

### Run

The exact recipe that gave us the success above. Today's agent module
declares `dependencies: []` in its manifest.json (it owns its own
mesh node + storage internally), so loading it only needs
`capability_module` for the SDK token handshake.

**1. Stage a modules dir** with `agent` + `capability_module`. The
plugin manager scans subdirectories named after the module, each with
a `manifest.json` and the plugin `.so` (older `metadata.json`-only
modules are silently skipped — easy way to lose half an hour, ask me
how I know).

```bash
MODS=$(mktemp -d -t lmao-probe-mods.XXXX)

# Agent — already laid out as a manifest.json layout in the Basecamp
# install dir, so just symlink the whole thing.
ln -s ~/.local/share/Logos/LogosBasecamp/modules/agent          "$MODS/agent"

# Capability module — needs a synthesized manifest.json. The .so can
# come from anywhere; pick a build that's around.
mkdir -p "$MODS/capability_module"
ln -s <path-to>/capability_module_plugin.so \
      "$MODS/capability_module/capability_module_plugin.so"
cat > "$MODS/capability_module/manifest.json" <<'EOF'
{
  "author": "Logos Core Team",
  "category": "security",
  "dependencies": [],
  "description": "Coordinates permissions between modules",
  "main": { "linux-amd64": "capability_module_plugin.so" },
  "manifestVersion": "0.2.0",
  "name": "capability_module",
  "type": "core",
  "version": "1.0.0"
}
EOF
```

**2. Start `logoscore` in Remote mode**:

```bash
export LOGOS_INSTANCE_ID=$(uuidgen | tr -d - | head -c12)

logoscore --mode 0 -m "$MODS" -l capability_module,agent \
    > /tmp/logoscore.log 2>&1 &
```

…and **wait ~45 s** for it to come up. The agent module's bundled
lmao subprocess does a cold dial-out to the public `logos.dev` Waku
fleet, which takes 20–40 s before the agent's `info()` reports
`storage_enabled: true` and a real pubkey. (Calling `info()` earlier
just gets `{"error": "daemon not running"}` from the agent's own
validation — the IPC works fine; the *agent state* is just not
populated yet.)

**3. Run the probe** in the same shell (so it inherits
`LOGOS_INSTANCE_ID`):

```bash
./result/bin/agent_info_probe
```

Two output streams, both printing the same JSON:

- stderr (via `qInfo`):  `agent.info() → {"kind":"info","name":"…",…}`
- stdout (via `fputs`):  `{"kind":"info","name":"…",…}`

So `./result/bin/agent_info_probe | jq .` gives you a clean parsed
view.

### Outcomes — what we learn

| Result | Means | Next step |
|---|---|---|
| Prints valid JSON ✓ | Remote-mode QtRO consumer works from a non-host process. The bindings story for Rust is a mechanical wrap. | Move to Phase B. Open the messaging-migration PR with confidence. |
| `getClient` returns null | QtRO registry not reachable. | Check `LOGOS_INSTANCE_ID` matches in both shells; check logoscore's stderr for "Registry host created at:". |
| `invokeRemoteMethod` returns invalid `QVariant` | Method dispatch failed. | Confirm `agent` module is in the load list logoscore reported. Confirm the method exists in the module's `getMethods()` (it should after PR #15 + the API freeze in #20). |
| **Hangs past the `Timeout(10_000)` arg** | This is what happens when no logoscore is running at all. QtRO opens the `QLocalSocket` to the registry name, gets `ServerNotFoundError`, starts a reconnect timer, and the SDK's pre-call token handshake (`capability_module.requestModule("agent")`) blocks waiting for the registry — *the `Timeout` arg is the per-call deadline, not a connect-deadline*. Phase B's Rust shim will need an outer caller-side timeout on top, or a separate "is the registry reachable" probe before issuing real calls. | Flagged. Document in the shim. |
| Compile fails before we get here | SDK / Qt mismatch on the host. | Surface the specific error; we'd need to revisit the SDK-as-CMake-package story before going further. |

### What the spike has confirmed so far

- `nix build` against `logos-cpp-sdk` + the workspace Qt pin works on linux-x86_64. ~3 min cold (compiles the whole SDK static lib), seconds incremental.
- The right way to consume the SDK is `add_subdirectory(${LOGOS_CPP_SDK_DIR}/cpp)` — its CMakeLists already exports a `logos_sdk` STATIC target with Qt + Boost + OpenSSL + nlohmann_json wired up. Cherry-picking individual `.cpp`s drifts on every SDK update.
- `LogosAPI` ctor + `getClient(...)` are non-blocking; the actual connect-and-handshake happens inside `invokeRemoteMethod` and isn't gated by the `Timeout` arg's value when there's no registry at all.

### What's still open

- The success path. The probe builds and runs; what it does against a *real* logoscore + agent module is the actually-interesting half. Easiest reproduction: run it from a Basecamp checkout where all four modules are already loaded (`-m ~/.local/share/Logos/LogosBasecamp/modules`). Until that's been demonstrated, we know the bindings + build story works but not the dispatch path.

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

## Phase B — Rust shim ✓ green

`rust-shim/` ships the same `agent.info()` round-trip but driven from
Rust:

- `src/main.rs` — Rust binary equivalent of `probe-cpp/main.cpp`.
- `src/lib.rs` — safe `Shim` wrapper over the unsafe FFI.
- `shim/shim.{h,cpp}` — C-callable layer over `LogosAPI`. Owns a Qt
  event-loop thread; serialises calls onto it via
  `QMetaObject::invokeMethod(Qt::QueuedConnection)`. JSON in, JSON
  out — Rust never sees a Qt type.
- `build.rs` — drives `cmake-rs` to build the shim + the SDK's
  `logos_sdk` static target, then `bindgen` over `shim.h`.
- `flake.nix` — wraps cargo + cmake + Qt6 + Boost / OpenSSL /
  nlohmann_json + bindgen's libclang. `nix build` produces
  `result/bin/agent_info_probe_rs`.

Verified end-to-end on linux-x86_64, 2026-05-10. With logoscore +
agent + capability_module loaded, `result/bin/agent_info_probe_rs`
prints:

```json
{
  "kind": "info",
  "name": "basecamp",
  "pubkey": "036174d7bda3afefbecef527548c00da49773543c350fc231d192d8fea6c5f40d7",
  "capabilities": ["text"],
  "encryption_pubkey": "cd9de6e0d7bba572d050342562c3061be97e77ea3e7d912eff3dd2a29b100f1b",
  "storage_enabled": true,
  "load": {"bucket":"free","queue_depth":0,"max_concurrent":1,"avg_latency_ms":0},
  ...
}
```

Two gotchas captured along the way:

- `buildRustPackage`'s default cmake / ninja setupHooks step on
  cmake-rs's own pipeline. Set
  `dontUseCmakeConfigure = true`, `dontUseCmakeBuild = true`,
  `dontUseNinjaBuild = true`, `dontUseNinjaInstall = true` so the
  Rust build owns the build pipeline.
- The shim's CMakeLists must `install(TARGETS logos_sdk ARCHIVE
  DESTINATION lib)` alongside `logos_shim` — cmake-rs only exposes
  the install prefix to `build.rs`, not the build dir. Without an
  install rule for the SDK target, rustc's linker can't find
  `liblogos_sdk.a`.

What the shim doesn't do yet: event subscriptions. `LogosAPI` exposes
`onModuleEvent(...)` for receiving `delegate_complete` /
`fetch_cid_complete` / etc. events; the C-callable shim is sync-only
for now. Adding a `logos_shim_subscribe(name, callback)` that pushes
events into a Rust mpsc channel is the obvious next extension —
needed for the observatory TUI to surface live task / presence
events without polling.

## Status

- [x] Phase A scaffolded
- [x] Phase A green-lights — verified with real `agent.info()` JSON over QtRO, see top of this README
- [x] Phase B scaffolded
- [x] Phase B green-lights — same JSON, but driven from Rust
- [ ] Event subscription extension to the shim (push events into a Rust channel)
- [ ] Bring it into the actual `refactor/cli-as-remote-consumer`
      branch tracked in #19's roll-out
