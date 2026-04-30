#!/usr/bin/env bash
#
# Summarizer executor — talks to a lemonade-server (or any OpenAI-
# compatible endpoint) directly with curl, bypassing Goose. Used as the
# `--exec` for an `lmao agent run` peer that advertises the "summarize"
# capability.
#
# We bypass Goose because some smaller models (e.g. Qwen3.5-4B-GGUF) are
# loaded by lemonade with `preserve_thinking: true` baked into the chat
# template, and the resulting `reasoning_content` SSE deltas confuse
# Goose's OpenAI client (it sees no `content` and reports an empty
# response). Disabling thinking via `chat_template_kwargs` per request
# isn't reachable through Goose, so we drive the model directly here.
#
# Stdin: the task text (anything — link, snippet, paragraph).
# Stdout: the model's summary, plain text.
# Stderr: timestamped task input + model output for the audit log
#         (uploaded to Logos Storage by `lmao agent run`).
#
# Env:
#   LEMONADE_ENDPOINT  default http://192.168.0.125:3000
#   LEMONADE_MODEL     default user.Qwen3.5-4B-GGUF
#   LEMONADE_TIMEOUT   default 180 (seconds)
#   LEMONADE_MAX_TOKENS default 800

set -euo pipefail

input="$(cat)"
ts="$(date -u +%Y-%m-%dT%H:%M:%SZ)"

ENDPOINT="${LEMONADE_ENDPOINT:-http://192.168.0.125:3000}"
MODEL="${LEMONADE_MODEL:-user.Qwen3.5-4B-GGUF}"
TIMEOUT="${LEMONADE_TIMEOUT:-180}"
MAX_TOKENS="${LEMONADE_MAX_TOKENS:-800}"

# Audit-log header (stderr).
{ printf '=== task input @ %s ===\n%s\n' "$ts" "$input"; } >&2

# Build the request body. jq does the JSON escaping for us so newlines /
# quotes in the task text don't break the stream.
body=$(jq -n \
    --arg model "$MODEL" \
    --argjson max_tokens "$MAX_TOKENS" \
    --arg user "Summarise the following content. Be concise (3-6 sentences). If it looks like a URL, infer that the operator wants the page summarised — do your best with what context you have.

---
$input" '
  {
    model: $model,
    stream: true,
    max_tokens: $max_tokens,
    chat_template_kwargs: { enable_thinking: false },
    messages: [{ role: "user", content: $user }]
  }')

# Drive the SSE stream; accumulate `content` deltas (skip the
# `reasoning_content` ones if the model still emits them — defensive).
output=""
while IFS= read -r line; do
    case "$line" in
        "data: [DONE]") break ;;
        "data: "*)
            json="${line#data: }"
            chunk=$(printf '%s' "$json" \
                | jq -r '.choices[0].delta.content // empty' 2>/dev/null || true)
            [[ -n "$chunk" ]] && output+="$chunk"
            ;;
    esac
done < <(curl -sN -m "$TIMEOUT" "$ENDPOINT/v1/chat/completions" \
            -H "Content-Type: application/json" \
            -d "$body")

ts_done="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
{ printf '\n=== model output @ %s (model=%s) ===\n%s\n' \
    "$ts_done" "$MODEL" "$output"; } >&2

# Trim leading/trailing whitespace before emitting on stdout.
printf '%s\n' "$(printf '%s' "$output" | sed -e 's/^[[:space:]]*//' -e 's/[[:space:]]*$//')"
