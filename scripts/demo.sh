#!/usr/bin/env bash
#
# LMAO end-to-end demo on the real logos.dev Logos Messaging fleet.
#
# Brings up THREE persistent-identity agents in the background:
#   - alice    (text, summarize) — trusts bob
#   - bob      (code, review)    — trusts alice
#   - charlie  (code, review)    — UNTRUSTED, advertises the same caps as bob
#
# Then runs a delegating client (alice) that picks a peer by capability.
# Both bob and charlie advertise `code` on the mesh; without the trust
# filter, alice's CapabilityMatch would pick whoever announces first.
# With the filter on, charlie is skipped — that's the demo. The first
# two agents share the original "discover, delegate, audit" arc; charlie
# is the live counter-example proving the filter is doing real work.
#
# Prerequisites:
#   - liblogosdelivery built and on disk; export LIBLOGOSDELIVERY_LIB_DIR
#   - logos-messaging-a2a CLI built with --features logos-delivery
#
# Usage:
#   make demo               # picks sensible defaults
#   ./scripts/demo.sh       # same
#   ./scripts/demo.sh -v    # verbose: keep libp2p logs visible

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DEMO_DIR="${LMAO_DEMO_DIR:-$ROOT_DIR/.demo}"
BIN="${LMAO_BIN:-$ROOT_DIR/target/release/logos-messaging-a2a}"
ALICE_KEYFILE="$DEMO_DIR/alice.key"
BOB_KEYFILE="$DEMO_DIR/bob.key"
CHARLIE_KEYFILE="$DEMO_DIR/charlie.key"
ALICE_TRUST="$DEMO_DIR/alice-trust.toml"
BOB_TRUST="$DEMO_DIR/bob-trust.toml"
CHARLIE_TRUST="$DEMO_DIR/charlie-trust.toml"
ALICE_LOG="$DEMO_DIR/alice.log"
BOB_LOG="$DEMO_DIR/bob.log"
CHARLIE_LOG="$DEMO_DIR/charlie.log"
ALICE_SOCKET="$DEMO_DIR/alice.sock"
BOB_SOCKET="$DEMO_DIR/bob.sock"
CHARLIE_SOCKET="$DEMO_DIR/charlie.sock"
PIDFILE="$DEMO_DIR/agents.pid"

VERBOSE=0
for arg in "$@"; do
  case "$arg" in
    -v|--verbose) VERBOSE=1 ;;
    -h|--help)
      sed -n '2,/^$/p' "$0" | sed 's/^# \?//'
      exit 0
      ;;
  esac
done

if [[ -z "${LIBLOGOSDELIVERY_LIB_DIR:-}" ]]; then
  echo "error: LIBLOGOSDELIVERY_LIB_DIR is unset" >&2
  echo "       export it to the dir containing liblogosdelivery.so" >&2
  exit 1
fi
export LD_LIBRARY_PATH="$LIBLOGOSDELIVERY_LIB_DIR:${LD_LIBRARY_PATH:-}"

if [[ ! -x "$BIN" ]]; then
  echo "error: CLI binary not found at $BIN" >&2
  echo "       build it with:" >&2
  echo "       cargo build --release -p logos-messaging-a2a-cli --features logos-delivery" >&2
  exit 1
fi

mkdir -p "$DEMO_DIR"

# Tear down any previous demo run cleanly. We have to chase down two
# kinds of orphan: the agent itself (PIDFILE), AND any --exec subprocess
# the agent had spawned that hasn't exited yet (e.g. a long-running
# `goose run` waiting on an LLM). The exec subprocesses inherit the
# agent's libp2p sockets, so leaving them alive blocks the next demo
# run with `Address already in use` on UDP 9010-9012.
cleanup() {
  if [[ -f "$PIDFILE" ]]; then
    while read -r pid; do
      [[ -n "$pid" ]] && kill "$pid" 2>/dev/null || true
    done <"$PIDFILE"
    rm -f "$PIDFILE"
  fi
  # Catch orphan exec children that inherited the agent's UDP socket.
  pkill -9 -f "goose-with-audit\\.sh"     2>/dev/null || true
  pkill -9 -f "goose run --no-session"    2>/dev/null || true
}
trap cleanup EXIT

# Quiet log filter: drop the libp2p / nim-waku noise unless -v was given.
if [[ "$VERBOSE" -eq 1 ]]; then
  LOG_FILTER=(cat)
else
  # Remove DBG/INF/WRN/NTC/ERR lines from the embedded node — they're useful
  # for debugging but drown the demo narrative.
  LOG_FILTER=(grep -vE '^(DBG|INF|WRN|NTC|ERR|TRC) ')
fi

