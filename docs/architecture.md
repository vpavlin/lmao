# LMAO Architecture

## Stack at a Glance

```
┌──────────────────────────────────────────────────────────────────────┐
│                        Application Layer                             │
│                                                                      │
│  ┌──────────────┐  ┌──────────────┐  ┌─────────────────────────┐     │
│  │   lmao CLI   │  │  lmao agent  │  │     Basecamp pair       │     │
│  │              │  │     run      │  │                         │     │
│  │ info, peers, │  │              │  │  agent (universal core) │     │
│  │ task send,   │  │  daemon +    │  │  agent_ui (QML view)    │     │
│  │ delegate,    │  │  inbox loop  │  │                         │     │
│  │ storage,     │  │  + executor  │  │  Spawns `lmao agent     │     │
│  │ presence     │  │  + storage   │  │  run`, talks to it via  │     │
│  └──────┬───────┘  └──────┬───────┘  │  the same IPC socket    │     │
│         │  IPC            │          └────────────┬────────────┘     │
│         ▼                 │                       │                  │
│  ┌────────────────────────┴───────────────────────┴───────────┐      │
│  │           Unix-socket IPC (length-prefixed JSON)           │      │
│  │     $XDG_RUNTIME_DIR/lmao.sock — one request per conn      │      │
│  └────────────────────────────────────────────────────────────┘      │
│                                                                      │
├──────────────────────────────────────────────────────────────────────┤
│                          Node Layer                                  │
│                                                                      │
│  ┌────────────────────────────────────────────────────────────┐      │
│  │                    LmaoNode<T: Transport>                  │      │
│  │                                                            │      │
│  │  announce()/discover()  — AgentCard pub/sub                │      │
│  │  send_task()/poll_tasks()/respond()  — A2A task flow       │      │
│  │  poll_presence()        — live PeerMap from heartbeats     │      │
│  │  delegate_task()        — fan-out to capable peers         │      │
│  │  respond_stream()/poll_stream_chunks()/reassemble_stream() │      │
│  │                                                            │      │
│  │  Identity:  secp256k1 + optional X25519 IntroBundle        │      │
│  │  Caches:    discover_rx, presence_rx, stream_rx, task_rx   │      │
│  │             (subscriptions are sticky — gossip mesh        │      │
│  │              doesn't buffer for late subscribers)          │      │
│  └──┬──────────┬──────────┬──────────┬────────────────────────┘      │
│     │          │          │          │                                │
├─────┼──────────┼──────────┼──────────┼────────────────────────────────┤
│     │          │          │          │                                │
│  ┌──┴────┐ ┌───┴────┐ ┌───┴─────┐ ┌──┴──────────┐                    │
│  │Crypto │ │Storage │ │Executor │ │  Execution  │                    │
│  │       │ │        │ │         │ │             │                    │
│  │X25519 │ │trait   │ │--exec   │ │ Status Net  │                    │
│  │ECDH + │ │Storage-│ │CLI      │ │ EVM client  │                    │
│  │ChaCha │ │Backend │ │(stdin/  │ │ (x402 only) │                    │
│  │20-    │ │        │ │ stdout) │ │             │                    │
│  │Poly   │ │impls:  │ │         │ │ LEZ stub    │                    │
│  │1305   │ │ libstg │ │default: │ │             │                    │
│  │       │ │ rest   │ │ goose   │ │             │                    │
│  │Intro- │ │ logos- │ │         │ │             │                    │
│  │Bundle │ │  core  │ │         │ │             │                    │
│  └───────┘ └────────┘ └─────────┘ └─────────────┘                    │
│                                                                      │
├──────────────────────────────────────────────────────────────────────┤
│              Reliability Layer  (logos-messaging-a2a-transport::sds) │
│                                                                      │
│  ┌────────────────────────────────────────────────────────────┐      │
│  │              SdsTransport<T: Transport>                    │      │
│  │                                                            │      │
│  │   publish_reliable() / poll_dedup() / send_ack()           │      │
│  │   lamport clocks · bloom-filter dedup · batch ACK          │      │
│  │   retransmit ≤ 3× · ACK timeout 10 s                       │      │
│  └────────────────────────┬───────────────────────────────────┘      │
│                           │                                          │
├───────────────────────────┼──────────────────────────────────────────┤
│                  Transport Layer  (swappable trait Transport)        │
│                                                                      │
│  ┌────────────────────────┴───────────────────────────────────┐      │
│  │                      trait Transport                       │      │
│  │   publish · subscribe (mpsc::Receiver) · unsubscribe       │      │
│  ├────────────────────────────────────────────────────────────┤      │
│  │ LogosDeliveryTransport     LogosCoreDeliveryTransport      │      │
│  │ (`logos-delivery` feat,    (`logos-core` feat,             │      │
│  │  default)                   IPC via delivery_module)       │      │
│  │ liblogosdelivery FFI                                       │      │
│  │ preset: "logos.dev"                                        │      │
│  ├────────────────────────────────────────────────────────────┤      │
│  │ LogosMessagingTransport    InMemoryTransport               │      │
│  │ (`rest` feat,               (built-in, no deps)            │      │
│  │  nwaku REST fallback)       in-process tests + demos       │      │
│  └────────────────────────┬───────────────────────────────────┘      │
│                           │                                          │
├───────────────────────────┼──────────────────────────────────────────┤
│                       Logos Messaging Network                        │
│                                                                      │
│  ┌────────────────────────┴───────────────────────────────────┐      │
│  │  Pub/Sub Relay  (Waku-derived gossipsub mesh)              │      │
│  │                                                            │      │
│  │  Content topics (4 segments: /app/gen/name/enc):           │      │
│  │   /lmao/1/discovery/proto             AgentCard broadcasts │      │
│  │   /lmao/1/presence/proto              PresenceAnnounce     │      │
│  │   /lmao/1/task-{pubkey}/proto         per-agent task inbox │      │
│  │   /lmao/1/ack-{msg_id}/proto          SDS acknowledgements │      │
│  │   /lmao/1/stream-{task_id}/proto      respond_stream chunks│      │
│  │                                                            │      │
│  │  Names like `task-{pubkey}` are hyphen-encoded type+id —   │      │
│  │  the underlying transport requires exactly 4 segments.     │      │
│  └────────────────────────────────────────────────────────────┘      │
└──────────────────────────────────────────────────────────────────────┘
```

