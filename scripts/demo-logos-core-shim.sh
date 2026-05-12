#!/usr/bin/env bash
# demo-logos-core-shim.sh — logos-core-native two-agent demo
#
# Runs two lmao agents that share Basecamp's delivery_module and
# storage_module instead of embedding their own Waku/Codex nodes.
# Requires logoscore (or Basecamp) with delivery_module and
# storage_module loaded and LOGOS_INSTANCE_ID set in the environment.
#
# Usage:
#   LOGOS_CPP_SDK_DIR=/path/to/logos-cpp-sdk make demo-logos-core-shim
#
# Or manually:
#   export LOGOS_CPP_SDK_DIR=/path/to/logos-cpp-sdk
#   export LOGOS_INSTANCE_ID=<from logoscore>        # set by logoscore automatically
#   export LMAO_AGENT_DELIVERY_CFG='{"logLevel":"WARN","mode":"Core","preset":"logos.dev"}'
#   ./scripts/demo-logos-core-shim.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(dirname "$SCRIPT_DIR")"

# ── Build ────────────────────────────────────────────────────────────────────

if [[ -z "${LOGOS_CPP_SDK_DIR:-}" ]]; then
    echo "error: LOGOS_CPP_SDK_DIR is not set" >&2
    echo "       Point it at a logos-cpp-sdk checkout and re-run." >&2
    exit 1
fi

echo "[demo] Building lmao with shim feature..."
LOGOS_CPP_SDK_DIR="$LOGOS_CPP_SDK_DIR" \
    cargo build --release \
    -p logos-messaging-a2a-cli \
    --no-default-features --features shim,rest \
    --manifest-path "$REPO_ROOT/Cargo.toml"

LMAO="$REPO_ROOT/target/release/logos-messaging-a2a"

# ── Runtime checks ───────────────────────────────────────────────────────────

if [[ -z "${LOGOS_INSTANCE_ID:-}" ]]; then
    echo "error: LOGOS_INSTANCE_ID is not set" >&2
    echo "       Start logoscore with delivery_module and storage_module," >&2
    echo "       then source its environment or set LOGOS_INSTANCE_ID manually." >&2
    exit 1
fi

# ── Socket paths ─────────────────────────────────────────────────────────────

ALICE_SOCK="${XDG_RUNTIME_DIR:-/tmp}/lmao-demo-alice.sock"
BOB_SOCK="${XDG_RUNTIME_DIR:-/tmp}/lmao-demo-bob.sock"
rm -f "$ALICE_SOCK" "$BOB_SOCK"

# ── Agent startup ────────────────────────────────────────────────────────────

echo "[demo] Starting Alice (shim mode)..."
"$LMAO" \
    --transport delivery-module \
    --storage storage-module \
    ${LMAO_AGENT_DELIVERY_CFG:+--delivery-module-cfg "$LMAO_AGENT_DELIVERY_CFG"} \
    --daemon-socket "$ALICE_SOCK" \
    agent run --name alice --capabilities text \
    --exec "sed s/^/[alice]\\ /" &
ALICE_PID=$!

echo "[demo] Starting Bob (shim mode)..."
"$LMAO" \
    --transport delivery-module \
    --storage storage-module \
    ${LMAO_AGENT_DELIVERY_CFG:+--delivery-module-cfg "$LMAO_AGENT_DELIVERY_CFG"} \
    --daemon-socket "$BOB_SOCK" \
    agent run --name bob --capabilities text \
    --exec "sed s/^/[bob]\\ /" &
BOB_PID=$!

cleanup() {
    echo "[demo] Stopping agents..."
    kill "$ALICE_PID" "$BOB_PID" 2>/dev/null || true
    wait "$ALICE_PID" "$BOB_PID" 2>/dev/null || true
    rm -f "$ALICE_SOCK" "$BOB_SOCK"
}
trap cleanup EXIT

# ── Wait for sockets ─────────────────────────────────────────────────────────

echo "[demo] Waiting for agents to connect to delivery_module..."
for sock in "$ALICE_SOCK" "$BOB_SOCK"; do
    deadline=$(( $(date +%s) + 120 ))
    while [[ ! -S "$sock" ]]; do
        if (( $(date +%s) > deadline )); then
            echo "error: timed out waiting for $sock" >&2
            exit 1
        fi
        sleep 1
    done
done
sleep 2  # brief settle

# ── Discovery + task exchange ─────────────────────────────────────────────────

echo "[demo] Alice discovering peers..."
BOB_PUBKEY=$("$LMAO" --daemon-socket "$BOB_SOCK" info | jq -r '.pubkey')
echo "[demo] Bob's pubkey: $BOB_PUBKEY"

echo "[demo] Alice sending task to Bob..."
"$LMAO" \
    --daemon-socket "$ALICE_SOCK" \
    task send --to "$BOB_PUBKEY" --text "Hello from Alice via logos-core shim!"

echo "[demo] Done. Both agents used delivery_module + storage_module."
echo "       No embedded Waku or Codex nodes were started."