run_agent_bg() {
  local name="$1" caps="$2" tcp="$3" udp="$4" sport="$5" sock="$6" keyfile="$7" trustfile="$8" logfile="$9" exec_cmd="${10}"
  echo "  starting $name (caps: $caps, tcp:$tcp udp:$udp storage:$sport sock:$sock trust:$(basename "$trustfile"))..."
  "$BIN" \
    --transport logos-delivery \
    --storage libstorage \
    --storage-data-dir "$DEMO_DIR/storage-$name" \
    --storage-port "$sport" \
    --keyfile "$keyfile" \
    --trust-file "$trustfile" \
    --tcp-port "$tcp" --udp-port "$udp" \
    --daemon-socket "$sock" \
    agent run --name "$name" --capabilities "$caps" --exec "$exec_cmd" \
    >"$logfile" 2>&1 &
  echo "$!" >>"$PIDFILE"
}

# Default executors: simple sed-based stubs that prefix the task text so
# different agents produce visibly different responses without needing a
# model running. Swap in a real coding agent (Goose against a local
# OpenAI-compatible inference endpoint, lemonade, vLLM, Ollama, etc.) by
# exporting paths to the bundled wrapper:
#
#   export LMAO_DEMO_ALICE_EXEC="$(pwd)/scripts/goose-with-audit.sh"
#   export LMAO_DEMO_BOB_EXEC="$(pwd)/scripts/goose-with-audit.sh"
#
# `goose-with-audit.sh` runs goose with the user's existing
# `~/.config/goose/config.yaml` (provider/model/host) and writes the
# task input + model output to stderr so `lmao agent run` has something
# real to upload as the per-task audit log. (Goose --quiet writes
# nothing to stderr by design.) Set GOOSE_BIN if `goose` isn't on PATH.
ALICE_EXEC="${LMAO_DEMO_ALICE_EXEC:-sh -c 'echo summarizer-stderr-line >&2; sed s/^/[summarized]\ /'}"
BOB_EXEC="${LMAO_DEMO_BOB_EXEC:-sh -c 'echo reviewer-stderr-line >&2; sed s/^/[reviewed]\ \ \ /'}"
# charlie is the untrusted peer; alice never delegates to him so his
# exec output is irrelevant. The visible-prefix stub makes it obvious in
# logs if alice ever DOES route to him by mistake (regression).
CHARLIE_EXEC="${LMAO_DEMO_CHARLIE_EXEC:-sh -c 'echo charlie-stderr-line >&2; sed s/^/[CHARLIE-RAN-THIS-FILTER-FAILED]\ /'}"

wait_for_pubkey() {
  local logfile="$1" deadline="$2"
  local start=$(date +%s)
  while :; do
    if [[ -f "$logfile" ]] && grep -q "^Pubkey: " "$logfile"; then
      grep "^Pubkey: " "$logfile" | head -1 | awk '{print $2}'
      return 0
    fi
    if (( $(date +%s) - start > deadline )); then
      echo "" >&2
      echo "error: agent didn't print Pubkey within ${deadline}s. Last log lines:" >&2
      tail -20 "$logfile" >&2 || true
      return 1
    fi
    sleep 0.5
  done
}

: >"$PIDFILE"

echo
echo "═══ LMAO demo on logos.dev ═══"
echo

# Pre-derive each agent's pubkey from its keyfile (creating the keyfile
# if missing). `lmao trust pubkey` runs over an in-process InMemory
# transport — no liblogosdelivery, no mesh-join cost, ~10 ms per call.
echo "[1/5] preparing identities + friend-keyring trust lists…"
ALICE_PK="$(  "$BIN" --keyfile "$ALICE_KEYFILE"   trust pubkey)"
BOB_PK="$(    "$BIN" --keyfile "$BOB_KEYFILE"     trust pubkey)"
CHARLIE_PK="$("$BIN" --keyfile "$CHARLIE_KEYFILE" trust pubkey)"

# Fresh trust files for this demo run.
#   alice ↔ bob: mutual trust (the friends).
#   charlie:    no entries, mode=off — willing to talk to anyone, but
#               nobody specifically vouches for him. The agent will
#               still accept tasks (mode=off doesn't filter), which is
#               fine; we just need him visible on the mesh and
#               advertising code,review.
# `trust add` flips Off → Enforce on the first entry, so by the time
# alice and bob start, the filter is live.
rm -f "$ALICE_TRUST" "$BOB_TRUST" "$CHARLIE_TRUST"
"$BIN" --trust-file "$ALICE_TRUST" trust add "$BOB_PK"   --nickname bob   --cap code --cap review    >/dev/null 2>&1
"$BIN" --trust-file "$BOB_TRUST"   trust add "$ALICE_PK" --nickname alice --cap text --cap summarize >/dev/null 2>&1
# charlie's trust file: empty + mode=off, written via export.
printf 'mode = "off"\n' > "$CHARLIE_TRUST"

echo "  alice   (${ALICE_PK:0:16}…) trusts bob   for code,review"
echo "  bob     (${BOB_PK:0:16}…) trusts alice for text,summarize"
echo "  charlie (${CHARLIE_PK:0:16}…) on the mesh, NOT in alice's or bob's list"
echo "                                  (advertises code,review — same caps as bob)"

