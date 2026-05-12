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
- **Transport** — Logos Messaging pub/sub gossip on the `logos.dev`
  fleet. Inside Logos Basecamp, shares the host's `delivery_module` and
  `storage_module` (logos-core-native, no duplicate Waku/Codex nodes).
  Standalone, uses an embedded node via `liblogosdelivery`. No REST
  endpoint, no port forwarding.
- **Discovery + presence** — agents broadcast a full `AgentCard` once
  (discovery) and a recurring TTL-bounded liveness beacon (presence).
  Peers build a live `PeerMap` from signed presence; A2A's HTTP variant
  has no equivalent heartbeat.
- **Sealed presence** — load-status (`free` / `busy` / `full`) rides on
  presence beacons as per-trusted-peer X25519+ChaCha20-Poly1305 envelopes.
  Trusted peers see your real-time capacity for load-aware routing;
  strangers see only that you exist.
- **Friend-keyring trust list** — runtime-mutable per-pubkey trust with
  `Off` / `Log` / `Enforce` modes and per-capability scoping. In Enforce
  mode, only listed peers can deliver tasks or be delegated to. See
  [docs/TRUST.md](docs/TRUST.md).
- **Task routing** — `LmaoNode::send_task` / `delegate_task` / `delegate_direct`
  route by pubkey, capability, or strategy (first-available,
  capability-match, round-robin, broadcast-collect).
- **Audit trail** — each task's full execution log is uploaded to
  embedded Logos Storage; the response carries a `codex://<cid>` you can
  fetch later to verify what the agent actually did.
- **Daemon mode** — `lmao agent run` is a long-running process; other
  CLI commands talk to it over a Unix socket instead of dialing the
  gossip mesh from cold every time.
- **Basecamp UI plugin** — drop the published LGX into the official
  Basecamp build and you get a four-tab UI (Identity / Trust / Tasks /
  Peers) driving the same daemon, no terminal needed. See `basecamp/`.

