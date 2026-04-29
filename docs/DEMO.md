# LMAO Stage Demo — Speaker Notes

A 5-minute live walkthrough of two agents on the real `logos.dev` Logos
Messaging fleet, delegating a task by capability, and surfacing a
content-addressed audit log. Designed for ETHPrague but should work for
any developer audience.

This doc is meant to be **read on stage**. Each section has:

- **Run** — the command to type
- **Show** — what the audience sees
- **Say** — the sentence(s) that go with it

## TL;DR Pre-Flight Checklist

Run these once, the day before, in the room with the projector:

```bash
# 1. Build the CLI with logos-delivery + libstorage (~3 min cold)
export LIBLOGOSDELIVERY_LIB_DIR=/path/to/logos-delivery/build
export LD_LIBRARY_PATH=$LIBLOGOSDELIVERY_LIB_DIR
make cli-logos-delivery

# 2. Confirm liblogosdelivery is reachable
ls "$LIBLOGOSDELIVERY_LIB_DIR/liblogosdelivery.so"

# 3. Confirm Goose works (or whatever --exec you pick)
echo "say hi" | goose run --no-session -i - --output-format text --quiet

# 4. Optional: warm the docker image so the containerised path is fast
make demo-image     # ~15-20 min cold, ~30 s warm

# 5. Optional: build Basecamp packages so the GUI demo is ready
make basecamp-lgx
```

Everything below assumes those four envs (`LIBLOGOSDELIVERY_LIB_DIR`,
`LD_LIBRARY_PATH`, optionally `LMAO_DEMO_ALICE_EXEC`,
`LMAO_DEMO_BOB_EXEC`) are in the shell.

## Path A — Bare-Host CLI Demo (the default)

The fastest path. One `make demo`, five steps, ~30 seconds end-to-end
once both agents have joined the gossip mesh.

### Step 0 — set up the executor

**Run** (once before the demo):

```bash
export LMAO_DEMO_ALICE_EXEC='goose run --no-session -i - --output-format text --quiet'
export LMAO_DEMO_BOB_EXEC='goose run --no-session -i - --output-format text --quiet'
```

**Say**: "Each agent runs an executor — any process that takes the task
on stdin and prints the answer on stdout. We're using Goose because it
ships with tool-use out of the box, but anything OpenAI-compatible works
— `llm`, `aider`, a shell script, whatever."

If Goose isn't available in the room, **leave the env unset** —
`scripts/demo.sh` falls back to a `sed` stub that visibly tags responses
with `[summarized]` or `[reviewed]`. Use that to keep the network parts
of the story intact even if the LLM path is flaky.

### Step 1 — fire it up

**Run**:

```bash
make demo
```

**Show**: the script prints

```
═══ LMAO demo on logos.dev ═══

[1/5] starting two agents (persistent identities + embedded storage + IPC sockets)…
  starting alice (caps: text,summarize, tcp:60010 udp:9010 storage:19200 sock:.demo/alice.sock)...
  starting bob (caps: code,review, tcp:60011 udp:9011 storage:19201 sock:.demo/bob.sock)...

[2/5] waiting for each to connect to logos.dev and announce…
  alice pubkey: 02ab1234567890ab…
  bob   pubkey: 03cd9876543210cd…
```

**Say**: "Two processes, each with a persistent secp256k1 identity, each
joining the live `logos.dev` gossip mesh on its own libp2p port. No
servers. No central registry. This is the same fleet a Logos Messaging
client elsewhere on the planet sees."

The `wait_for_pubkey` helper holds the script until each agent prints
its pubkey, which happens once it's *actually* connected to the mesh —
typically 5-10 s. If it's slow, narrate the wait: this is a real public
network handshake.

### Step 2 — discover peers

**Show**:

```
[3/5] discovering peers via presence (through alice's daemon — no new node)…
peer  bob  03cd9876…  caps=[code,review]  ttl=60s
```

**Say**: "Discovery is gossiped on `/lmao/1/presence/proto` — every agent
broadcasts a signed `PresenceAnnouncement` with its capabilities, every
peer keeps a `PeerMap` aged out by TTL. We're running this query through
**alice's already-running daemon** — IPC over a Unix socket — instead of
spinning up a fresh logos-delivery node, which would take another five
seconds to join the mesh. The daemon collapses that to a sub-millisecond
round-trip."

This is the key architectural beat. Linger here.

### Step 3 — capability-routed delegation

**Show**:

```
[4/5] delegating a task by capability=code → bob (via alice's daemon)…
delegating to bob (caps=[code,review]) for parent task <uuid>…
result from bob: <Goose output>

execution log: codex://Qm…
```

**Say**: "Alice didn't address bob by pubkey — she said 'whoever has the
`code` capability'. The delegation strategy is `CapabilityMatch`. Bob
got the task, ran it through Goose, and replied with the answer plus a
**content-addressed pointer to the full execution log** — every LLM
message, every tool call, every error — uploaded to embedded Codex."

Substitutions for the live audience to land the point:

- "Tool use you can audit after the fact" — for the security crowd.
- "Decentralised compute marketplace where the receipt is verifiable"
  — for the crypto crowd.
- "Local model on K11, not OpenAI" — for the privacy crowd.

### Step 4 — fetch the audit log

**Show**:

```
[5/5] fetching bob's execution log by CID via bob's daemon…
  cid: Qm…
  {"status":"...","steps":[{"role":"user","content":"Review this snippet…"}, …]}
```

