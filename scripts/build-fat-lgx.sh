#!/usr/bin/env bash
#
# Build a "fat" agent-module LGX that bundles the lmao binary +
# liblogosdelivery.so alongside the plugin .so, so the LGX is
# self-contained when installed into the official Basecamp build.
#
# The default `nix build .#lgx-portable` only ships the plugin .so —
# the spawned `lmao agent run` and its libstorage / libwaku deps need
# to be present at runtime. agent_impl.cpp::resolveLmaoBinary() looks
# next to its own .so for the binary, and we prepend the plugin dir
# to LD_LIBRARY_PATH so liblogosdelivery.so resolves automatically.
# This script just stages those extra files into the LGX archive.
#
# Inputs (env vars, with sensible defaults):
#   LMAO_BIN_PATH            release lmao binary
#                            (default: target/release/logos-messaging-a2a)
#   LIBLOGOSDELIVERY_PATH    liblogosdelivery.so
#                            (default: $LIBLOGOSDELIVERY_LIB_DIR/liblogosdelivery.so
#                             or ../logos-delivery/build/liblogosdelivery.so)
#
# Output:
#   basecamp/agent-module/result-fat/logos-agent-module-lib.lgx
#
# Usage:
#   cd /path/to/lmao
#   ./scripts/build-fat-lgx.sh
#   # then install with lgpm against the official Basecamp dirs.

set -euo pipefail

REPO="$(cd "$(dirname "$0")/.." && pwd)"
MOD_DIR="$REPO/basecamp/agent-module"

LMAO_BIN_PATH="${LMAO_BIN_PATH:-$REPO/target/release/logos-messaging-a2a}"
if [[ -z "${LIBLOGOSDELIVERY_PATH:-}" ]]; then
    if [[ -n "${LIBLOGOSDELIVERY_LIB_DIR:-}" ]]; then
        LIBLOGOSDELIVERY_PATH="$LIBLOGOSDELIVERY_LIB_DIR/liblogosdelivery.so"
    else
        # Common workspace layout: …/logos-messaging/logos-delivery/build/.
        guess="$(realpath -m "$REPO/../logos-delivery/build/liblogosdelivery.so" 2>/dev/null || true)"
        if [[ -f "$guess" ]]; then
            LIBLOGOSDELIVERY_PATH="$guess"
        fi
    fi
fi

if [[ ! -f "$LMAO_BIN_PATH" ]]; then
    echo "fat-lgx: lmao binary not found at $LMAO_BIN_PATH" >&2
    echo "  set LMAO_BIN_PATH or run \`cargo build --release -p logos-messaging-a2a-cli --features logos-delivery,libstorage\`" >&2
    exit 1
fi
if [[ -z "${LIBLOGOSDELIVERY_PATH:-}" || ! -f "$LIBLOGOSDELIVERY_PATH" ]]; then
    echo "fat-lgx: liblogosdelivery.so not found" >&2
    echo "  set LIBLOGOSDELIVERY_PATH (or LIBLOGOSDELIVERY_LIB_DIR) to a built copy" >&2
    exit 1
fi

# Build the standard portable LGX first. Use a separate result symlink
# so we don't clobber whatever the user has at result/.
echo "fat-lgx: building plain portable LGX…"
( cd "$MOD_DIR" && nix build .#lgx-portable -o result-portable )
SRC_LGX="$(ls "$MOD_DIR"/result-portable/*.lgx | head -n1)"
if [[ -z "$SRC_LGX" || ! -f "$SRC_LGX" ]]; then
    echo "fat-lgx: nix build did not produce an LGX file" >&2
    exit 1
fi
echo "fat-lgx: source LGX = $SRC_LGX"

# Tar-injecting files leaves the manifest's per-variant hashes stale
# and Basecamp's package manager rejects with a "signature error".
# Use the `lgx` CLI to swap in a directory variant — it recomputes the
# hash tree as part of `lgx add`.
LGX_BIN="${LGX_BIN:-$HOME/bin/lgx}"
if ! [[ -x "$LGX_BIN" ]]; then
    LGX_BIN="$(command -v lgx 2>/dev/null || true)"
fi
if [[ -z "$LGX_BIN" ]]; then
    echo "fat-lgx: \`lgx\` CLI not found; install logos-co/lgx-cli or set \$LGX_BIN" >&2
    exit 1
fi

WORK="$(mktemp -d -t fat-lgx.XXXXXX)"
trap 'rm -rf "$WORK"' EXIT

# Stage everything that should live alongside agent_plugin.so in the
# variant directory: the plugin itself (extracted from the source LGX)
# plus the lmao binary and liblogosdelivery.so.
STAGE="$WORK/stage"
mkdir -p "$STAGE"
"$LGX_BIN" extract "$SRC_LGX" --variant linux-amd64 --output "$WORK/extracted" >/dev/null 2>&1 \
    || tar -xzf "$SRC_LGX" -C "$WORK/extracted-fallback"
# `lgx extract` lays files directly in --output; otherwise dig the
# variant out of the tar fallback.
PLUGIN_SO="$(find "$WORK" -name 'agent_plugin.so' | head -n1)"
if [[ -z "$PLUGIN_SO" ]]; then
    echo "fat-lgx: agent_plugin.so not found inside source LGX" >&2
    exit 1
fi
cp -f  "$PLUGIN_SO"             "$STAGE/agent_plugin.so"
cp -f  "$LMAO_BIN_PATH"         "$STAGE/lmao"
chmod +x                        "$STAGE/lmao"
cp -f  "$LIBLOGOSDELIVERY_PATH" "$STAGE/liblogosdelivery.so"

# Make a writable copy of the source LGX, then swap its linux-amd64
# variant for our staged directory. `lgx add` recomputes the manifest
# hashes so Basecamp's verifier accepts it.
OUT_DIR="$MOD_DIR/result-fat"
mkdir -p "$OUT_DIR"
OUT_LGX="$OUT_DIR/$(basename "$SRC_LGX")"
cp -f "$SRC_LGX" "$OUT_LGX"
chmod u+w "$OUT_LGX"
"$LGX_BIN" add "$OUT_LGX" \
    --variant linux-amd64 \
    --files "$STAGE" \
    --main agent_plugin.so \
    --yes

echo
echo "fat-lgx: wrote $OUT_LGX"
ls -lh "$OUT_LGX"
echo
echo "Install (against official Basecamp's dirs — adjust paths if yours differ):"
echo "  lgpm --modules-dir ~/.local/share/Logos/LogosBasecamp/modules \\"
echo "       install --file '$OUT_LGX'"
echo
echo "agent-ui LGX is unaffected — install it the usual way:"
echo "  nix build .#lgx-portable in basecamp/agent-ui, then lgpm install."
