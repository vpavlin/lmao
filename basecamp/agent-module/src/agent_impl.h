#pragma once

#include <cstdint>
#include <functional>
#include <memory>
#include <string>
#include <vector>

/// LMAO agent module — exposes the running `lmao agent run` daemon's
/// IPC surface to other Logos modules and UIs.
///
/// On construction, this class spawns `lmao agent run --daemon-socket
/// <path>` as a subprocess, waits for the IPC socket to appear, and
/// proxies calls into JSON requests over that socket. The subprocess
/// owns the actual Logos Messaging node + libstorage; this class is
/// the C++ ↔ JSON bridge.
///
/// # Public API contract
///
/// The methods below are split into two intended-audience groups:
///
/// - **Client API** — the subset other Basecamp modules and UIs are
///   expected to depend on. Stable: changes need a version bump and
///   migration notes. Documented in detail in `docs/MODULE_API.md`.
///   Use case: a Notes app calling `agent.start_delegate("summarize",
///   note_text, "")` to summarise a note via any peer that advertises
///   the `summarize` capability.
/// - **Operator API** — used by an agent's own settings/admin UI
///   (currently the QML Trust pane in `basecamp/agent-ui`). Not part
///   of the public contract; semantics may shift as the trust list and
///   daemon-lifecycle surfaces evolve.
///
/// # Method flavours
///
/// - **Synchronous** (`info`, `peers`, `daemon_state`, `trust_*`,
///   `task_history_*`) — block on a fast IPC round-trip (~10 ms). Use
///   for queries the UI needs to render this frame.
/// - **Asynchronous** (`start_delegate`, `start_fetch_cid`) — return a
///   request_id JSON ack immediately; the actual IPC runs on a worker
///   thread; on completion fire `emitEvent("<name>_complete", json)`
///   keyed by that id. Use for anything that can take more than a
///   frame (LLM inference, Codex DHT walks). **Recommended over the
///   synchronous variants when calling from a UI thread.**
///
/// # Return shapes
///
/// All methods return JSON-encoded `std::string`. Two error shapes a
/// caller must handle:
///
/// - From this module's own validation (e.g. daemon not running):
///   `{"error": "<message>"}`. Branch on `.error`.
/// - Forwarded from the daemon process:
///   `{"kind": "error", "message": "<text>"}`. Branch on
///   `kind == "error" && message`.
///
/// Successful responses are documented per-method in
/// `docs/MODULE_API.md`. The `delegate_complete` and
/// `fetch_cid_complete` event payloads are documented there too.
///
/// # Universal-module type constraints
///
/// Per Logos universal-module rules, only `std::string`, `bool`,
/// `int64_t`, `uint64_t`, `double`, `void`, `std::vector<T>`. No Qt
/// types in this header.
///
/// **Method declarations must be single-line.** The C++ generator
/// silently skips multi-line declarations — `trust_add` shipped
/// without a dispatch wrapper for two builds because of this. If you
/// edit this header, keep every method on one line.
class AgentImpl {
public:
    AgentImpl();
    ~AgentImpl();

    AgentImpl(const AgentImpl&) = delete;
    AgentImpl& operator=(const AgentImpl&) = delete;

    // ── Client API ──────────────────────────────────────────────────
    //
    // Stable surface for other Basecamp modules + UIs to depend on.
    // See docs/MODULE_API.md for the full contract.

    /// Daemon identity, uptime, configuration. Synchronous.
    /// Returns `{name, pubkey, capabilities, encryption_pubkey?,
    /// storage_spr?, load, uptime_secs, …}`.
    std::string info();

    /// Local liveness probe — no IPC round-trip. Returns one of:
    /// `"ready"` (daemon socket up, accepting IPC),
    /// `"starting"` (subprocess spawned, still dialing the mesh / binding
    /// the socket — typically 10–20 s on a cold start),
    /// `"offline"` (subprocess never started or exited).
    /// Use this before calling other methods so the UI can render a
    /// non-blocking "starting…" state.
    std::string daemon_state();

    /// Live peers in the daemon's PeerMap. Pass an empty string to
    /// list all live peers; pass a capability (e.g. `"code"`) to
    /// filter. Returns a JSON array of peer entries.
    std::string peers(const std::string& capability_filter);

