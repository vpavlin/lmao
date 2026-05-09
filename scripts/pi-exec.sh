#!/usr/bin/env bash
#
# Pi coding agent executor — wires Mario Zechner's `pi` CLI as an lmao
# agent --exec. Pi (https://pi.dev) is a minimal terminal coding harness
# with built-in read/bash/edit/write tools, so this peer can do real
# task analysis: fetch a URL with curl, grep a repo, run a quick test,
# and let the LLM summarize the result.
#
# Stdin: the task text (URL, prompt, code-review request — anything).
# Stdout: pi's reply (the analysis).
# Stderr: timestamped task input + reply for the lmao audit log
#         (uploaded to Logos Storage by the daemon if storage is on).
#
# Env:
#   PI_BIN          default `pi` (pi-coding-agent on PATH)
#   PI_PROVIDER     override pi's default provider (e.g. "ollama")
#   PI_MODEL        override pi's default model
#   PI_TIMEOUT      seconds before we kill pi (default 180)
#   PI_TOOLS        if set, ENABLE pi's read/bash/edit/write tools.
#                   Default is text-only (--no-tools) because tools
#                   on the demo machine probe AGENTS.md / CLAUDE.md
#                   context files in cwd and can hang on unrelated
#                   side trips. Turn on for deeper code-review tasks.
#   PI_THINKING     pi --thinking level: off|minimal|low|medium|high|xhigh
#
# Pi's own auth (API keys) and session-dir config live in
# `~/.pi/agent/settings.json` — we don't touch them here.

set -euo pipefail

input="$(cat)"
ts="$(date -u +%Y-%m-%dT%H:%M:%SZ)"

PI="${PI_BIN:-pi}"
TIMEOUT="${PI_TIMEOUT:-180}"

args=(-p)
# Reuse the conversation thread when the lmao daemon stamped a session.
# Pi keeps per-session state in --session-dir; we map our LMAO_SESSION_ID
# to a stable file under that dir. Pi's `--session <id>` matches by
# partial UUID, so we find an existing file and pass its UUID; first
# turn creates the session, subsequent turns reuse it (skips KV-cache
# cold-start on lemonade).
PI_SESSION_DIR="${PI_SESSION_DIR:-${HOME}/.pi/sessions/lmao}"
mkdir -p "$PI_SESSION_DIR"
if [[ -n "${LMAO_SESSION_ID:-}" ]]; then
    # Map LMAO_SESSION_ID (uuid or hex) to a sidecar that pi created.
    # The sidecar is `lmao-<our-id>.uuid` and stores the pi UUID we
    # extracted after pi auto-created the session on the first turn.
    safe_sid="$(printf '%s' "$LMAO_SESSION_ID" | tr -c 'A-Za-z0-9._-' '_')"
    map_file="$PI_SESSION_DIR/lmao-$safe_sid.uuid"
    if [[ -s "$map_file" ]]; then
        pi_uuid="$(cat "$map_file")"
        args+=(--session "$pi_uuid" --session-dir "$PI_SESSION_DIR")
    else
        # First turn for this thread: let pi auto-create. Capture the
        # new session's UUID below so the next turn can find it.
        args+=(--session-dir "$PI_SESSION_DIR")
        capture_new_session=1
    fi
else
    args+=(--no-session)
fi
[[ -n "${PI_PROVIDER:-}" ]] && args+=(--provider "$PI_PROVIDER")
[[ -n "${PI_MODEL:-}"    ]] && args+=(--model    "$PI_MODEL")
[[ -n "${PI_THINKING:-}" ]] && args+=(--thinking "$PI_THINKING")
# Default: no tools. Earlier we also passed --no-context-files /
# --no-skills / etc., but pi's session-save path silently exits when
# those are combined with --session-dir. Keep just --no-tools; cwd is
# /tmp anyway so context-file scans are harmless.
[[ -z "${PI_TOOLS:-}"    ]] && args+=(--no-tools)

