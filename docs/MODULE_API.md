# Agent module — public API

The agent module is loadable into Logos Basecamp (and any future
Logos-core host) as a regular module. Other modules and UIs depend
on it via `LogosAPI::callModule("agent", method, args)` from C++ /
`logos.callModule("agent", method, args)` from QML.

This document is the contract. Methods listed here are part of the
**Client API** — the surface other modules code against. Method
signatures are stable; behaviour changes need a version bump.

The implementation header is `basecamp/agent-module/src/agent_impl.h`.
That file additionally lists an **Operator API** (`trust_*`,
`stop_daemon`, `is_running`) used only by an agent's own
admin/settings UI. Other modules should not depend on those.

## Use case

A Notes app inside Basecamp wants to summarise a note, turn it into
a travel report, draft a code review, etc. — using whichever peer on
the user's mesh advertises the matching capability.

```qml
// In the Notes app's QML, on a "Summarise" menu action:
const ackRaw = logos.callModule("agent", "start_delegate",
    [/* capability */ "summarize",
     /* text       */ note.body,
     /* session_id */ ""]);
const ack = JSON.parse(ackRaw);
if (ack.error) {
    // surface the failure
    return;
}
// ack: { task_id, session_id }
// Listen for completion:
Connections {
    target: logos
    function onModuleEventReceived(moduleName, eventName, data) {
        if (moduleName !== "agent" || eventName !== "delegate_complete") return;
        const event = JSON.parse(data[0]);
        if (event.task_id !== ack.task_id) return;
        if (!event.success) {
            // event.error has the reason
            return;
        }
        appendSummaryToNote(note, event.body);
        if (event.cid) saveAuditLogCid(note, event.cid);
    }
}
```

The agent module routes the task by capability, picks a trusted live
peer, ships the text, waits for the response, fires the
`delegate_complete` event back to the Notes app.

## Method conventions

- All methods return JSON-encoded `std::string`.
- All methods are reachable via `logos.callModule(...)` / `getClient(...)`.
- **Async methods** (`start_*`) return immediately with an ack
  containing a request id (`task_id` for delegations, `request_id`
  for fetches); the actual result arrives later as a named event
  carrying that id. **Prefer these from a UI thread.**
- **Sync methods** block on a fast IPC round-trip (~10 ms for
  metadata, longer for `delegate` / `fetch_cid` because they wait
  on network).

### Error shapes

A consumer must handle two error shapes:

```jsonc
// (1) Validation error from the agent module itself —
//     daemon not running, malformed args, etc.
{ "error": "daemon not running" }

// (2) Forwarded error from the underlying daemon process —
//     network failure, no live peers, capability mismatch,
//     trust filter blocked the target, …
{ "kind": "error", "message": "no trusted peers with capability 'summarize' for delegation: 1 live peer(s) all filtered by trust list (mode=Enforce). Add them with `lmao trust add <pubkey>` or pass `--trust-file <path>` to use a different list." }
```

A defensive parser:

```js
function readAgentError(obj) {
    if (!obj) return "unparseable response";
    if (obj.error) return obj.error;
    if (obj.kind === "error" && obj.message) return obj.message;
    return null;  // not an error
}
```

## Methods (Client API)

### `info()`

Daemon identity, uptime, current configuration. Synchronous.

**Args**: none.

**Returns**:

```jsonc
{
  "name": "alice",
  "pubkey": "0226e882fbb0efd6...",
  "capabilities": ["text", "summarize"],
  "encryption_pubkey": "cd9de6e0d7bba572...",   // optional — present when --encrypt is enabled
  "storage_spr": "spr:CiUIAhIh...",            // optional — present when storage is enabled
  "load": { "bucket": "free", "queue_depth": 0, "max_concurrent": 1, "avg_latency_ms": 0 },
  "uptime_secs": 2079,
  "socket_path": "/run/user/1000/lmao-basecamp-685661.sock"
}
```

### `daemon_state()`

Local liveness probe — no IPC round-trip. Tri-state badge for UIs.

**Args**: none.

**Returns**: a JSON-encoded string, one of:

- `"ready"` — daemon socket up, accepting IPC.
- `"starting"` — subprocess spawned, still dialing the mesh / binding the
  socket. Typically 10–20 s on a cold start.
- `"offline"` — subprocess never started, or has exited.

> Note: some Basecamp builds JSON-encode `std::string` returns again
> on the wire. If your `JSON.parse` yields a string instead of a
> parsed object, re-parse it. See the `parseModuleJson` helper in
> `basecamp/agent-ui/Main.qml` for a portable pattern.

### `peers(capability_filter)`

Snapshot of the daemon's PeerMap.

**Args**:

| Name | Type | Meaning |
|---|---|---|
| `capability_filter` | `string` | Empty = list all live peers. Non-empty (e.g. `"code"`) = only peers advertising that capability. |

**Returns**:

```jsonc
{
  "kind": "presence_peers",
  "peers": [
    {
      "agent_id": "02b6a180a38e5dfe...",
      "name": "bob",
      "capabilities": ["code", "review"],
      "waku_topic": "/lmao/1/task-02b6a180a38e5dfe.../proto",
      "last_seen_secs": 1778310987,
      "ttl_secs": 300,
      "load": { "bucket": "free", "queue_depth": 0, "max_concurrent": 1, "avg_latency_ms": 0 }
    }
  ]
}
```

