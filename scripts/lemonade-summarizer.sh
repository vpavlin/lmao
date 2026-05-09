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
#   LEMONADE_ENDPOINT  default http://localhost:8000
#   LEMONADE_MODEL     default user.Qwen3.5-4B-GGUF
#   LEMONADE_TIMEOUT   default 180 (seconds)
#   LEMONADE_MAX_TOKENS default 800

set -euo pipefail

input="$(cat)"
ts="$(date -u +%Y-%m-%dT%H:%M:%SZ)"

ENDPOINT="${LEMONADE_ENDPOINT:-http://localhost:8000}"
MODEL="${LEMONADE_MODEL:-user.Qwen3.5-4B-GGUF}"
TIMEOUT="${LEMONADE_TIMEOUT:-180}"
MAX_TOKENS="${LEMONADE_MAX_TOKENS:-800}"

# Per-thread conversation history. When the lmao daemon stamps an
# LMAO_SESSION_ID, we replay the prior turns as proper chat messages
# so lemonade can serve them from its KV-cache prefix instead of
# recomputing the whole conversation every follow-up. The history file
# is JSONL of `{role, content}` rows; one row per appended message.
#
# We cap the prior context to the last LEMONADE_HISTORY_TURNS rows
# (default 20 = 10 user/assistant pairs). Without this cap a long
# session bloats every request body — jq slurps the entire file into
# RAM and lemonade re-tokenises the lot, both of which can OOM a
# constrained host. The on-disk file still grows; it just isn't all
# replayed in one request. Trim it offline if you want.
SESSIONS_DIR="${LEMONADE_SESSIONS_DIR:-${HOME}/.local/share/lmao-summarizer/sessions}"
HISTORY_TURNS="${LEMONADE_HISTORY_TURNS:-20}"
mkdir -p "$SESSIONS_DIR"
HISTORY_FILE=""
if [[ -n "${LMAO_SESSION_ID:-}" ]]; then
    # Sanitise the session id for filesystem safety. lmao session ids
    # are uuids or hex prefixes, so this should be a no-op in practice.
    safe_sid="$(printf '%s' "$LMAO_SESSION_ID" | tr -c 'A-Za-z0-9._-' '_')"
    HISTORY_FILE="$SESSIONS_DIR/${safe_sid}.jsonl"
fi

USER_PROMPT="Summarise the following content. Be concise (3-6 sentences). If it looks like a URL, infer that the operator wants the page summarised — do your best with what context you have.

---
$input"

# Build messages array: last HISTORY_TURNS rows of prior history (if
# any) + new user turn. `tail -n` keeps the request body bounded even
# when the on-disk JSONL has grown huge.
if [[ -n "$HISTORY_FILE" && -s "$HISTORY_FILE" ]]; then
    messages_json="$(tail -n "$HISTORY_TURNS" "$HISTORY_FILE" \
        | jq -s --arg user "$USER_PROMPT" \
            '. + [{role:"user", content:$user}]')"
else
    messages_json="$(jq -n --arg user "$USER_PROMPT" \
        '[{role:"user", content:$user}]')"
fi

# Audit-log header (stderr).
{
    printf '=== task input @ %s ===\n%s\n' "$ts" "$input"
    if [[ -n "$HISTORY_FILE" ]]; then
        prior_turns="$(jq -s 'length' "$HISTORY_FILE" 2>/dev/null || echo 0)"
        printf 'session: %s (prior turns: %s)\n' "$LMAO_SESSION_ID" "$prior_turns"
    fi
} >&2

# Build the request body. jq does the JSON escaping for us so newlines /
# quotes in the task text don't break the stream.
body=$(jq -n \
    --arg model "$MODEL" \
    --argjson max_tokens "$MAX_TOKENS" \
    --argjson messages "$messages_json" '
  {
    model: $model,
    stream: true,
    max_tokens: $max_tokens,
    chat_template_kwargs: { enable_thinking: false },
    messages: $messages
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
trimmed="$(printf '%s' "$output" | sed -e 's/^[[:space:]]*//' -e 's/[[:space:]]*$//')"

# Persist this turn so the next message in the same session sees it as
# prior context. Skip on empty/error output so a bad turn doesn't poison
# the thread with garbage. We append both the user prompt and the
# assistant reply — lemonade's prefix-cache hit needs identical message
# history on subsequent calls.
if [[ -n "$HISTORY_FILE" && -n "$trimmed" ]]; then
    jq -nc --arg c "$USER_PROMPT" '{role:"user", content:$c}'    >> "$HISTORY_FILE"
    jq -nc --arg c "$trimmed"     '{role:"assistant", content:$c}' >> "$HISTORY_FILE"
fi

printf '%s\n' "$trimmed"
