#!/usr/bin/env bash
#
# LMAO end-to-end demo on the real logos.dev Logos Messaging fleet.
#
# Brings up two persistent-identity agents in the background, then runs
# a delegating client that picks the right peer by capability, sends a
# task, and prints the response. No Docker, no nwaku, no servers — both
# agents are first-class peers on logos.dev via liblogosdelivery.
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
ALICE_TRUST="$DEMO_DIR/alice-trust.toml"
BOB_TRUST="$DEMO_DIR/bob-trust.toml"
ALICE_LOG="$DEMO_DIR/alice.log"
BOB_LOG="$DEMO_DIR/bob.log"
ALICE_SOCKET="$DEMO_DIR/alice.sock"
BOB_SOCKET="$DEMO_DIR/bob.sock"
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

# Tear down any previous demo run cleanly.
cleanup() {
  if [[ -f "$PIDFILE" ]]; then
    while read -r pid; do
      [[ -n "$pid" ]] && kill "$pid" 2>/dev/null || true
    done <"$PIDFILE"
    rm -f "$PIDFILE"
  fi
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
# different agents produce visibly different responses. Swap in a real
# coding agent (e.g. Goose against a local Ollama model) by exporting:
#
#   export LMAO_DEMO_ALICE_EXEC='goose run --no-session -i - --output-format text --quiet'
#   export LMAO_DEMO_BOB_EXEC='goose run --no-session -i - --output-format text --quiet'
#
# These recipes assume `~/.config/goose/config.yaml` is configured for an
# Ollama provider and a model that's already pulled locally.
ALICE_EXEC="${LMAO_DEMO_ALICE_EXEC:-sh -c 'echo summarizer-stderr-line >&2; sed s/^/[summarized]\ /'}"
BOB_EXEC="${LMAO_DEMO_BOB_EXEC:-sh -c 'echo reviewer-stderr-line >&2; sed s/^/[reviewed]\ \ \ /'}"

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
ALICE_PK="$("$BIN" --keyfile "$ALICE_KEYFILE" trust pubkey)"
BOB_PK="$(  "$BIN" --keyfile "$BOB_KEYFILE"   trust pubkey)"

# Fresh trust files for this demo run. Each agent trusts the other for
# the capabilities the other actually advertises. `trust add` flips Off
# → Enforce on the first entry, so by the time the agent starts the
# filter is live.
rm -f "$ALICE_TRUST" "$BOB_TRUST"
"$BIN" --trust-file "$ALICE_TRUST" trust add "$BOB_PK"   --nickname bob   --cap code --cap review    >/dev/null 2>&1
"$BIN" --trust-file "$BOB_TRUST"   trust add "$ALICE_PK" --nickname alice --cap text --cap summarize >/dev/null 2>&1
echo "  alice (${ALICE_PK:0:16}…) trusts bob   for code,review"
echo "  bob   (${BOB_PK:0:16}…) trusts alice for text,summarize"

echo
echo "[2/5] starting two agents (persistent identities + Codex + IPC sockets + trust filter)…"
run_agent_bg alice "text,summarize" 60010 9010 19200 "$ALICE_SOCKET" "$ALICE_KEYFILE" "$ALICE_TRUST" "$ALICE_LOG" "$ALICE_EXEC"
run_agent_bg bob   "code,review"    60011 9011 19201 "$BOB_SOCKET"   "$BOB_KEYFILE"   "$BOB_TRUST"   "$BOB_LOG"   "$BOB_EXEC"

echo "  waiting for each to connect to logos.dev…"
ALICE_PK_LOGGED="$(wait_for_pubkey "$ALICE_LOG" 30)"
BOB_PK_LOGGED="$(  wait_for_pubkey "$BOB_LOG"   30)"
# Sanity: the keyfile-derived pubkey we used to seed the trust file
# matches the one the actual agent advertises. If these diverge, the
# trust filter would silently drop legitimate traffic.
if [[ "$ALICE_PK_LOGGED" != "$ALICE_PK" || "$BOB_PK_LOGGED" != "$BOB_PK" ]]; then
  echo "error: keyfile-derived pubkey doesn't match agent-runtime pubkey" >&2
  echo "       alice: derived=${ALICE_PK:0:16}… runtime=${ALICE_PK_LOGGED:0:16}…" >&2
  echo "       bob:   derived=${BOB_PK:0:16}… runtime=${BOB_PK_LOGGED:0:16}…"   >&2
  exit 1
fi

# Give both agents time to see the gossip mesh + complete announce/presence.
# 12s is a comfortable margin for the logos.dev fleet.
sleep 12

echo
echo "[3/5] discovering peers via presence (through alice's daemon — no new node)…"
# We talk to alice's already-running node over IPC instead of spinning
# up a fresh logos-delivery client for every CLI invocation. This
# collapses 20+ seconds of mesh-join into a sub-millisecond Unix
# socket round-trip. Demo-friendly.
"$BIN" --daemon-socket "$ALICE_SOCKET" \
  presence peers --timeout 5 2>&1 | "${LOG_FILTER[@]}"

echo
echo "[4/5] delegating a task by capability=code → bob (via alice's daemon)…"
echo "  alice's CapabilityMatch picks from peers ∩ trust list — bob qualifies;"
echo "  any stranger advertising 'code' on the gossip mesh would be filtered out."
DELEGATE_OUT="$DEMO_DIR/delegate.out"
"$BIN" --daemon-socket "$ALICE_SOCKET" \
  task delegate \
    --capability code \
    --text "Review this snippet: fn main() { println!(\"hello\"); }" \
    --timeout 25 2>&1 | "${LOG_FILTER[@]}" | tee "$DELEGATE_OUT"

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
echo "Agent logs: $ALICE_LOG, $BOB_LOG"
echo "Tear down: rm -rf $DEMO_DIR  (or just exit — agents stop on script exit)"