    /// Async delegate by capability — finds a matching peer, sends
    /// the subtask, listens for the response. Returns immediately with
    /// `{task_id, session_id}`; on completion fires
    /// `emitEvent("delegate_complete", json)` carrying
    /// `{task_id, success, agent_id, body, cid, error, elapsed_ms}`.
    /// `session_id` (empty string = stamp a fresh one) threads the
    /// conversation across follow-up turns: receivers see it as
    /// `LMAO_SESSION_ID` so executor wrappers (pi `--session`,
    /// lemonade conversation history) can reuse a thread instead of
    /// cold-starting every turn. **Recommended entry point** —
    /// non-blocking, event-driven, plays nicely with QML.
    std::string start_delegate(const std::string& capability, const std::string& text, const std::string& session_id);

    /// Synchronous delegate by capability. Blocks the caller for up to
    /// the configured timeout (`LMAO_AGENT_DELEGATE_TIMEOUT_SECS`,
    /// default 180 s). Use only from a worker thread; prefer
    /// `start_delegate` from a UI thread.
    std::string delegate(const std::string& capability, const std::string& text);

    /// Send a task directly to a known recipient pubkey, fire-and-forget.
    /// Returns `{task_id, acked}` once the recipient ACKs the envelope
    /// (no executor result is waited for — that arrives via the same
    /// `delegate_complete` event channel if the receiver responds).
    std::string send_task(const std::string& recipient_pubkey, const std::string& text);

    /// Async fetch a CID from the storage backend. Returns immediately
    /// with `{request_id, cid}`; on completion fires
    /// `emitEvent("fetch_cid_complete", json)` carrying
    /// `{request_id, cid, success, payload_b64, error}`. **Recommended
    /// entry point** for any CID-resolution UI affordance — the Codex
    /// DHT walk can take tens of seconds.
    std::string start_fetch_cid(const std::string& cid);

    /// Synchronous fetch — blocks for up to 90 s. Returns
    /// `{cid, payload_b64}` on success; the standard error shapes on
    /// failure. Prefer `start_fetch_cid` from a UI thread.
    std::string fetch_cid(const std::string& cid);

    /// List persisted task history (newest first). Pass empty strings
    /// for `direction` and `capability` to skip those filters. `limit`
    /// caps the page size; `offset` skips entries before applying the
    /// cap (pagination). Returns
    /// `{kind: "task_history_list", entries: [...], history_path?}`.
    std::string task_history_list(int64_t limit, int64_t offset, const std::string& direction, const std::string& capability);

    /// Look up a single persisted task by id. Returns
    /// `{kind: "task_history_get", entry?: {...}}`. `entry` is `null`
    /// when no row matches.
    std::string task_history_get(const std::string& task_id);

    // ── Operator API ────────────────────────────────────────────────
    //
    // Used by an agent's own admin/settings UI. Not part of the
    // contract other modules consume. Subject to change.

    /// Snapshot the daemon's friend-keyring trust list. Returns
    /// `{kind: "trust_list", mode, entries: [...], trust_file?}`.
    std::string trust_list();

    /// Add (or replace) a trusted peer. `capabilities` is a comma-
    /// separated list — empty string means "trusted for any capability".
    /// `notes` may be empty.
    std::string trust_add(const std::string& pubkey, const std::string& nickname, const std::string& capabilities, const std::string& notes);

    /// Remove a trusted peer by pubkey *or* nickname.
    std::string trust_remove(const std::string& target);

    /// Get or set the trust enforcement mode. Pass an empty string to
    /// query without changing it; otherwise `"off"`, `"enforce"`, or
    /// `"log"`.
    std::string trust_mode(const std::string& new_mode);

    /// Ask the running daemon to stop and exit cleanly. Returns once
    /// the daemon socket is gone.
    std::string stop_daemon();

    /// Whether the subprocess is currently running and its IPC socket
    /// is reachable. Lower-level than `daemon_state()` — that one is
    /// what UIs should call.
    bool is_running();

    // ── Generator hookup ───────────────────────────────────────────
    //
    // Wired by the universal-module generator's `awk` patch (see
    // `basecamp/agent-module/flake.nix`). Workers call this from
    // background threads; the patched ctor marshals onto the GUI
    // thread via `QMetaObject::invokeMethod(...,
    // Qt::QueuedConnection)` before forwarding to
    // LogosProviderBase::emitEvent.
    std::function<void(const std::string& eventName,
                       const std::string& data)> emitEvent;

private:
    struct State;
    std::shared_ptr<State> m_state;
};