# Override pi's default "coding assistant" system prompt — that prompt
# encourages pi to start with "Let me explore the project" and look
# around cwd, which (a) leaks the daemon's working directory into
# user-visible output and (b) confuses the model when the task is
# external (a URL, an unrelated topic) and there's nothing to explore.
# We want pi to be a plain analyst: read input, produce one direct
# answer, stop.
#
# Two flavours: with and without tools. The "no-tools" prompt tells the
# model up-front that it has no fetch capability, so it answers from
# training instead of refusing. The "tools" prompt enables it to
# `curl`, `read`, etc. when the request needs grounding (e.g. a URL).
if [[ -n "${PI_TOOLS:-}" ]]; then
    SYSTEM_PROMPT_DEFAULT="You are an analyst peer in a delegation network. You have read/bash/edit/write tools available — use them when grounding the answer requires it (fetching a URL, inspecting a file, running a quick command). Otherwise answer directly from your training. Either way, produce one direct, complete answer and stop. Do not explore the local filesystem unless the request requires it. Do not ask follow-up questions. Do not preface with planning narration."
else
    SYSTEM_PROMPT_DEFAULT="You are an analyst peer in a delegation network. Read the request, produce one direct, complete answer, and stop. Do not explore the local filesystem. Do not ask follow-up questions. Do not preface with planning narration. If a URL is mentioned, answer based on what you know about it from your training; you cannot fetch live pages."
fi
SYSTEM_PROMPT="${PI_SYSTEM_PROMPT:-$SYSTEM_PROMPT_DEFAULT}"
args+=(--system-prompt "$SYSTEM_PROMPT")

# Audit-log header on stderr — environment + task identity. Anything
# the operator might want to correlate later (which model, which
# session, which sender, which agent capability path) goes here.
{
  echo "=== meta @ $ts ==="
  echo "lmao_task_text:    (see input below)"
  echo "lmao_session_id:   ${LMAO_SESSION_ID:-(none — single-shot)}"
  echo "lmao_sender:       ${LMAO_SENDER_PUBKEY:-(unknown)}"
  echo "pi_provider:       ${PI_PROVIDER:-(pi default — see settings.json)}"
  echo "pi_model:          ${PI_MODEL:-(pi default — see settings.json)}"
  echo "pi_thinking:       ${PI_THINKING:-(pi default)}"
  echo "pi_tools_enabled:  $([[ -n "${PI_TOOLS:-}" ]] && echo yes || echo no)"
  echo "pi_session_dir:    $PI_SESSION_DIR"
  echo "pi_session_uuid:   ${pi_uuid:-(new — captured below)}"
  echo "exec_timeout_secs: $TIMEOUT"
  echo
  echo "=== task input @ $ts ==="
  echo "$input"
  echo
  echo "=== pi invocation ==="
  echo "$PI ${args[*]}"
  echo
} >&2

# Capture pi's stderr to a temp file so we can fold it into the audit
# log under a labelled section. Previously dropped (`2>>/dev/null`),
# which lost tool-call traces, retry messages, and any model errors —
# exactly the diagnostics the operator wants when a task misbehaves.
pi_stderr="$(mktemp -t pi-exec-stderr.XXXXXX)"
trap 'rm -f "$pi_stderr"' EXIT

# Pipe input → pi. timeout(1) kills pi if it runs over budget so a stuck
# tool call (e.g. an infinite curl, a hung bash) can't pin the agent.
# We capture stdout into a variable so we can echo it both to lmao
# (stdout) and to the audit log (stderr).
#
# We `cd /tmp` first so pi doesn't pick up the daemon process's cwd
# as project context — pi's default behaviour scans cwd for AGENTS.md /
# CLAUDE.md / a git repo to ground its responses in. For a stateless
# delegation peer, that's noise and a privacy leak.
#
# We capture pi's exit status separately so we can distinguish "pi
# answered" from "timeout killed pi" / "pi crashed". Without this the
# `|| true` would swallow non-zero exits and the lmao agent would
# happily ship an empty/placeholder reply tagged as a successful task.
set +e
output="$(cd /tmp && printf '%s' "$input" | timeout "$TIMEOUT" "$PI" "${args[@]}" 2>"$pi_stderr")"
pi_exit=$?
set -e