## Crate Layout

```
logos-messaging-a2a (workspace root)
│
├── logos-messaging-a2a-core           A2A protocol types, topics, delegation, retry
│   └── deps: crypto
│
├── logos-messaging-a2a-crypto         X25519 ECDH + ChaCha20-Poly1305 + identity
│   └── deps: (none internal)
│
├── logos-messaging-a2a-transport      Transport trait + SDS reliability + backends
│   ├── feat `logos-delivery`  → LogosDeliveryTransport (liblogosdelivery FFI)
│   ├── feat `logos-core`      → LogosCoreDeliveryTransport (Logos Core IPC)
│   ├── feat `rest`            → LogosMessagingTransport (nwaku REST fallback)
│   └── always: InMemoryTransport (in-process)
│
├── logos-messaging-a2a-storage        StorageBackend trait + impls
│   ├── feat `libstorage`      → LibstorageBackend (embedded Codex via FFI)
│   ├── feat `logos-core`      → LogosCoreStorageBackend (Logos Core IPC)
│   └── always: LogosStorageRest (Codex REST fallback)
│
├── logos-messaging-a2a-execution      x402 payments (Status Network EVM), LEZ stub
│   └── deps: core
│
├── logos-messaging-a2a-node           LmaoNode — orchestrator
│   └── deps: core, crypto, transport, storage, execution
│
├── logos-messaging-a2a-cli            `lmao` binary: agent run + daemon-aware sub-cmds
│   └── deps: core, crypto, transport, storage, node
│
├── logos-messaging-a2a-mcp            MCP stdio bridge (Claude/Cursor → A2A fleet)
│   └── deps: core, transport, node
│
├── logos-messaging-a2a-ffi            UniFFI bindings (Swift / Kotlin)
│   └── deps: core, crypto, transport, node
│
└── lmao-ffi                           C-ABI thin wrapper
    └── deps: core, transport, node
```

