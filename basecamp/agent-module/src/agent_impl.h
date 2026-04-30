#pragma once

#include <cstdint>
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
/// All methods are blocking on the IPC round-trip. Returned strings
/// are JSON unless otherwise noted; on error the returned JSON is
/// shaped `{"error": "<message>"}` so callers can branch on `.error`.
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

    /// Send a task directly to a known recipient pubkey.
    std::string send_task(const std::string& recipient_pubkey, const std::string& text);

    /// Fetch bytes by CID from the embedded libstorage backend. The
    /// payload is base64-encoded in the JSON response since IPC
    /// carries arbitrary binary.
    std::string fetch_cid(const std::string& cid);

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
    std::string trust_add(const std::string& pubkey,
                          const std::string& nickname,
                          const std::string& capabilities,
                          const std::string& notes);

    /// Remove a trusted peer by pubkey *or* nickname.
    std::string trust_remove(const std::string& target);

    /// Get or set the trust enforcement mode. Pass an empty string to
    /// query without changing it; otherwise `"off"`, `"enforce"`, or
    /// `"log"`.
    std::string trust_mode(const std::string& new_mode);

private:
    struct State;
    State* m_state;
};