### `start_delegate(capability, text, session_id)`

Async delegate by capability. **Recommended entry point** for
delegation from any UI thread.

**Args**:

| Name | Type | Meaning |
|---|---|---|
| `capability` | `string` | e.g. `"summarize"`. The agent module routes to a peer that advertises this. |
| `text` | `string` | Subtask text — the prompt or input passed to the receiver's executor on stdin. |
| `session_id` | `string` | Empty = stamp a fresh session. Non-empty = continue an existing conversation thread (Follow-up). |

**Returns** (synchronous ack):

```jsonc
{ "task_id": "fcc907c9-a371-...", "session_id": "<echoed-or-stamped>" }
```

**Emits** when the delegation finishes (via `emitEvent`):

```jsonc
// Event name: "delegate_complete"
// data[0]:
{
  "task_id": "fcc907c9-a371-...",      // matches the ack's task_id
  "success": true,
  "agent_id": "02b6a180a38e5dfe...",   // peer that handled it
  "body": "Concise three-sentence summary…",
  "cid": "zDvZRwzm6E2hZoge2PnJC1RWZ9o…", // when storage is enabled
  "error": "",                          // populated when success == false
  "elapsed_ms": 4320
}
```

The QML side wires this with `Connections { target: logos; function onModuleEventReceived(moduleName, eventName, data) {…} }` and routes by `task_id`.

### `delegate(capability, text)`

Synchronous delegate. Blocks the caller for up to
`LMAO_AGENT_DELEGATE_TIMEOUT_SECS` (default 180 s). Use only from
worker threads.

**Returns** the same shape as the `delegate_complete` event payload
(without the `task_id`).

### `send_task(recipient_pubkey, text)`

Direct fire-and-forget send to a specific peer.

**Args**:

| Name | Type | Meaning |
|---|---|---|
| `recipient_pubkey` | `string` | secp256k1 compressed-hex pubkey, as appears in `peers` / `info`. |
| `text` | `string` | Task text. |

**Returns**:

```jsonc
{ "kind": "task_send", "task_id": "c38a0b28-...", "from": "<my-pubkey>", "acked": true }
```

`acked` is `true` once the recipient acknowledges the envelope
(SDS layer); the executor result, if any, arrives later via the same
`delegate_complete` event channel.

### `start_fetch_cid(cid)`

Async fetch of a content-addressed payload from the storage backend.
**Recommended** for any CID-resolution UI — Codex DHT walks can take
tens of seconds.

**Args**:

| Name | Type | Meaning |
|---|---|---|
| `cid` | `string` | The CID. Strip any `codex://` prefix before calling. |

**Returns** (synchronous ack):

```jsonc
{ "request_id": "abc123", "cid": "zDvZRwzm6E2hZ..." }
```

**Emits**:

```jsonc
// Event name: "fetch_cid_complete"
// data[0]:
{
  "request_id": "abc123",
  "cid": "zDvZRwzm6E2hZ...",
  "success": true,
  "payload_b64": "Y29udGVudHM=",   // base64-encoded bytes
  "error": ""
}
```

### `fetch_cid(cid)`

Synchronous fetch — blocks for up to 90 s. Returns
`{cid, payload_b64}` on success.

### `task_history_list(limit, offset, direction, capability)`

Page through persisted task history (newest first).

**Args**:

| Name | Type | Meaning |
|---|---|---|
| `limit` | `int64` | Max rows to return. |
| `offset` | `int64` | Rows to skip before the page. |
| `direction` | `string` | `""` = both, `"sent"` = delegations from us, `"received"` = tasks others sent us. |
| `capability` | `string` | `""` = no filter, otherwise restrict to that capability. |

**Returns**:

```jsonc
{
  "kind": "task_history_list",
  "entries": [
    {
      "task_id": "fcc907c9-...",
      "direction": "sent",
      "peer_pubkey": "02b6a180a38e5dfe...",
      "peer_name": "bob",
      "capability": "summarize",
      "text": "<original task text>",
      "body": "<response>",
      "cid": "zDv...",
      "success": true,
      "error": "",
      "started_at_secs": 1778310987,
      "elapsed_ms": 4320,
      "session_id": "..."
    }
  ],
  "history_path": "/.../history.jsonl"
}
```

### `task_history_get(task_id)`

One persisted task by id.

**Returns**: `{kind: "task_history_get", entry: {…} | null}` with the
same `entry` shape as in `task_history_list`.

## Events emitted

| Event name | Carries | Payload shape |
|---|---|---|
| `delegate_complete` | result of a `start_delegate` call | see above |
| `fetch_cid_complete` | result of a `start_fetch_cid` call | see above |

Consumers must subscribe explicitly:

```qml
Component.onCompleted: {
    logos.onModuleEvent("agent", "delegate_complete");
    logos.onModuleEvent("agent", "fetch_cid_complete");
}
```

…before the events flow. Without the explicit subscribe the QML bridge
filters them out.

## Stability

This document tracks the contract. Method names + argument shapes are
stable; new fields may be added to return / event payloads (consumers
should ignore unknown keys). Breaking changes get a major version bump
in `basecamp/agent-module/metadata.json` and a migration note here.

The Operator API methods listed in `agent_impl.h` (`trust_*`,
`stop_daemon`, `is_running`) are explicitly NOT part of this contract.