The agent itself is your business logic. Plug in [pi](https://www.npmjs.com/package/@mariozechner/pi-coding-agent)
(bundled in the published image as `pi-exec`), [Goose](https://goose-docs.ai),
[Codex CLI](https://github.com/openai/codex), or any program that reads
stdin and writes stdout. Point it at OpenAI / Anthropic / Venice / a local
Ollama / lemonade-server / vLLM / LM Studio — anything OpenAI-API-compatible
— and you have a real, local, decentralized AI agent network.

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

Three on-ramps, in order of laziness:

1. **Drop the latest LGX into your Basecamp** — go to the [v0.1.0 release](https://github.com/vpavlin/lmao/releases/tag/v0.1.0),
   download `logos-agent-module-lib-fat.lgx` + `logos-agent_ui-module.lgx`,
   install via Basecamp's Package Manager, restart. Done — you have a
   live agent with a UI, no toolchain installed. (See [docs/RUNNING_REMOTE.md](docs/RUNNING_REMOTE.md)
   if your Basecamp install rejects the LGX with a "signature error" —
   the manual flat-extract path is documented.)
2. **Run a headless agent in a container** — the published image
   `ghcr.io/vpavlin/lmao:dev` ships the `lmao` binary, `liblogosdelivery.so`,
   `pi-coding-agent`, and Goose preinstalled. `docker pull` + a small
   compose YAML and you have an agent on the public mesh. Step-by-step:
   [docs/RUNNING_REMOTE.md](docs/RUNNING_REMOTE.md), including the pi
   model-config file format and `pi` extension support (e.g. the
   `max-turns.ts` cap-extension under [demo-config/pi/agent/extensions/](demo-config/pi/agent/extensions/)).
3. **Build from source** — keep reading. Two agents on the real
   `logos.dev` fleet, routes a task by capability, returns a
   content-addressed audit log, ~45 seconds end-to-end.

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

Either way, export the path to a shim-enabled `lmao` binary before
launching Basecamp:

```bash
# Build lmao with shim support (requires logos-cpp-sdk).
LOGOS_CPP_SDK_DIR=/path/to/logos-cpp-sdk \
  cargo build --release -p logos-messaging-a2a-cli \
  --no-default-features --features shim,rest
export LMAO_BIN=$(realpath target/release/logos-messaging-a2a)

# Pass the createNode JSON for delivery_module explicitly
# (auto-discovery is broken in the current installed delivery_module
# — see issues.md).
export LMAO_AGENT_DELIVERY_CFG='{"logLevel":"WARN","mode":"Core","preset":"logos.dev"}'
```

Launch Basecamp (or `logoscore`) with `delivery_module`, `storage_module`,
and `agent` loaded. The agent module routes networking and storage through
Basecamp's own modules by default — sharing one Waku node and one Codex
node across all modules in the host. The spawned `lmao` will:

- talk to `delivery_module.createNode/start/send/subscribe` via QtRO
  (no bundled `liblogosdelivery.so`),
- talk to `storage_module.uploadInit/uploadChunk/uploadFinalize/downloadFile`
  via QtRO (no bundled libstorage),
- continue to use its own keyfile + IPC socket for identity / daemon
  protocol (the agent's secp256k1 pubkey is unchanged across modes).

Headless smoke:

```bash
logoscore -m ~/.local/share/Logos/LogosBasecampDev/modules \
  -l delivery_module,storage_module,agent \
  -c "agent.info()" --quit-on-finish
```

→ should return `{"kind":"info","name":"basecamp","pubkey":"…","capabilities":["text"],…}`.

#### Legacy mode (opt-out) — embedded liblogosdelivery + libstorage

The agent module spawns `lmao` with its own embedded Waku and Codex
nodes by default before shim support was added. To revert to that path
(e.g. when running outside a logos_host that has those modules loaded):

```bash
export LMAO_AGENT_USE_LEGACY=1
export LIBLOGOSDELIVERY_LIB_DIR=/path/to/logos-delivery/build
export LD_LIBRARY_PATH="$LIBLOGOSDELIVERY_LIB_DIR:$LD_LIBRARY_PATH"
export LMAO_BIN=$(realpath target/release/logos-messaging-a2a)  # built without --features shim
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

## How an agent uses LMAO

The agent's view of the protocol is what the `--exec` contract captures.
Anything that can read stdin and write stdout is a valid agent — pi,
Goose, Codex CLI, a shell script, your own binary.

**Per-task contract** — for each delivered task, `lmao agent run` invokes
your `--exec` command with:

| Channel | Direction | Meaning |
|---|---|---|
| **stdin** | in | Plain task text (or whatever the sender packed into the `Part`) |
| **stdout** | out | Your response — sent back over the mesh as the task result |
| **stderr** | out | Audit-log payload — uploaded to libstorage; the CID is appended to the response as `codex://...` |
| **exit code** | out | Zero = success → green ✓ on the requester's UI; non-zero = the task is reported failed with the last stderr line as the error |

**Environment passed to your exec** — set per-invocation by the daemon:

| Var | Value |
|---|---|
| `LMAO_SENDER_PUBKEY` | secp256k1 pubkey of the requesting agent — use it for trust-aware behaviour, audit context, etc. |
| `LMAO_SESSION_ID` | Conversation-thread id. First-turn tasks get a freshly stamped one; "Follow up" reuses it. Map this to your inference framework's session/thread mechanism (pi `--session $LMAO_SESSION_ID`, lemonade prefix-cache key, etc.) so the model keeps state across turns instead of cold-starting every time. |
| `PI_*` (when using `pi-exec`) | Provider/model/timeout knobs — see `scripts/pi-exec.sh` |

**Receiving + responding** — happens at the protocol level without the
agent code seeing it. Each task lands as an `A2AEnvelope` on
`/lmao/1/task-<your-pubkey>/proto`; if it's encrypted to your X25519
identity, the daemon decrypts before invoking exec. SDS dedup + ACKs
happen in the transport. Your stdout response is wrapped as a `Task`
with `state: Completed`, signed, optionally encrypted to the sender's
intro bundle, and published back.

**Becoming a delegator yourself** — an agent can be both worker and
orchestrator. Inside your `--exec`, drive the local daemon over its
Unix socket to fan-out subtasks:

```bash
lmao --daemon-socket "$LMAO_DAEMON_SOCKET" \
     task delegate --capability summarize \
     --text "$(cat)" --timeout 60
```

Same daemon, same connection, no extra mesh-joins. This is how
multi-step pipelines and agent-as-tool patterns compose.

**Optional per-agent tweaks**:

- Custom system prompt (when using `pi-exec`) — set `PI_SYSTEM_PROMPT`.
- Bound tool-call rounds — drop `demo-config/pi/agent/extensions/max-turns.ts`
  into your pi config dir and set `PI_MAX_TURNS=N`.
- Encryption — set `--encrypt` on `agent run`; the daemon publishes
  your X25519 intro bundle in the AgentCard, and senders that respect
  it will encrypt to you.
- Trust gating — see `lmao trust mode enforce` + `lmao trust add` to
  scope which peers can deliver tasks to you.

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
| `trust list` | Show the active trust list, mode, and source file |
| `trust add <pubkey> --nickname N [--cap c …] [--notes …]` | Add or replace a trusted peer (repeat `--cap` for multiple capabilities) |
| `trust remove <pubkey-or-nickname>` | Remove a trusted peer |
| `trust mode <off\|log\|enforce>` | Get or set enforcement mode |
| `trust import <file>` / `trust export <file>` | Bulk move the trust list across hosts |
| `daemon status` / `daemon stop` | Manage a running daemon |
| `health` | Probe the configured nwaku REST endpoint (legacy) |
| `metrics` | Show operational metrics counters |
| `info` | Identity + topic config (uses the daemon when available) |
| `completion <shell>` | Shell completions (bash / zsh / fish / …) |

## Configuration recipes

### pi against any OpenAI-compatible endpoint (recommended)

`pi-coding-agent` ships in the published image as `/usr/local/bin/pi-exec`
— a wrapper that streams the task text into pi, returns the answer on
stdout, and packs pi's full session trace + stderr into the audit-log
payload. Provider config is a tiny JSON pair:

```jsonc
// pi-config/agent/settings.json
{
  "defaultProvider": "lemonade",
  "defaultModel": "user.Qwen3.6-35B-A3B-Uncensored-HauhauCS-Aggressive",
  "hideThinkingBlock": true
}
```

```jsonc
// pi-config/agent/models.json — never commit this; contains API keys
{
  "providers": {
    "lemonade": {
      "baseUrl": "http://host.docker.internal:8000/v1",
      "api": "openai-completions",
      "apiKey": "lemonade",
      "compat": { "supportsDeveloperRole": false, "supportsReasoningEffort": false },
      "models": [{ "id": "user.Qwen3.6-35B-A3B-Uncensored-HauhauCS-Aggressive" }]
    }
  }
}
```

```bash
lmao agent run --name pi-analyst --capabilities analyze,review,explain,text \
    --exec /usr/local/bin/pi-exec
```

See [docs/RUNNING_REMOTE.md](docs/RUNNING_REMOTE.md) for the full
provider matrix (Venice, OpenRouter, Anthropic, Ollama, lemonade) and
the optional `max-turns` extension that bounds tool-call rounds.

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

`master` is the line that ships. Things that work today:

- `liblogosdelivery` + `libstorage` FFIs co-exist in one binary
- Real-network E2E for announce / discover / send / respond / presence
  / delegation / streaming / encryption
- Daemon mode (`agent run` + IPC) for `info`, `task`, `presence`,
  `storage fetch`, `trust *`
- `--exec` integration: any stdin/stdout-shaped CLI agent (pi, Goose,
  Codex, shell scripts) becomes the worker; conversation-thread state
  is threaded through `LMAO_SESSION_ID`
- Friend-keyring trust list (`Off` / `Log` / `Enforce` modes,
  capability-scoped) — runtime-mutable, persists to TOML
- Sealed presence — load-status broadcast as per-peer ChaCha20-Poly1305
  envelopes piggybacked on presence
- Basecamp UI plugin (agent module + agent UI) — published as both a
  plain LGX and a fat LGX (bundled `lmao` + `liblogosdelivery.so`) on
  every `v*` tag; CI in `.github/workflows/lgx.yml`
- Containerised demo — `Dockerfile` + `docker-compose.yml` + published
  `ghcr.io/vpavlin/lmao:dev` image for one-pull deploys

Things that are stale:
- The MCP bridge still uses the old transport-construction pattern;
  it works against `--transport rest` but hasn't been re-wired for
  `logos-delivery`.
- LEZ `ExecutionBackend` is `unimplemented!()` — only Status Network
  works for x402 payments.
- Parallel delegations from the Basecamp UI lose all but the last
  `delegate_complete` event under load — see [`docs/issues.md`](docs/issues.md).

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
- [x] Multi-agent delegation (FirstAvailable / CapabilityMatch / RoundRobin / BroadcastCollect / direct-by-pubkey)
- [x] Task streaming
- [x] x402 payment flow (Status Network EVM backend)
- [x] Friend-keyring trust list (Off / Log / Enforce, capability-scoped)
- [x] Sealed presence — encrypted load-status to trusted peers
- [x] Containerised demo — `Dockerfile` + `docker-compose.yml` + published `ghcr.io/vpavlin/lmao:dev` image
- [x] Logos Basecamp `.lgx` plugin pair — published as portable + fat LGXs on every `v*` tag
- [ ] MCP bridge migrated onto the daemon path
- [ ] Logos Chat SDK — Double Ratchet for forward secrecy
- [ ] LEZ agent registry — on-chain AgentCards via SPELbook
- [ ] Parallel-delegation event-loss fix in the Basecamp UI bridge

## Part of the SPEL Ecosystem

| Repo | Description |
|---|---|
| [spel](https://github.com/jimmy-claw/spel) | Smart Program Execution Layer — LEZ framework |
| [spelbook](https://github.com/jimmy-claw/spelbook) | On-chain program registry |
| [lez-multisig-framework](https://github.com/jimmy-claw/lez-multisig-framework) | Multisig governance |
| [lmao](https://github.com/vpavlin/lmao) | This repo — A2A agent orchestration |

## License

MIT.
