# LMAO — Logos Module for Agent Orchestration

[![codecov](https://codecov.io/gh/vpavlin/lmao/branch/master/graph/badge.svg)](https://codecov.io/gh/vpavlin/lmao)

> **LMAO** = **L**ogos **M**odule for **A**gent **O**rchestration
>
> Yes, the acronym is intentional. Building decentralized AI agent
> infrastructure is serious work — but it doesn't have to be humourless.
> LMAO implements Google's [A2A protocol](https://github.com/google/A2A)
> over [Logos Messaging](https://logos.co/messaging/) decentralized
> transport, with [Logos Storage (Codex)](https://codex.storage/) as the
> content-addressed audit trail. **Local, decentralized, verifiable AI
> agents.**

## What it is

LMAO is the **coordination layer** for a fleet of AI agents. It does not
do inference itself. It does:

- **Identity** — each agent has a secp256k1 keypair. No DNS, no central
  registry. Other agents reach you by your pubkey.
- **Transport** — embedded Logos Messaging node (via `liblogosdelivery`).
  Pub/sub gossip on the `logos.dev` fleet. No nwaku container, no REST
  endpoint, no port forwarding.
- **Discovery** — agents announce themselves with capabilities; peers
  build a live `PeerMap` from signed presence broadcasts.
- **Task routing** — `LmaoNode::send_task` / `delegate_task` route by
  pubkey, capability, or strategy (first-available, capability-match,
  round-robin, broadcast-collect).
- **Audit trail** — each task's full execution log is uploaded to
  embedded Logos Storage; the response carries a `codex://<cid>` you can
  fetch later to verify what the agent actually did.
- **Daemon mode** — `lmao agent run` is a long-running process; other
  CLI commands talk to it over a Unix socket instead of dialing the
  gossip mesh from cold every time.

The agent itself is your business logic. Plug in [Goose](https://goose-docs.ai),
[Codex CLI](https://github.com/openai/codex), or any program that reads
stdin and writes stdout. Configure it to point at your local Ollama,
llama.cpp server, vLLM, or LM Studio — anything OpenAI-API-compatible —
and you have a real, local, decentralized AI agent network.

| | HTTP/SSE A2A | LMAO |
|---|---|---|
| Identity | DNS + TLS cert | secp256k1 pubkey |
| Discovery | Central registry | Content-addressed gossip |
| Endpoints | Stable IP required | Just a pubkey |
| Inference | Cloud API | Local OpenAI-compatible (Ollama / llama.cpp / vLLM) |
| Audit | Server logs you can't see | Content-addressed CID anyone can verify |
| Privacy | Traffic analysis easy | Optional E2E encryption |
| Censorship | Single point of failure | Decentralized relay |
| NAT | Needs port forwarding | Works behind NAT |

## Architecture

```
┌──────────────────────────────────────────────────────────────────┐
│                      Logos Messaging Network                      │
│                                                                   │
│  /lmao/1/discovery/proto         AgentCard broadcasts            │
│  /lmao/1/presence/proto          Signed peer announcements       │
│  /lmao/1/task-{pubkey}/proto     Per-agent task inbox            │
│  /lmao/1/stream-{task_id}/proto  Streaming chunks                │
│  /lmao/1/ack-{msg_id}/proto      SDS acknowledgements            │
└────────┬───────────────────────┬──────────────────────────────┬──┘
         │                       │                              │
   ┌─────▼─────┐           ┌─────▼─────┐                  ┌─────▼─────┐
   │  alice    │           │   bob     │                  │   ...     │
   │  agent    │◀── IPC ──▶│  agent    │                  │           │
   │           │           │           │                  │           │
   │  ┌──────┐ │           │  ┌──────┐ │                  │           │
   │  │ exec │ │           │  │ exec │ │  --exec '...'    │           │
   │  │ Goose│ │           │  │ Goose│ │  per-task        │           │
   │  └──┬───┘ │           │  └──┬───┘ │  executor        │           │
   │     │     │           │     │     │                  │           │
   └─────┼─────┘           └─────┼─────┘                  └───────────┘
         │                       │
   ┌─────▼─────┐           ┌─────▼─────┐
   │  Ollama   │           │  Ollama   │     local OpenAI-compatible
   │  (or any  │           │  (or any  │     inference, per-agent
   │  OAI-API) │           │  OAI-API) │
   └───────────┘           └───────────┘

   ┌──────────────┐        ┌──────────────┐
   │ libstorage   │        │ libstorage   │     embedded Codex node
   │ (embedded)   │        │ (embedded)   │     per agent — exec
   └──────────────┘        └──────────────┘     logs are uploaded,
                                                CIDs returned in
                                                task responses
```

Everything in dashed boxes is one Rust binary linking two FFIs
(`liblogosdelivery` for messaging, `libstorage` for Codex). No Docker,
no nwaku container, no REST endpoint, no inference server inside LMAO.

## Quick Start

> Goal: from a fresh checkout, `make demo` runs two agents on the real
> `logos.dev` fleet, routes a task by capability, returns a content-
> addressed audit log. ~45 seconds.

### Prerequisites

- Linux x86_64 (macOS should work; not yet smoke-tested)
- Rust stable (`rustup`)
- Nim 2.x — `curl https://nim-lang.org/choosenim/init.sh -sSf | sh`
- `build-essential libclang-dev libssl-dev` (Debian / Ubuntu)
- Optional: a local OpenAI-compatible inference endpoint (Ollama,
  llama.cpp, vLLM, LM Studio). The default demo uses tiny `sed`-based
  stub executors so you can prove the plumbing without a model.

### 1. Build `liblogosdelivery`

```bash
git clone https://github.com/logos-messaging/logos-delivery
cd logos-delivery
make liblogosdelivery
export LIBLOGOSDELIVERY_LIB_DIR="$PWD/build"
```

This produces `liblogosdelivery.so`. ~5 minutes on first build (Nim
compiles a lot of dependencies).

### 2. Build the LMAO CLI

```bash
git clone https://github.com/vpavlin/lmao
cd lmao
make cli-logos-delivery       # release build with --features logos-delivery,libstorage
```

The `libstorage` feature uses pre-built static blobs from
[`storage-bindings`](https://crates.io/crates/storage-bindings) — no Nim
build for storage, just downloaded once on first compile.

### 3. Run the demo

```bash
make demo
```

You should see five steps complete in ~45 s:

```
[1/5] starting two agents (persistent identities + embedded storage + IPC sockets)…
[2/5] waiting for each to connect to logos.dev and announce…
        alice pubkey: 0226e882fbb0efd6…
        bob   pubkey: 02b6a180a38e5dfe…
[3/5] discovering peers via presence (through alice's daemon — no new node)…
        Source: daemon
        Found 2 live peer(s):
          alice (text, summarize)  …
          bob   (code, review)     …
[4/5] delegating a task by capability=code → bob (via alice's daemon)…
        [OK] agent=02b6a180a38e subtask=…
          Result: [reviewed]   Review this snippet: fn main() { println!("hello"); }
          ---
          execution log: codex://zDvZRwzkxyaFVWegnFCg6dva4qsADNnZevXTjFkTxhXYLkwzMmnW
[5/5] fetching bob's execution log by CID via bob's daemon…
        reviewer-stderr-line
```

That's the full story: announce → discover → delegate by capability →
respond → audit-fetch.

### 4. (Optional) Run the same demo in containers

For the security story (Goose runs LLM-suggested shell commands; you
probably don't want them with full access to your home dir):

```bash
make demo-containerized
```

Each agent runs as non-root inside its own debian-trixie-slim
container with no host filesystem access except a scoped data volume.
Goose-suggested file edits and shell commands are bounded by the
container — your `~/.ssh`, `/etc`, etc. are unreachable. Same fleet,
same logos.dev mesh, same five-step narrative; the host CLI drives
each container's daemon over a Unix-socket volume mount.

First run builds the image (~15-20 min: Nim + Rust + Goose); cached
runs finish in ~1 min. See `Dockerfile` + `docker-compose.yml`.

### 4b. (Optional) Run inside Logos Basecamp

LMAO ships a Basecamp module pair (`basecamp/agent-module` + `basecamp/agent-ui`)
so you can drive an agent from inside the Logos desktop app.

The `agent_ui` tab gives you four panes — daemon status, peers,
delegate-by-capability, content-addressed audit-log fetch — all routed
through `logos.callModule("agent", …)` into a C++ module that spawns
`lmao agent run` as a subprocess and proxies its IPC.

**Option A — portable .lgx for the prebuilt Basecamp** (recommended;
works with the AppImage / DMG from the Basecamp release page):

```bash
make basecamp-lgx
# → dist/agent.lgx + dist/agent_ui.lgx — fully self-contained,
# zero /nix/store references, ~170 KB combined.
```

Drop both `.lgx` files into Basecamp's Package Manager UI (drag-and-drop
or "Import"), or:

```bash
make basecamp-lgx-install LMAO_BASECAMP_MODULES=<path-to-modules-dir>
```

**Option B — local dev install for a Nix-built Basecamp** (faster
iteration):

```bash
make basecamp-install
# → ~/.local/share/Logos/LogosBasecampDev/modules
```

Either way, the shell that launches Basecamp must have these env vars
exported so the module's spawned `lmao agent run` subprocess can find
its native lib + binary:

```bash
export LIBLOGOSDELIVERY_LIB_DIR=/path/to/logos-delivery/build
export LD_LIBRARY_PATH="$LIBLOGOSDELIVERY_LIB_DIR:$LD_LIBRARY_PATH"
export LMAO_BIN=$(realpath target/release/logos-messaging-a2a)
```

### 5. Plug in a real coding agent

The demo defaults to `sed` so it works without a model. Swap in
[Goose](https://goose-docs.ai) + Ollama:

```bash
ollama serve &                       # in another terminal
ollama pull qwen2.5-coder:7b

curl -fsSL https://github.com/aaif-goose/goose/releases/download/stable/download_cli.sh | bash
cat > ~/.config/goose/config.yaml <<'YAML'
GOOSE_PROVIDER: ollama
GOOSE_MODEL: qwen2.5-coder:7b
OLLAMA_HOST: localhost
extensions: {}
YAML

# Real agent recipes for the demo
export LMAO_DEMO_BOB_EXEC='goose run --no-session -i - --output-format text --quiet'
export LMAO_DEMO_ALICE_EXEC='goose run --no-session -i - --output-format text --quiet'
make demo
```

bob now actually reads the snippet, runs Goose's tool-use loop against
`qwen2.5-coder:7b`, and produces a real review. The CID in the
response retrieves the full Goose conversation transcript.

## How a human uses LMAO

Two surfaces, depending on what you're doing:

**Operator** — you have agents you want to run.

```bash
lmao --keyfile alice.key --tcp-port 60010 --udp-port 9010 \
     --storage libstorage --storage-data-dir ./alice-storage \
     --daemon-socket ./alice.sock \
     agent run --name alice --capabilities text,summarize \
               --exec 'goose run --no-session -i - --output-format text --quiet'
```

**Client** — you want to query / delegate against a fleet you (or
someone else) is running.

```bash
# All of these talk to the daemon if available, else spin up an
# ephemeral node. Use --daemon-socket to pick a specific daemon.
lmao --daemon-socket ./alice.sock daemon status
lmao --daemon-socket ./alice.sock presence peers
lmao --daemon-socket ./alice.sock task delegate --capability code --text "..."
lmao --daemon-socket ./alice.sock task send --to <pubkey> --text "..."
lmao --daemon-socket ./alice.sock storage fetch <cid>
lmao --daemon-socket ./alice.sock daemon stop
```

The "daemon mode" matters because each fresh `lmao` invocation otherwise
spins up its own embedded Logos Messaging node (5–20 s of mesh-join).
With the daemon, every CLI call is a sub-millisecond Unix-socket
round-trip against an already-connected node.

## CLI reference

```bash
lmao [OPTIONS] <COMMAND>
```

### Global options

| Flag | Description |
|---|---|
| `--transport <kind>` | `logos-delivery` (default) or `rest` |
| `--preset <name>` | Logos Messaging preset (default `logos.dev`) |
| `--tcp-port <u16>` | libp2p TCP port (0 = OS-assigned) |
| `--udp-port <u16>` | discv5 UDP port (0 = OS-assigned) |
| `--storage <kind>` | `none` (default) or `libstorage` |
| `--storage-data-dir <path>` | Persistent state for the embedded Codex node |
| `--storage-port <u16>` | discovery port for libstorage |
| `--daemon-socket <path>` | Unix socket of a running daemon (default `$XDG_RUNTIME_DIR/lmao.sock`) |
| `--keyfile <path>` | Persistent identity (created if missing) |
| `--encrypt` | X25519 + ChaCha20-Poly1305 session encryption |
| `--waku <url>` | nwaku REST URL (only for `--transport rest`) |
| `--json` | Structured JSON output |

### Commands

| Command | Description |
|---|---|
| `agent run --name N --capabilities C [--exec '<cmd>']` | Run an agent. Daemon. Default exec is the Goose recipe. |
| `agent discover` | One-shot discovery via the discovery topic |
| `agent bundle` | Print this agent's IntroBundle (out-of-band key exchange) |
| `task send --to <pk> --text '<msg>'` | Send a task |
| `task status --id <uuid>` | Poll for results |
| `task stream --id <uuid> [--timeout <s>]` | Follow streaming chunks |
| `task delegate [--to <pk>] [--capability <c>] [--strategy s] --text '<msg>'` | Delegate a subtask |
| `presence announce --name N [--ttl s] [--repeat]` | One-shot presence ping |
| `presence discover [--capability c]` | Listen for raw presence broadcasts |
| `presence peers [--capability c] [--watch]` | Show the daemon's PeerMap |
| `storage fetch <cid> [-o file]` | Retrieve bytes by CID via the daemon |
| `daemon status` / `daemon stop` | Manage a running daemon |
| `health` | Probe the configured nwaku REST endpoint (legacy) |
| `metrics` | Show operational metrics counters |
| `info` | Identity + topic config (uses the daemon when available) |
| `completion <shell>` | Shell completions (bash / zsh / fish / …) |

## Configuration recipes

### Goose against Ollama

```yaml
# ~/.config/goose/config.yaml
GOOSE_PROVIDER: ollama
GOOSE_MODEL: qwen2.5-coder:7b
OLLAMA_HOST: localhost
extensions: {}
```

```bash
lmao agent run --name coder --capabilities code,review \
    --exec 'goose run --no-session -i - --output-format text --quiet'
```

### Codex CLI against a local OpenAI-compatible endpoint

```toml
# ~/.codex/config.toml
[model_providers.local]
name = "local"
base_url = "http://localhost:11434/v1"   # Ollama's OpenAI compat endpoint
wire_api = "chat"
```

```bash
lmao agent run --name coder --capabilities code \
    --exec 'codex exec --provider local --model qwen2.5-coder:7b'
```

### Pure Unix stub (no LLM, useful for plumbing tests)

```bash
lmao agent run --name echoer --capabilities echo \
    --exec "sh -c 'echo audit-line >&2; sed s/^/[done]\ /'"
```

stderr → uploaded to libstorage as audit log; stdout → response.

## Architecture deep dive

### Encryption

Each agent optionally generates an X25519 identity (`--encrypt`). Other
agents discover its `IntroBundle` via the AgentCard. The sender derives
a session key via ECDH and ships the task as `A2AEnvelope::EncryptedTask`
with ChaCha20-Poly1305. Receiver decrypts transparently.

**Note:** static ECDH; no forward secrecy. Migration to the Logos Chat
SDK Double Ratchet is on the roadmap.

### Storage offload + audit

Two distinct uses of Logos Storage:

1. **Per-task audit log** (default in `--storage libstorage`): each
   `--exec` invocation's stderr is uploaded to embedded Codex; the CID
   appears in the response.
2. **Large payload offload** (`StorageOffloadConfig`, library-level):
   payloads above a threshold are uploaded and the message carries only
   the CID; the receiver pulls bytes by CID transparently.

### Presence + PeerMap

```
┌─────────┐     /lmao/1/presence/proto    ┌─────────┐
│ alice   │──────signed announcement─────▶│ bob     │
│  ◀──────────────────────────────────────│         │
└──┬──────┘                                └────┬────┘
   │                                            │
   ▼                                            ▼
   PeerMap (TTL evict, capability-indexed)
   Updated by `node.poll_presence()` in the inbox loop.
```

Announcements are signed (secp256k1 over canonical JSON, signature field
excluded). Verifiers reject tampered or spoofed messages.

### Delegation

```
                  ┌─────────────────┐
                  │  orchestrator   │
                  └────────┬────────┘
                           │ DelegationRequest
                           │ { CapabilityMatch, BroadcastCollect,
                           │   FirstAvailable, RoundRobin }
                           ▼
                  ┌──────────────────┐
                  │     PeerMap      │ ← populated by presence
                  └────────┬─────────┘
                           │
              ┌────────────┼────────────┐
              ▼            ▼            ▼
        ┌─────────┐  ┌─────────┐  ┌─────────┐
        │ peer A  │  │ peer B  │  │ peer C  │ ← matching capability
        └─────────┘  └─────────┘  └─────────┘
```

### Streaming

`respond_stream` publishes `TaskStreamChunk`s on a per-task topic
(`/lmao/1/stream-{task_id}/proto`) with incrementing indices and a
final-chunk flag. Consumers buffer and reassemble.

### SDS reliability

Send path goes through an SDS layer:
- Bloom-filter dedup
- Lamport-clock causal ordering
- Configurable ACK timeout + retry (`with_config(ChannelConfig)`)

## Crates

| Crate | Description |
|---|---|
| `logos-messaging-a2a-crypto` | X25519 ECDH + ChaCha20-Poly1305 |
| `logos-messaging-a2a-core` | A2A types — `AgentCard`, `Task`, `Message`, `Part`, presence, delegation |
| `logos-messaging-a2a-transport` | `Transport` trait + `LogosDeliveryTransport` (liblogosdelivery FFI) + `LogosCoreDeliveryTransport` (Logos Core IPC) + `LogosMessagingTransport` (REST, fallback) + `InMemoryTransport` + SDS |
| `logos-messaging-a2a-storage` | `StorageBackend` trait + `LibstorageBackend` (libstorage FFI) + REST + Logos Core IPC |
| `logos-messaging-a2a-execution` | `ExecutionBackend` trait + Status Network (EVM) + LEZ stub |
| `logos-messaging-a2a-node` | `LmaoNode` — main agent node — announce, discover, send, respond, presence, delegation, streaming, payments |
| `logos-messaging-a2a-cli` | The `lmao` binary |
| `logos-messaging-a2a-mcp` | MCP bridge — expose agents as tools to Claude / Cursor |
| `logos-messaging-a2a-ffi` | C FFI for Logos Core / Qt module integration |
| `lmao-ffi` | Higher-level C FFI wrapper |

## Examples

The `examples/` directory has runnable end-to-end demos that don't
require the CLI binary:

| Example | What it shows |
|---|---|
| `cargo run --features logos-delivery --example logos_delivery_two_agents` | Full A2A loop on real logos.dev: announce → discover → send → respond |
| `cargo run --features logos-delivery --example logos_delivery_streaming` | 8 streaming chunks, in-order reassembly |
| `cargo run --features logos-delivery --example logos_delivery_delegation` | 3 nodes, capability-routed delegation |
| `cargo run --features logos-delivery --example logos_delivery_encrypted` | X25519+ChaCha20 encrypted task + response |
| `cargo run --example two_agents` | In-memory pipeline + payment flow (no network deps) |
| `cargo run --example ping_pong [-- --encrypt]` | Minimal in-memory roundtrip |

## Testing

`InMemoryTransport` lets every test run without a network. ~970
unit/integration tests across the workspace; `cargo test --workspace`
takes ~45 s on a recent machine.

```bash
cargo test --workspace                                     # default features
cargo test --workspace --features logos-delivery,libstorage   # FFI paths included
```

## Status

This branch (`feat/logos-delivery-transport`) is the active
ETHPrague-prep line. Things that work today:

- `liblogosdelivery` + `libstorage` FFIs co-exist in one binary
- Real-network E2E for announce / discover / send / respond / presence
  / delegation / streaming / encryption
- Daemon mode (`agent run` + IPC) for `info`, `task`, `presence`,
  `storage fetch`
- `--exec` integration so any stdin/stdout-shaped CLI agent (Goose,
  Codex, gptme, plain shell scripts) becomes the worker

Things that are stale:
- The MCP bridge still uses the old transport-construction pattern;
  it works against `--transport rest` but hasn't been re-wired for
  `logos-delivery`.
- LEZ `ExecutionBackend` is `unimplemented!()` — only Status Network
  works for x402 payments.
- The Logos Core / Qt plugin (`module/`) is scaffolding only.

Bus-factor flag: the `storage-bindings` crate and its prebuilt native
binaries live under personal namespace `nipsysdev/*` (Xav). Worth
mirroring before depending on it for production.

## Roadmap

- [x] A2A core types + SDS reliability
- [x] X25519 + ChaCha20 encryption
- [x] In-memory transport for testing
- [x] nwaku REST + Logos Core IPC transports
- [x] `LogosDeliveryTransport` — embedded Logos Messaging via liblogosdelivery
- [x] `LibstorageBackend` — embedded Logos Storage via libstorage
- [x] CID-based audit log per task (--exec stderr → libstorage → CID in response)
- [x] CLI daemon mode (Unix socket IPC) for info / task / presence / storage
- [x] `--exec` flag — any OpenAI-compatible coding agent (Goose, Codex, …)
- [x] Presence broadcasts + PeerMap with capability indexing
- [x] Multi-agent delegation (FirstAvailable / CapabilityMatch / RoundRobin / BroadcastCollect)
- [x] Task streaming
- [x] x402 payment flow (Status Network EVM backend)
- [ ] Containerised demo — Dockerfile + docker-compose for the agent fleet
- [ ] MCP bridge migrated onto the daemon path
- [ ] Logos Chat SDK — Double Ratchet for forward secrecy
- [ ] LEZ agent registry — on-chain AgentCards via SPELbook
- [ ] Logos Core `.lgx` plugin — agent fleet UI in the Logos desktop

## Part of the SPEL Ecosystem

| Repo | Description |
|---|---|
| [spel](https://github.com/jimmy-claw/spel) | Smart Program Execution Layer — LEZ framework |
| [spelbook](https://github.com/jimmy-claw/spelbook) | On-chain program registry |
| [lez-multisig-framework](https://github.com/jimmy-claw/lez-multisig-framework) | Multisig governance |
| [lmao](https://github.com/vpavlin/lmao) | This repo — A2A agent orchestration |

## License

MIT.
