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
/// Methods come in two flavours:
///
/// - Synchronous (`info`, `peers`, `daemon_state`, `trust_*`, …) —
///   block on a fast IPC round-trip (~10 ms). Use for queries the UI
///   needs to render this frame.
/// - Asynchronous (`start_delegate`, `start_fetch_cid`) — return a
///   request_id immediately, run the actual IPC on a worker thread,
///   then call `emitEvent("<name>_complete", json)` with a payload
///   keyed by that request_id. Use for anything that can take more
///   than a frame (LLM inference, Codex DHT walks).
///
/// Returned strings are JSON unless otherwise noted; on error the
/// returned JSON is shaped `{"error": "<message>"}` so callers can
/// branch on `.error`.
///
/// API constraints (per Logos universal-module rules): only
/// `std::string`, `bool`, `int64_t`, `uint64_t`, `double`, `void`,
/// `std::vector<T>`. No Qt types in this header.
class AgentImpl {
public:
    AgentImpl();
    ~AgentImpl();

    AgentImpl(const AgentImpl&) = delete;
    AgentImpl& operator=(const AgentImpl&) = delete;

    /// Daemon identity, uptime, configuration.
    std::string info();

    /// Live peers in the daemon's PeerMap. Pass an empty string to
    /// list all live peers; pass a capability (e.g. `"code"`) to filter.
    std::string peers(const std::string& capability_filter);

    /// Delegate a task by capability — finds a matching peer, sends
    /// the task, waits for the response.
    std::string delegate(const std::string& capability, const std::string& text);

    /// Async delegate. Returns a JSON ack with a `task_id` immediately;
    /// the actual IPC runs on a worker thread. On completion fires
    /// `emitEvent("delegate_complete", json)` where `json` carries:
    ///   { task_id, success, agent_id, body, cid, error, elapsed_ms }
    /// The QML side starts the call, then listens via
    /// `Connections { target: logos; function onModuleEventReceived(...) }`
    /// to render results without blocking the UI thread.
    /// `session_id` (empty string = none) threads the conversation: when
    /// set, the receiver's exec gets `LMAO_SESSION_ID=<id>` so wrappers
    /// (pi --session, lemonade conversation history) can reuse a thread
    /// instead of cold-starting on every follow-up.
    /// (Single-line signature — the universal-module C++ parser
    /// silently skips multi-line method declarations.)
    std::string start_delegate(const std::string& capability, const std::string& text, const std::string& session_id);

    /// Send a task directly to a known recipient pubkey.
    std::string send_task(const std::string& recipient_pubkey, const std::string& text);

    /// Fetch bytes by CID from the embedded libstorage backend. The
    /// payload is base64-encoded in the JSON response since IPC
    /// carries arbitrary binary.
    std::string fetch_cid(const std::string& cid);

    /// Async fetch. Returns a JSON ack with a `request_id` immediately;
    /// runs on a worker thread. On completion fires
    /// `emitEvent("fetch_cid_complete", json)`:
    ///   { request_id, cid, success, payload_b64, error }
    /// Used for the auto-prefetch on delegation success and any CID
    /// click in the UI — the QML thread never blocks on the Codex
    /// DHT walk.
    std::string start_fetch_cid(const std::string& cid);

    /// Ask the running daemon to stop and exit cleanly.
    std::string stop_daemon();

    /// Whether the subprocess is currently running and its IPC socket
    /// is reachable.
    bool is_running();

    /// Local liveness probe — no IPC round-trip. Returns one of:
    ///   - `"ready"`    daemon socket has appeared and the agent is
    ///                  accepting IPC.
    ///   - `"starting"` subprocess spawned, still dialing the mesh /
    ///                  binding the socket. Typically lasts 10-20 s
    ///                  on a cold start.
    ///   - `"offline"`  subprocess never started or exited.
    /// Used by the QML status timer to render a tri-state badge
    /// without paying the IPC cost while the daemon is still booting.
    std::string daemon_state();

    /// Snapshot the daemon's friend-keyring trust list. Returns the
    /// JSON daemon response: `{kind: "trust_list", mode, entries: […],
    /// trust_file?}`.
    std::string trust_list();

    /// Add (or replace) a trusted peer. `capabilities` is a comma-
    /// separated list — empty string means "trusted for any capability".
    /// `notes` may be empty. Returns the JSON daemon response.
    /// (Single-line signature — the universal-module C++ parser
    /// silently skips multi-line method declarations, which is how
    /// this method shipped without a dispatch wrapper for two builds.)
    std::string trust_add(const std::string& pubkey, const std::string& nickname, const std::string& capabilities, const std::string& notes);

    /// Remove a trusted peer by pubkey *or* nickname.
    std::string trust_remove(const std::string& target);

    /// Get or set the trust enforcement mode. Pass an empty string to
    /// query without changing it; otherwise `"off"`, `"enforce"`, or
    /// `"log"`.
    std::string trust_mode(const std::string& new_mode);

    /// List persisted task history (newest first). Pass empty strings
    /// for `direction` and `capability` to skip those filters. `limit`
    /// caps the page size; `offset` skips entries before applying the
    /// cap (useful for pagination). Returns the daemon JSON shape:
    /// `{kind: "task_history_list", entries: [...], history_path?}`.
    /// (Single-line signature — the universal-module C++ parser
    /// silently skips multi-line method declarations.)
    std::string task_history_list(int64_t limit, int64_t offset, const std::string& direction, const std::string& capability);

    /// Look up a single persisted task by id. Returns
    /// `{kind: "task_history_get", entry?: {...}}` — the entry is
    /// `null` if no row matches.
    std::string task_history_get(const std::string& task_id);

    /// Event emission callback — wired up by the universal-module
    /// generator at construction time so it can dispatch to the
    /// LogosProviderBase event channel. Don't call directly; use the
    /// `start_*` async methods which call it on completion.
    std::function<void(const std::string& eventName,
                       const std::string& data)> emitEvent;

private:
    struct State;
    std::shared_ptr<State> m_state;
};