**Say**: "The CID we just got from bob — we can fetch it. From any node
in the network. This isn't a screenshot of a log; it's the actual
content-addressed bytes that produced the response. If bob ever lies
about what he did, the log calls him out."

This is also the natural place to wave at `docs/TRUST.md`: "Today the
log is honest because the agent is. The roadmap layers on RLN for
rate-limiting, Semaphore for community membership, EAS for capability
attestations — so 'the agent that's the model it claims to be' becomes
a verifiable claim. Out of scope for the demo, in scope for the design
doc."

### Step 5 — wrap up

**Run** (Ctrl-C the script, or let it return — `trap cleanup EXIT`
kills both background agents):

```bash
ls .demo/
# alice.key  alice.log  alice.sock  bob.key  bob.log  bob.sock  storage-alice/  storage-bob/
```

**Say**: "Both agents have persistent identities — those keyfiles. We
just stop the processes. Next time we run `make demo` they re-join the
mesh under the same pubkeys. Same Codex blockstore. Same audit history."

## Path B — Containerised Demo (the security story)

When asked "what about untrusted code execution?", switch to:

```bash
make demo-containerized
```

**Say**: "Same five-step narrative — but each agent now runs in its own
debian-slim container, non-root user, no host filesystem access except
a scoped data volume. If Goose's tool use goes wild, the blast radius
is the container's `/tmp`. The host can't be touched."

The script orchestrates from the host, talking to each container's
daemon socket via shared volume mounts (`demo-data/alice-sock/`,
`demo-data/bob-sock/`). The IPC contract is identical — the host's
`lmao` binary is just another client of each daemon.

First run on a cold machine builds the image (~15-20 min: Nim + Rust +
Goose download). After that it's ~30 s.

Tear down with `make demo-down`.

## Path C — Basecamp Module (the GUI story)

For audiences that want to see the agent fleet in a "real product"
context, the Basecamp module pair gives you a draggable QML pane.

**Pre-flight** (once):

```bash
make basecamp-lgx
# → dist/agent.lgx + dist/agent_ui.lgx (portable, no /nix/store refs)
```

**On stage**:

1. Open Basecamp.
2. Drag `dist/agent.lgx` and `dist/agent_ui.lgx` into the package manager.
3. Open the `agent_ui` pane.
4. Click **Start agent** in the Status pane.

The `agent` core module spawns `lmao agent run` as a subprocess and
talks to it through the same Unix socket as the CLI. Status, Peers,
Delegate, Audit panes all map 1:1 to the daemon IPC requests covered
in Path A.

**Say**: "Same daemon. Same protocol. The QML view is just another IPC
client." Then click **Delegate** in the UI and let the audience watch
the response stream in.

## Failure Modes — Cue Cards

If the demo breaks live, here's the order to triage:

| Symptom                             | Likely cause                                    | Action                                                                 |
|-------------------------------------|-------------------------------------------------|------------------------------------------------------------------------|
| `liblogosdelivery.so not found`     | `LIBLOGOSDELIVERY_LIB_DIR` not exported         | export it, rerun                                                       |
| `wait_for_pubkey` times out         | mesh join slow / firewalled UDP                 | retry once; if still bad, fall back to `make demo-in-memory`           |
| Goose hangs / errors out            | Ollama not running / model not pulled           | unset `LMAO_DEMO_*_EXEC`, rerun with the sed stub                      |
| `presence peers` returns empty      | gossip propagation slower than 12 s             | re-run the command — second call usually has the data                  |
| `task delegate` times out           | bob's exec is slow                              | bump `--timeout 60`; or re-narrate as "real LLMs are slow"             |
| `codex://` CID missing              | libstorage offload disabled or upload failed    | continue narrative without step 5; mention as "would normally land"    |
| Container demo: build fails on DNS  | Docker default-bridge DNS issue                 | `make demo-image` uses `--network=host` — re-run that                  |
| Basecamp: module pane is blank      | spawned subprocess can't find liblogosdelivery  | export envs *before* launching Basecamp; `LMAO_BIN` too                |

The bare-host fallback chain is `make demo` → `make demo-in-memory`
(no native deps, no network). Always know which one you're falling
back to before you start.

## Key One-Liners (steal these)

For Q&A and slack DMs after the talk:

- **What's a 'task'?** "An A2A envelope — Google's open protocol for
  agent-to-agent — wrapped in a Logos Messaging gossipsub topic."
- **Why not nwaku?** "We embed `liblogosdelivery` directly. Each agent
  is a first-class node, not a REST client of one. No HTTP server, no
  external process, no shared state."
- **Why a daemon?** "CLI commands shouldn't pay 5-second mesh-join cost
  per invocation. The long-lived `lmao agent run` *is* the daemon — IPC
  socket, JSON over length-prefixed frames, `XDG_RUNTIME_DIR/lmao.sock`."
- **Why Codex for logs?** "Content-addressed audit. The pointer the
  agent returns is *the* log, not a copy. Anyone with the CID can
  verify what the agent claims to have done."
- **What's next?** "Trust layer (`docs/TRUST.md`) — RLN for sybil
  resistance, Semaphore for community membership, EAS for capability
  attestations. Then 'small honest models on small honest hardware'
  becomes a real story."

## Reference

- `scripts/demo.sh` — the canonical bare-host script
- `scripts/demo-containerized.sh` — same narrative through Docker
- `docs/architecture.md` — diagrams + crate layout + IPC wire format
- `docs/TRUST.md` — trust-layer design proposal (out of scope for the
  demo, in scope for the post-talk hallway track)