echo
echo "[2/5] starting three agents on logos.dev (persistent identities + Codex + IPC + trust filter)…"
run_agent_bg alice   "text,summarize" 60010 9010 19200 "$ALICE_SOCKET"   "$ALICE_KEYFILE"   "$ALICE_TRUST"   "$ALICE_LOG"   "$ALICE_EXEC"
run_agent_bg bob     "code,review"    60011 9011 19201 "$BOB_SOCKET"     "$BOB_KEYFILE"     "$BOB_TRUST"     "$BOB_LOG"     "$BOB_EXEC"
run_agent_bg charlie "code,review"    60012 9012 19202 "$CHARLIE_SOCKET" "$CHARLIE_KEYFILE" "$CHARLIE_TRUST" "$CHARLIE_LOG" "$CHARLIE_EXEC"

echo "  waiting for each to connect to logos.dev…"
ALICE_PK_LOGGED="$(  wait_for_pubkey "$ALICE_LOG"   30)"
BOB_PK_LOGGED="$(    wait_for_pubkey "$BOB_LOG"     30)"
CHARLIE_PK_LOGGED="$(wait_for_pubkey "$CHARLIE_LOG" 30)"
# Sanity: the keyfile-derived pubkey we used to seed the trust file
# matches the one the actual agent advertises. If these diverge, the
# trust filter would silently drop legitimate traffic.
if [[ "$ALICE_PK_LOGGED" != "$ALICE_PK" || "$BOB_PK_LOGGED" != "$BOB_PK" || "$CHARLIE_PK_LOGGED" != "$CHARLIE_PK" ]]; then
  echo "error: keyfile-derived pubkey doesn't match agent-runtime pubkey" >&2
  echo "       alice:   derived=${ALICE_PK:0:16}…   runtime=${ALICE_PK_LOGGED:0:16}…"   >&2
  echo "       bob:     derived=${BOB_PK:0:16}…   runtime=${BOB_PK_LOGGED:0:16}…"     >&2
  echo "       charlie: derived=${CHARLIE_PK:0:16}… runtime=${CHARLIE_PK_LOGGED:0:16}…" >&2
  exit 1
fi

# Give all agents time to see the gossip mesh + complete announce/presence.
# 12s is a comfortable margin for the logos.dev fleet with three peers.
sleep 12

echo
echo "[3/5] discovering peers via presence (through alice's daemon — no new node)…"
# We talk to alice's already-running node over IPC instead of spinning
# up a fresh logos-delivery client for every CLI invocation. This
# collapses 20+ seconds of mesh-join into a sub-millisecond Unix
# socket round-trip. Demo-friendly.
"$BIN" --daemon-socket "$ALICE_SOCKET" \
  presence peers --timeout 5 2>&1 | "${LOG_FILTER[@]}"
echo "  ↑ alice sees both bob and charlie. Both advertise code,review."
echo "    Without the trust filter, alice's next CapabilityMatch would be a coin flip."

echo
echo "[4/5] alice delegates a code-review task — the filter routes only to bob…"
echo "  candidates (from peers): bob, charlie  ← both advertise 'code'"
echo "  ∩ alice's trust list:    {bob}         ← charlie is NOT in alice's list"
echo "  → CapabilityMatch picks bob; charlie is skipped despite matching caps."
DELEGATE_OUT="$DEMO_DIR/delegate.out"
"$BIN" --daemon-socket "$ALICE_SOCKET" \
  task delegate \
    --capability code \
    --text "Review this snippet: fn main() { println!(\"hello\"); }" \
    --timeout "${LMAO_DEMO_DELEGATE_TIMEOUT:-90}" 2>&1 | "${LOG_FILTER[@]}" | tee "$DELEGATE_OUT"

# Pull the codex CID from the response and fetch the audit log from
# bob's daemon — the same blockstore that produced it. Closes the loop
# on the "verifiable agent action" story: the response's pointer is
# retrievable, in this same demo, with one CLI call.
CID="$(grep -oE 'codex://[A-Za-z0-9]+' "$DELEGATE_OUT" | head -1 | sed 's|codex://||')"
if [[ -n "$CID" ]]; then
  echo
  echo "[5/5] fetching bob's execution log by CID via bob's daemon…"
  echo "  cid: $CID"
  "$BIN" --daemon-socket "$BOB_SOCKET" storage fetch "$CID" 2>&1 \
    | "${LOG_FILTER[@]}" | sed 's/^/  /'
else
  echo
  echo "[5/5] (skipped: no CID found in delegation response)"
fi

echo
echo "═══ Demo complete ═══"
echo
echo "Agent logs: $ALICE_LOG, $BOB_LOG, $CHARLIE_LOG"
echo "Charlie's log shouldn't show any '[CHARLIE-RAN-THIS-FILTER-FAILED]' lines —"
echo "if it does, the trust filter let him through and the demo regressed."
echo "Tear down: rm -rf $DEMO_DIR  (or just exit — agents stop on script exit)"
