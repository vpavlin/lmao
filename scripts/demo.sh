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
ALICE_LOG="$DEMO_DIR/alice.log"
BOB_LOG="$DEMO_DIR/bob.log"
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
  local name="$1" caps="$2" tcp="$3" udp="$4" keyfile="$5" logfile="$6" exec_cmd="$7"
  echo "  starting $name (caps: $caps, tcp:$tcp udp:$udp)..."
  "$BIN" \
    --transport logos-delivery \
    --keyfile "$keyfile" \
    --tcp-port "$tcp" --udp-port "$udp" \
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
ALICE_EXEC="${LMAO_DEMO_ALICE_EXEC:-sed 's/^/[summarized] /'}"
BOB_EXEC="${LMAO_DEMO_BOB_EXEC:-sed 's/^/[reviewed]   /'}"

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
echo "[1/4] starting two agents (persistent identities)…"
run_agent_bg alice "text,summarize" 60010 9010 "$ALICE_KEYFILE" "$ALICE_LOG" "$ALICE_EXEC"
run_agent_bg bob   "code,review"    60011 9011 "$BOB_KEYFILE"   "$BOB_LOG"   "$BOB_EXEC"

echo
echo "[2/4] waiting for each to connect to logos.dev and announce…"
ALICE_PK="$(wait_for_pubkey "$ALICE_LOG" 30)"
BOB_PK="$(wait_for_pubkey "$BOB_LOG"   30)"
echo "  alice pubkey: ${ALICE_PK:0:16}…"
echo "  bob   pubkey: ${BOB_PK:0:16}…"

# Give both agents time to see the gossip mesh + complete announce/presence.
# 12s is a comfortable margin for the logos.dev fleet.
sleep 12

echo
echo "[3/4] discovering peers via presence…"
# Window must comfortably exceed agent re-announce interval (default 15s)
# AND give this freshly-spawned client time to dial the logos.dev mesh.
"$BIN" --transport logos-delivery --tcp-port 60012 --udp-port 9012 \
  presence peers --timeout 25 2>&1 | "${LOG_FILTER[@]}"

echo
echo "[4/4] delegating a task by capability=code → bob…"
"$BIN" --transport logos-delivery --tcp-port 60013 --udp-port 9013 \
  task delegate \
    --capability code \
    --text "Review this snippet: fn main() { println!(\"hello\"); }" \
    --timeout 25 2>&1 | "${LOG_FILTER[@]}"

echo
echo "═══ Demo complete ═══"
echo
echo "Agent logs: $ALICE_LOG, $BOB_LOG"
echo "Tear down: rm -rf $DEMO_DIR  (or just exit — agents stop on script exit)"