# `timeout(1)` exits 124 when it had to kill the child; 137 if SIGKILL
# was sent (combined with --kill-after); other non-zero is whatever pi
# itself returned. Any of those = the task didn't complete cleanly.
exec_failed=0
if [[ "$pi_exit" -eq 124 || "$pi_exit" -eq 137 ]]; then
  output="(pi timed out after ${TIMEOUT}s — provider too slow or stuck mid-fetch)"
  exec_failed=1
elif [[ "$pi_exit" -ne 0 ]]; then
  output="(pi exited $pi_exit — see audit log for stderr)"
  exec_failed=1
elif [[ -z "$output" ]]; then
  # Exit 0 + no output also happens (e.g. pi --no-tools answering with
  # only a tool-call that never gets sent). Still a failure as far as
  # the operator is concerned — they expected an answer.
  output="(pi returned empty output — check provider config or PI_TIMEOUT=$TIMEOUT)"
  exec_failed=1
fi

echo "$output"

# When pi auto-created a session for this LMAO_SESSION_ID, find the
# new file (newest match in the dir) and remember its UUID so the next
# turn reuses the same session. The session filename format is
# `<isoTimestamp>_<uuid>.jsonl`.
if [[ "${capture_new_session:-0}" == "1" && -n "${map_file:-}" ]]; then
    newest="$(ls -t "$PI_SESSION_DIR"/*.jsonl 2>/dev/null | head -n1)"
    if [[ -n "$newest" ]]; then
        # Extract the uuid: filename is <ts>_<uuid>.jsonl. Strip dir,
        # then everything up to and including the underscore.
        base="${newest##*/}"
        uuid="${base#*_}"
        uuid="${uuid%.jsonl}"
        printf '%s\n' "$uuid" > "$map_file"
        # Also record it for the audit-log footer below — operator can
        # correlate this run to the session JSONL on disk.
        pi_uuid="$uuid"
    fi
fi

# Audit-log footer: pi output + everything pi printed to stderr +
# the per-turn slice of the session JSONL (tool calls, reasoning,
# usage stats — pi-coding-agent stores them all there).
{
  echo "=== pi output @ $(date -u +%Y-%m-%dT%H:%M:%SZ) ==="
  echo "$output"
  echo
  echo "=== pi stderr ==="
  if [[ -s "$pi_stderr" ]]; then
    cat "$pi_stderr"
  else
    echo "(empty)"
  fi
  echo

  # Pi keeps a per-session JSONL with one row per message
  # (user / tool-call / tool-result / assistant). Find the file matching
  # the session UUID we used and dump it. Skip if we don't know the
  # uuid (e.g. --no-session run).
  if [[ -n "${pi_uuid:-}" ]]; then
    # session filename ends with `_<uuid>.jsonl`; glob it.
    session_file="$(ls -t "$PI_SESSION_DIR"/*"_${pi_uuid}.jsonl" 2>/dev/null | head -n1)"
    if [[ -n "$session_file" && -s "$session_file" ]]; then
      echo "=== pi session trace ($(basename "$session_file")) ==="
      # Try to pretty-print each line if jq is available; otherwise
      # dump raw. Pi rows are usually small enough to read raw.
      if command -v jq >/dev/null 2>&1; then
        jq -c '.' "$session_file" 2>/dev/null || cat "$session_file"
      else
        cat "$session_file"
      fi
      echo
    fi
  fi
} >&2

# Propagate failure to the lmao agent so it calls respond_failed()
# instead of respond(), which lets the UI render a red ✗ on the task
# card rather than a green ✓ with a stub answer.
exit "$exec_failed"
