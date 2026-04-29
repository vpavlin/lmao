#!/usr/bin/env bash
#
# Container-per-agent variant of the LMAO demo.
#
# - Builds (or reuses) the `lmao:dev` image
# - Brings up alice + bob as separate containers via docker compose
# - Drives the same five-step narrative from the host, talking to each
#   container's daemon socket via the shared `demo-data/*-sock/` volume
#
# Each agent runs as non-root, with a fresh per-container filesystem,
# a separate UID namespace, no host filesystem access except its own
# data volume. Goose (when used as --exec) lives entirely inside the
# container — host SSH keys, ~/.aws, /etc, etc. are unreachable.
#
# Prereqs:
#   - docker + docker compose
#   - bare-host `lmao` binary built (we use it as the IPC client):
#     `make cli-logos-delivery`
#     (this needs LIBLOGOSDELIVERY_LIB_DIR set; export it before invoking)
#
# Usage:
#   make demo-containerized
#   ./scripts/demo-containerized.sh
#   ./scripts/demo-containerized.sh --rebuild        # force rebuild image
#   ./scripts/demo-containerized.sh --keep-running   # don't tear down at end

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

DEMO_DATA="$ROOT_DIR/demo-data"
ALICE_SOCKET="$DEMO_DATA/alice-sock/lmao.sock"
BOB_SOCKET="$DEMO_DATA/bob-sock/lmao.sock"
BIN="${LMAO_BIN:-$ROOT_DIR/target/release/logos-messaging-a2a}"

REBUILD=0
KEEP=0
for arg in "$@"; do
  case "$arg" in
    --rebuild)      REBUILD=1 ;;
    --keep-running) KEEP=1    ;;
    -h|--help)
      sed -n '2,/^$/p' "$0" | sed 's/^# \?//'
      exit 0
      ;;
  esac
done

if [[ ! -x "$BIN" ]]; then
  echo "error: bare-host CLI not found at $BIN" >&2
  echo "       build it first: LIBLOGOSDELIVERY_LIB_DIR=... make cli-logos-delivery" >&2
  exit 1
fi
if [[ -z "${LIBLOGOSDELIVERY_LIB_DIR:-}" ]]; then
  echo "warning: LIBLOGOSDELIVERY_LIB_DIR not set; the host-side CLI may fail" >&2
  echo "         to load liblogosdelivery.so. (Setting LD_LIBRARY_PATH so the" >&2
  echo "         IPC-only commands still work — they don't need the transport.)" >&2
fi
export LD_LIBRARY_PATH="${LIBLOGOSDELIVERY_LIB_DIR:-/nonexistent}:${LD_LIBRARY_PATH:-}"

# `host` UID 1000 needs to own the volume mount points, otherwise the
# in-container `lmao` user (uid 1000) can't write.
mkdir -p "$DEMO_DATA"/{alice,bob,alice-sock,bob-sock}

teardown() {
  echo
  echo "[cleanup] tearing down containers…"
  docker compose down --remove-orphans 2>&1 | sed 's/^/  /'
}
if (( ! KEEP )); then
  trap teardown EXIT
fi

echo
echo "═══ LMAO demo — container-per-agent on logos.dev ═══"
echo

if (( REBUILD )); then
  echo "[0/5] (re)building lmao:dev image…"
  docker compose build --no-cache 2>&1 | tail -5
elif ! docker image inspect lmao:dev >/dev/null 2>&1; then
  echo "[0/5] building lmao:dev image (first run, ~15 min)…"
  docker compose build 2>&1 | tail -5
else
  echo "[0/5] using existing lmao:dev image (--rebuild to force)"
fi

echo
echo "[1/5] bringing up alice + bob containers (non-root, isolated FS)…"
docker compose up -d 2>&1 | sed 's/^/  /'

echo
echo "[2/5] waiting for both daemon sockets to appear (each agent dials"
echo "      logos.dev from inside its container — ~15-25s)…"
for sock in "$ALICE_SOCKET" "$BOB_SOCKET"; do
  for i in $(seq 1 60); do
    if [[ -S "$sock" ]]; then break; fi
    sleep 1
  done
  if [[ ! -S "$sock" ]]; then
    echo "ERROR: $sock never appeared" >&2
    docker compose logs --tail=30 2>&1 | sed 's/^/  /'
    exit 1
  fi
done
echo "  alice socket: $ALICE_SOCKET"
echo "  bob socket:   $BOB_SOCKET"

# Initial presence broadcast happens at agent startup; give the network
# one re-announce window so the freshly-spawned peer's daemon has bob's
# announcement (and vice versa) in its PeerMap.
sleep 18

# All daemon-socket calls go through the host-side `lmao` (not the
# in-container one). Stderr from the host CLI is clean; the noisy
# libp2p / nim-waku DBG/INF stream comes from inside the containers
# and never reaches us, so no log filter needed here.
echo
echo "[3/5] daemon status for alice (via host → container socket)…"
"$BIN" --daemon-socket "$ALICE_SOCKET" daemon status 2>&1 | sed 's/^/  /'

echo
echo "[4/5] presence peers via alice's daemon…"
"$BIN" --daemon-socket "$ALICE_SOCKET" presence peers --timeout 5 2>&1 | sed 's/^/  /'

echo
echo "[5/5] task delegate by capability=code → bob (via alice's daemon)…"
DELEGATE_OUT="$DEMO_DATA/delegate.out"
"$BIN" --daemon-socket "$ALICE_SOCKET" \
  task delegate \
    --capability code \
    --text "Review this snippet: fn main() { println!(\"hello\"); }" \
    --timeout 25 2>&1 | tee "$DELEGATE_OUT" | sed 's/^/  /'

CID="$(grep -oE 'codex://[A-Za-z0-9]+' "$DELEGATE_OUT" | head -1 | sed 's|codex://||' || true)"
if [[ -n "$CID" ]]; then
  echo
  echo "[5b]  fetching the audit log via bob's daemon (host → container)…"
  echo "      cid: $CID"
  "$BIN" --daemon-socket "$BOB_SOCKET" storage fetch "$CID" 2>&1 | sed 's/^/        /'
else
  echo
  echo "[5b]  (skipped: no CID in delegation response)"
fi

echo
echo "═══ Demo complete ═══"
echo
echo "Containers are still up. Inspect with:"
echo "  docker compose ps"
echo "  docker compose logs alice"
echo "  docker compose logs bob"
echo
if (( KEEP )); then
  echo "Tear down when done:    docker compose down"
else
  echo "(Will tear down on script exit. Use --keep-running to leave them up.)"
fi