## CLI Daemon Mode

`lmao agent run` is both the long-lived agent **and** the IPC daemon. Other
CLI subcommands (`info`, `peers`, `task send`, `task delegate`, `task status`,
`storage fetch`) probe the socket first and fall back to a one-shot transport
build if no daemon is running.

```
                          $XDG_RUNTIME_DIR/lmao.sock
                                       │
        ┌──────────────────────────────┼──────────────────────────────┐
        ▼                              ▼                              ▼
  lmao info               lmao task send …            lmao task delegate …
   (probe)                    (probe)                       (probe)
        │                              │                              │
        └──────── 4-byte LE length-prefix + JSON Request ──────────────┘
                                       │
                                       ▼
                        ┌───────────────────────────┐
                        │   lmao agent run (daemon) │
                        │                           │
                        │   ┌─────────────────────┐ │
                        │   │   DaemonServer      │ │     Inbox loop:
                        │   │   accept loop       │ │      poll_tasks()
                        │   └────────┬────────────┘ │      poll_presence()
                        │            │              │      respond + exec
                        │            ▼              │      upload log → CID
                        │   ┌─────────────────────┐ │
                        │   │  Request handler    │ │
                        │   │  → LmaoNode methods │ │
                        │   │  → StorageBackend   │ │
                        │   └─────────────────────┘ │
                        └───────────────────────────┘
```

Wire format (`crates/logos-messaging-a2a-cli/src/daemon/protocol.rs`):

| Frame field      | Bytes             | Notes                                |
|------------------|-------------------|--------------------------------------|
| length           | 4 (LE u32)        | hard cap: `MAX_FRAME_BYTES = 16 MiB` |
| body             | `length` UTF-8    | `{ "kind": "...", ... }` JSON         |

Request kinds: `info`, `discover`, `presence_peers`, `task_send`,
`task_status`, `task_delegate`, `storage_fetch`, `shutdown`. Every connection
sends exactly one request, reads exactly one response, and closes — no
multiplexing, no correlation IDs.

Default socket path resolution (in order):
1. `$XDG_RUNTIME_DIR/lmao.sock`  (preferred — tmpfs, per-session)
2. `$XDG_CACHE_HOME/lmao/lmao.sock`
3. `$HOME/.cache/lmao/lmao.sock`
4. `/tmp/lmao.sock`

## Agent Execution Flow

```
Sender                 Logos Messaging          Agent (lmao agent run)
  │                          │                          │
  │── send_task(text) ───────▶ /lmao/1/task-{B}/proto ───▶ poll_tasks()
  │                          │                          │
  │                          │              ┌───────────┴────────────┐
  │                          │              │  spawn(--exec) on stdin │
  │                          │              │  collect stdout         │
  │                          │              │  log full transcript    │
  │                          │              └───────────┬────────────┘
  │                          │                          │
  │                          │                          ▼
  │                          │              ┌───────────────────────┐
  │                          │              │ libstorage (embedded  │
  │                          │              │ Codex). Upload log;   │
  │                          │              │ return CID.           │
  │                          │              └───────────┬───────────┘
  │                          │                          │
  │                          │                          ▼
  │                          │              answer + "\n---\nexecution
  │                          │              log: codex://<CID>"
  │◀─ poll_tasks() ──────────│ /lmao/1/task-{A}/proto ◀── respond()
```

Key design points:

- The executor is any process that **reads task text on stdin and prints
  the answer to stdout**. Default is [Goose](https://github.com/block/goose);
  any OpenAI-compatible CLI works. Configured via `--exec`.
- The full execution transcript (LLM messages, tool calls, errors) is
  uploaded to Logos Storage and the CID appended to the response — the
  task message itself stays small while the audit trail is content-addressed.
- Storage offload is opportunistic: if `--storage-backend` is unset,
  the agent skips upload and replies with the bare answer. Failures during
  upload are non-fatal; the response still ships, with an `exec_error`
  field added to the audit log if the executor itself failed.

## A2A Wire Types  (`logos-messaging-a2a-core`)

```
AgentCard
├── name, description, version
├── capabilities: Vec<String>
├── public_key: secp256k1 compressed hex
└── intro_bundle: Option<IntroBundle>      X25519 + version

Task
├── id: UUID v4
├── from, to: pubkey hex
├── state: Submitted → Working → Completed | Failed
├── message: Message { role, parts: [Part::Text { text }, …] }
└── result: Option<Message>

A2AEnvelope (wire format on every topic)
├── AgentCard(AgentCard)
├── Task(Task)
├── EncryptedTask { from, to, nonce, ciphertext }
├── StreamChunk { task_id, seq, is_final, text }
├── Ack { message_id }
├── Presence(PresenceAnnouncement)
├── DelegationRequest / DelegationResult
└── PaymentRequired (x402)
```

## Discovery + Presence

Two complementary mechanisms:

| Channel               | Topic                          | Lifecycle                          |
|-----------------------|--------------------------------|------------------------------------|
| AgentCard broadcast   | `/lmao/1/discovery/proto`      | One-shot on `announce()`. Receiver caches in `AgentRegistry`. |
| Presence heartbeat    | `/lmao/1/presence/proto`       | Periodic; signed; `PeerMap` ages out stale entries by `ttl_secs`. |

**The gossip mesh does not buffer for late subscribers** — `LmaoNode`
caches the subscription `Receiver`s (`discover_rx`, `presence_rx`,
`stream_rx`, `task_rx`) in cells so a polling caller drains incremental
deltas instead of subscribing-then-immediately-unsubscribing on each call.

`PresenceAnnouncement` is signed over a canonical-JSON serialization
(fixed key order, `signature` field excluded) using the agent's secp256k1
key. `PeerMap` rejects entries with an invalid signature or whose `agent_id`
doesn't match the recovered pubkey.

## Encrypted Tasks

When both ends carry `IntroBundle`s, `send_task_to(&task, Some(&peer_card))`
auto-derives a ChaCha20-Poly1305 session key via X25519 ECDH and ships the
task as an `EncryptedTask` envelope. `poll_tasks()` decrypts transparently
when the local node has the matching identity. No session-key rotation —
each task gets a fresh nonce off the same shared secret. See
`examples/logos_delivery_encrypted.rs` for the end-to-end flow.

## Streaming

`respond_stream(task, chunks)` publishes each chunk on
`/lmao/1/stream-{task_id}/proto` with a sequence number and an `is_final`
flag on the last chunk. The receiver (`poll_stream_chunks(task_id)`) drains
the cached subscription, and `reassemble_stream(task_id)` returns the
concatenated text once `is_final` has arrived. Out-of-order delivery is
handled by sorting on `seq` at reassembly time.

## Delegation

```
Orchestrator        PeerMap            Worker A / Worker B
  │                    │                       │
  │── delegate_task ──▶│                       │
  │   (strategy,       │                       │
  │    timeout_secs)   │                       │
  │                    │  select peer(s):      │
  │                    │   FirstAvailable      │
  │                    │   CapabilityMatch     │
  │                    │   BroadcastCollect    │
  │                    │   RoundRobin          │
  │                    │                       │
  │── Task(subtask) ───┼──────────────────────▶│
  │                    │                       │
  │   poll_tasks loop  │                       │
  │   up to timeout    │                       │
  │                    │                       │
  │◀── DelegationResult┼───────────────────────│
```

Strategy selection is fully client-side; the worker just sees a normal
`Task` on its inbox. `BroadcastCollect` returns one `DelegationResult` per
responding peer; the others return exactly one.

## Storage Offload

The node's `with_storage_offload(StorageOffloadConfig)` wraps any
`StorageBackend` implementation. When a task's payload exceeds
`max_inline_bytes` (default 100 KiB), the node uploads the bytes, replaces
the inline content with a `storage_cid` reference, and ships the slim
envelope. Receivers fetch on-demand. The audit-log upload performed by
`lmao agent run` uses the same backend.

| Backend                  | Feature flag    | Notes                                           |
|--------------------------|-----------------|-------------------------------------------------|
| `LibstorageBackend`      | `libstorage`    | Embedded Codex via storage-bindings 0.2.3 FFI. Default for the CLI. |
| `LogosCoreStorageBackend`| `logos-core`    | Logos Core IPC via `storage_module`. For Basecamp host integration. |
| `LogosStorageRest`       | (always)        | REST fallback when an external Codex node is preferred. |

## x402 Payment Flow

```
Client                  Worker (paywall)
  │                          │
  │── send_task(request) ────▶│
  │                          │
  │◀── 402 PaymentRequired ──│   { token_contract, recipient,
  │                          │     amount, network }
  │                          │
  │── ERC-20 transfer ───────▶│   on Status Network (EVM via execution)
  │                          │
  │── send_task(request +    │
  │     payment_tx_hash) ────▶│   verify on-chain
  │                          │
  │◀── Task(Completed) ──────│
```

Payments are scoped to the `logos-messaging-a2a-execution` crate and gated
on `with_payment(PaymentConfig)`. The default build does not require any
EVM credentials.

## Retry + SDS

P2P transports are unreliable; LMAO defends in two layers:

1. **SDS** (`logos-messaging-a2a-transport::sds`): per-message ACK,
   bloom-filter dedup, lamport-ordered buffering. Up to 3 retransmissions,
   ACK timeout 10 s.
2. **RetryLayer** (`logos-messaging-a2a-node::retry`): wraps `send_reliable`
   with exponential backoff (`base × 2^n`, capped at `max_delay_ms`,
   optional jitter). Only **transport errors** are retried — un-ACKed
   `Ok` results are left to SDS.

Configure via `LmaoNode::with_retry(RetryConfig { max_attempts: 5,
base_delay_ms: 1000, max_delay_ms: 60_000, jitter: true })`.

## Basecamp Module Pair

Two installable LGX packages live under `basecamp/`:

- **`agent`** (universal core, `type: core`, `interface: universal`) —
  C++ host that spawns `lmao agent run` as a child and proxies
  `Q_INVOKABLE` calls through the same Unix-socket IPC the CLI uses.
- **`agent_ui`** (`type: ui_qml`) — pure QML view with four panes
  (Status, Peers, Delegate, Audit). Calls `logos.callModule("agent",
  method, args)` for everything.

```
Basecamp host                 agent module                lmao agent run
                              (universal core)            (subprocess)
  ┌──────────────┐
  │  agent_ui    │  Q_INVOKABLE
  │  (QML view)  │ ───────────────▶ ┌──────────────┐
  │              │                  │  agent_impl  │   IPC over
  │              │                  │  QLocalSocket│ ─────────────▶ lmao.sock
  │              │ ◀─────────────── │              │
  └──────────────┘   JSON results   └──────┬───────┘                  │
                                           │  spawn on init           │
                                           ▼                          │
                                     `lmao agent run …`  ◀────────────┘
                                     (one process per agent)
```

The IPC contract is identical to the CLI: the module is just another
client of the daemon socket.

## MCP Bridge

`logos-messaging-a2a-mcp` exposes the agent fleet as MCP tools to hosts
like Claude Desktop or Cursor. Stdio transport, no HTTP server.

| Tool                | Description                                        |
|---------------------|----------------------------------------------------|
| `discover_agents`   | Drain discovery topic, cache, return AgentCards.   |
| `send_to_agent`     | Send a task by agent name, poll for the response.  |
| `list_cached_agents`| Return cache without a network round-trip.         |

Configured via CLI flags: `--waku-url` (REST endpoint when using the
`rest` feature), `--timeout` (response wait, default 30 s).

## Topic Format Constraint

The Logos Messaging transport requires content topics to have **exactly
four segments**: `/<app>/<generation>/<name>/<encoding>`. Earlier drafts
used five-segment topics (`/lmao/1/task/{pubkey}/proto`), but
liblogosdelivery rejects those — the second segment must parse as a
numeric generation. The current convention squashes type and id into a
single `name` segment with a hyphen separator: `task-{pubkey}`,
`stream-{task_id}`, `ack-{msg_id}`.

See `crates/logos-messaging-a2a-core/src/topics.rs` for the canonical
encoders.
