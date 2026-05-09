#!/usr/bin/env bash
#
# Tiny wrapper around `goose run` that gives `lmao agent run --exec`
# something to upload as the per-task audit log.
#
# `lmao agent run` treats the executor's stdout as the LMAO response
# and stderr as the "audit log" payload to ship to Logos Storage. Goose
# in --quiet mode writes nothing to stderr by design — fine for a chat
# session, useless for a content-addressed receipt. This wrapper emits
# the task input + the model output to stderr (timestamped) while still
# returning just the response on stdout.
#
# Usage (from scripts/demo.sh, --exec target):
#   ./scripts/goose-with-audit.sh
#
# Reads the task on stdin; runs goose with the user's existing config
# (~/.config/goose/config.yaml — provider/model/host); writes the
# response to stdout and the input+output transcript to stderr.

set -euo pipefail

input="$(cat)"
ts="$(date -u +%Y-%m-%dT%H:%M:%SZ)"

{
  printf '=== task input @ %s ===\n%s\n' "$ts" "$input"
} >&2

# Prefer GOOSE_BIN if set; otherwise expect `goose` on PATH.
GOOSE="${GOOSE_BIN:-goose}"
output="$(printf '%s' "$input" | "$GOOSE" run --no-session -i - --output-format text --quiet)"

ts_done="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
{
  printf '=== model output @ %s ===\n%s\n' "$ts_done" "$output"
} >&2

printf '%s\n' "$output"
