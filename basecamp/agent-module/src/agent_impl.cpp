#include "agent_impl.h"

#include <QByteArray>
#include <QCoreApplication>
#include <QDateTime>
#include <QDebug>
#include <QDir>
#include <QFile>
#include <QFileInfo>
#include <QJsonArray>
#include <QJsonDocument>
#include <QJsonObject>
#include <QJsonValue>
#include <QLocalSocket>
#include <QProcess>
#include <QProcessEnvironment>
#include <QRegularExpression>
#include <QStandardPaths>
#include <QString>

#include <atomic>
#include <chrono>
#include <dlfcn.h>
#include <mutex>
#include <random>
#include <thread>

namespace {

/// Hard cap on a single IPC frame — mirrors the Rust side's MAX_FRAME_BYTES.
constexpr qint64 MAX_FRAME_BYTES = 16 * 1024 * 1024;

/// How long the constructor waits for the daemon's socket to appear
/// before giving up. The daemon dials logos.dev which can take 15-25 s
/// from a cold start.
constexpr int SOCKET_WAIT_MS = 60'000;

/// Brief settle window between socket-present and the first IPC request,
/// giving the daemon time to subscribe to its inbox + presence and run
/// its initial announce.
constexpr int SOCKET_SETTLE_MS = 1'500;

/// Default delegation poll timeout (seconds). Local LLMs (especially
/// 35B+ on lemonade with cold KV cache) routinely sit 60-90 s on the
/// first turn, so the historical 60 s default surfaced as spurious
/// "delegation timed out" cards. Bumped to 180 s; override per-instance
/// via `LMAO_AGENT_DELEGATE_TIMEOUT_SECS`.
int delegateTimeoutSecs() {
    bool ok = false;
    int v = qEnvironmentVariable("LMAO_AGENT_DELEGATE_TIMEOUT_SECS").toInt(&ok);
    if (ok && v > 0) return v;
    return 180;
}

/// IPC envelope read window (milliseconds). Always a bit larger than
/// the daemon-side delegation timeout so the timeout response itself
/// has room to propagate before our local read gives up.
int delegateEnvelopeMs() {
    return delegateTimeoutSecs() * 1000 + 30'000;
}

QString errorJson(const QString& message) {
    QJsonObject obj;
    obj["error"] = message;
    return QString::fromUtf8(QJsonDocument(obj).toJson(QJsonDocument::Compact));
}

/// Locate THIS plugin's installed directory at runtime so we can look
/// for sibling files (a bundled `lmao` binary, `liblogosdelivery.so`,
/// etc.) without hardcoding the host's paths. dladdr resolves the
/// `.so` that a known symbol came from; we use this function itself
/// as the symbol so the lookup is self-contained.
QString pluginDir() {
    Dl_info info;
    if (dladdr(reinterpret_cast<void*>(&pluginDir), &info)
        && info.dli_fname && info.dli_fname[0]) {
        return QFileInfo(QString::fromUtf8(info.dli_fname)).absolutePath();
    }
    return QString();
}

/// Resolve the `lmao` binary path. Search order:
///   1. A bundled `lmao` / `logos-messaging-a2a` next to this plugin's
///      .so (set up by `scripts/build-fat-lgx.sh`). This is the path
///      that lets the LGX work in the official Basecamp build with no
///      operator-side env config.
///   2. `$LMAO_BIN` env var — explicit operator override.
///   3. Common system locations and `$PATH`.
QString resolveLmaoBinary() {
    const QString dir = pluginDir();
    if (!dir.isEmpty()) {
        for (const auto& name :
             {QStringLiteral("lmao"), QStringLiteral("logos-messaging-a2a")}) {
            const QString p = dir + "/" + name;
            if (QFileInfo::exists(p)) return p;
        }
    }
    if (auto env = qEnvironmentVariable("LMAO_BIN"); !env.isEmpty()
        && QFileInfo::exists(env)) {
        return env;
    }
    const QStringList candidates = {
        QDir::homePath() + "/.cargo/bin/lmao",
        "/usr/local/bin/lmao",
        "/usr/bin/lmao",
    };
    for (const auto& c : candidates) {
        if (QFileInfo::exists(c)) return c;
    }
    return QStandardPaths::findExecutable("logos-messaging-a2a");
}

QString socketPathFor(qint64 pid) {
    QString runtime = qEnvironmentVariable("XDG_RUNTIME_DIR");
    if (runtime.isEmpty()) runtime = QDir::tempPath();
    QDir().mkpath(runtime);
    return runtime + QStringLiteral("/lmao-basecamp-%1.sock").arg(pid);
}

/// Generate a 16-hex-char request id. Threadsafe, statistically
/// unique enough for in-flight task tracking inside a single host.
std::string newRequestId() {
    static std::mt19937_64 gen{std::random_device{}()};
    static std::mutex mu;
    std::lock_guard<std::mutex> lock(mu);
    char buf[17];
    snprintf(buf, sizeof(buf), "%016llx",
             (unsigned long long) std::uniform_int_distribution<uint64_t>{}(gen));
    return std::string(buf);
}

} // namespace

struct AgentImpl::State {
    QString lmaoBinary;
    QString socketPath;
    QProcess process;
    /// Becomes true only when the daemon's IPC socket exists on disk and
    /// has been given a brief settle window. All Q_INVOKABLE methods
    /// return an `{"error": "daemon not running"}` response while this
    /// is false — matching what we did before, but now without blocking
    /// the host's startup path.
    std::atomic<bool> started{false};
    /// Set on destruction so the wait thread exits promptly instead of
    /// stalling shutdown by 500 ms or so.
    std::atomic<bool> shuttingDown{false};
    /// Polls the socket path off the host's main thread. Joined in the
    /// destructor.
    std::thread waitThread;
};

AgentImpl::AgentImpl() : m_state(std::make_shared<State>()) {
    m_state->lmaoBinary = resolveLmaoBinary();
    if (m_state->lmaoBinary.isEmpty()) {
        qWarning().noquote()
            << "AgentImpl: lmao binary not found. Set $LMAO_BIN to the"
            << "logos-messaging-a2a release binary, or place it on PATH.";
        return;
    }
    m_state->socketPath = socketPathFor(QCoreApplication::applicationPid());
    QFile::remove(m_state->socketPath);

    // Port selection. The default logos-delivery TCP port (60000) clashes
    // with at least VS Code's local language-server socket on this
    // operator's machine, so we pick a less-trafficked range by default
    // and let env vars override per-instance when running multiple
    // Basecamps side by side.
    const QString tcpPort     = qEnvironmentVariable("LMAO_AGENT_TCP_PORT",     "60500");
    const QString udpPort     = qEnvironmentVariable("LMAO_AGENT_UDP_PORT",     "9500");
    const QString storagePort = qEnvironmentVariable("LMAO_AGENT_STORAGE_PORT", "19500");

    // Shim mode: route storage + messaging through Basecamp's
    // `storage_module` + `delivery_module` instead of bundling our
    // own libstorage / liblogosdelivery in the LGX. Opt-in via
    // `LMAO_AGENT_USE_SHIM=1` so the existing libstorage / embedded-Waku
    // path stays the default until the rollout (issue #19) is complete.
    //
    // TODO(issue #19): once the dependent flake plumbing lands, declare
    // `["delivery_module", "storage_module"]` in metadata.json so
    // Basecamp loads them before us. Until then the operator must
    // launch with all three modules in `-l` so the shim's `getClient`
    // calls find their counterparties in the registry.
    const bool useShim = qEnvironmentVariable("LMAO_AGENT_USE_SHIM") == QStringLiteral("1");

    QStringList args;
    // Persistent identity: keyfile lives next to the storage dir so
    // restarting Basecamp doesn't reroll the agent's secp256k1 pubkey.
    // Also unlocks sealed-presence on the basecamp daemon — `build_node`
    // auto-creates a `<keyfile>.x25519` sidecar when --keyfile is set.
    const QString agentKeyfile =
        QDir::homePath() + "/.local/share/lmao/agent.key";
    args << "--daemon-socket"     << m_state->socketPath
         << "--keyfile"           << agentKeyfile;

    if (useShim) {
        // The shim path delegates network + storage entirely to
        // Basecamp's own modules. No port pinning, no data-dir; both
        // are owned by delivery_module / storage_module.
        args << "--transport" << "delivery-module"
             << "--storage"   << "storage-module";
        // Optional explicit delivery_module cfg. Lets the operator
        // skip the catalog auto-discovery path when it's broken or
        // when they want a custom mesh config — passes directly to
        // `delivery_module.createNode(cfg)`.
        const QString cfg = qEnvironmentVariable("LMAO_AGENT_DELIVERY_CFG");
        if (!cfg.isEmpty()) {
            args << "--delivery-module-cfg" << cfg;
        }
    } else {
        args << "--transport"         << "logos-delivery"
             << "--tcp-port"          << tcpPort
             << "--udp-port"          << udpPort
             << "--storage"           << "libstorage"
             << "--storage-data-dir"  << QDir::homePath() + "/.local/share/lmao/storage"
             << "--storage-port"      << storagePort;
    }

    // Optional explicit entry-node peers — comma- or whitespace-separated
    // multiaddrs from $LMAO_AGENT_ENTRY_NODES. Used when the preset's
    // hardcoded bootstrap list is stale (server-side key rotation), or
    // for local-only peer-to-peer demos where the public mesh isn't
    // wanted. Repeats `--entry-node <multiaddr>` once per entry.
    {
        const QString raw = qEnvironmentVariable("LMAO_AGENT_ENTRY_NODES");
        if (!raw.isEmpty()) {
            const auto parts = raw.split(QRegularExpression(QStringLiteral("[\\s,]+")),
                                         Qt::SkipEmptyParts);
            for (const QString& p : parts) {
                args << "--entry-node" << p;
            }
        }
    }

    // Optional storage-layer bootstrap peers (SPRs) — libstorage only.
    // In shim mode `storage_module` owns the Codex node and we have no
    // say in its bootstrap config from here.
    if (!useShim) {
        const QString raw = qEnvironmentVariable("LMAO_AGENT_STORAGE_BOOTSTRAP");
        if (!raw.isEmpty()) {
            const auto parts = raw.split(QRegularExpression(QStringLiteral("[\\s,]+")),
                                         Qt::SkipEmptyParts);
            for (const QString& p : parts) {
                args << "--storage-bootstrap" << p;
            }
        }
    }

    args << "agent" << "run"
         << "--name"              << "basecamp"
         << "--capabilities"      << "text"
         << "--exec"              << qEnvironmentVariable(
                "LMAO_AGENT_EXEC",
                "sed s/^/[basecamp]\\ /");

    m_state->process.setProgram(m_state->lmaoBinary);
    m_state->process.setArguments(args);
    m_state->process.setProcessChannelMode(QProcess::ForwardedChannels);

    // Environment for the spawned `lmao agent run`. In the legacy path
    // we have to surface liblogosdelivery.so via LD_LIBRARY_PATH; in
    // shim mode there's no such library to find, but we must make sure
    // LOGOS_INSTANCE_ID propagates so the child's shim can discover the
    // QtRO registry running in this same Basecamp process.
    {
        QProcessEnvironment env = QProcessEnvironment::systemEnvironment();
        if (!useShim) {
            // Build LD_LIBRARY_PATH from (in priority order): the operator's
            // explicit LIBLOGOSDELIVERY_LIB_DIR, this plugin's own directory
            // (in case build-fat-lgx.sh bundled liblogosdelivery.so next to
            // the .so), and whatever was already in LD_LIBRARY_PATH. The
            // plugin-dir fallback is what makes the LGX self-contained on
            // an official-Basecamp install.
            QStringList ldDirs;
            const QString opLibDir = qEnvironmentVariable("LIBLOGOSDELIVERY_LIB_DIR");
            if (!opLibDir.isEmpty()) ldDirs << opLibDir;
            const QString plugDir = pluginDir();
            if (!plugDir.isEmpty()) ldDirs << plugDir;
            const QString existing = env.value("LD_LIBRARY_PATH");
            if (!existing.isEmpty()) ldDirs << existing;
            if (!ldDirs.isEmpty()) {
                env.insert("LD_LIBRARY_PATH", ldDirs.join(":"));
            }
            if (opLibDir.isEmpty() && plugDir.isEmpty()) {
                qWarning().noquote()
                    << "AgentImpl: neither LIBLOGOSDELIVERY_LIB_DIR nor a"
                    << "bundled libdir were resolvable; the spawned `lmao"
                    << "agent run` may fail to load liblogosdelivery.so.";
            }
        } else {
            // In shim mode the child uses LogosAPI over QtRO. The
            // registry URL is derived from LOGOS_INSTANCE_ID, which the
            // host (Basecamp) sets in our env. QProcessEnvironment
            // already inherits it from systemEnvironment(); just sanity-
            // check and warn loudly if it's missing.
            if (env.value("LOGOS_INSTANCE_ID").isEmpty()) {
                qWarning().noquote()
                    << "AgentImpl: LOGOS_INSTANCE_ID is not set; the"
                    << "shim path will fail to find Basecamp's QtRO"
                    << "registry. Are you running outside logos_host?";
            }
        }
        m_state->process.setProcessEnvironment(env);
    }

    m_state->process.start();

    if (!m_state->process.waitForStarted(5'000)) {
        qWarning() << "AgentImpl: lmao agent run failed to start:"
                   << m_state->process.errorString();
        return;
    }

    // Wait for the daemon's IPC socket to appear on a worker thread so
    // the constructor returns immediately. Blocking here would freeze
    // Basecamp's plugin loader for up to 60 s while logos.dev is dialled
    // — which is exactly what the user saw when clicking the LMAO tab.
    //
    // Yolo / whisper-wall / irc-module follow the same pattern: cheap
    // ctor + initLogos, slow work deferred. IPC methods check `started`
    // and return an error response until it flips, and the QML side's
    // 5 s status-refresh timer keeps retrying.
    const QString socketPath = m_state->socketPath;
    std::shared_ptr<State> state = m_state;
    m_state->waitThread = std::thread([state, socketPath]() {
        const auto deadline =
            std::chrono::steady_clock::now() + std::chrono::milliseconds(SOCKET_WAIT_MS);
        while (std::chrono::steady_clock::now() < deadline) {
            if (state->shuttingDown.load(std::memory_order_relaxed)) return;
            if (QFileInfo::exists(socketPath)) {
                // Brief settle window — let the daemon finish binding +
                // subscribing before the first IPC lands.
                std::this_thread::sleep_for(std::chrono::milliseconds(SOCKET_SETTLE_MS));
                state->started.store(true, std::memory_order_release);
                qInfo().noquote() << "AgentImpl: daemon up at" << socketPath;
                return;
            }
            std::this_thread::sleep_for(std::chrono::milliseconds(500));
        }
        qWarning() << "AgentImpl: daemon socket never appeared at" << socketPath;
    });
}

AgentImpl::~AgentImpl() {
    // Tell the wait-thread to bail out of its sleep loop; join it before
    // touching anything else so it can't observe a half-destroyed state.
    m_state->shuttingDown.store(true, std::memory_order_release);
    if (m_state->waitThread.joinable()) m_state->waitThread.join();

    if (m_state->started.load(std::memory_order_acquire)) {
        (void)stop_daemon();
        m_state->process.waitForFinished(3'000);
    }
    if (m_state->process.state() != QProcess::NotRunning) {
        m_state->process.terminate();
        if (!m_state->process.waitForFinished(2'000)) {
            m_state->process.kill();
            m_state->process.waitForFinished(1'000);
        }
    }
    QFile::remove(m_state->socketPath);
    // m_state is shared_ptr — drops here. Any worker threads still
    // running keep their own shared_ptr<State> reference; State stays
    // alive until the last one exits. Workers check shuttingDown
    // before emitting, so a destroyed AgentImpl won't fire bogus events.
}

namespace {

/// One-shot IPC: open the socket, write a length-prefixed JSON request
/// frame, read the length-prefixed JSON response frame, return its raw
/// JSON bytes. Mirrors the Rust-side framing in
/// `crates/logos-messaging-a2a-cli/src/daemon/`.
///
/// `readTimeoutMs` lets slow-path callers (storage fetches that walk
/// the Codex DHT, delegations that wait on a worker model) override
/// the default 30 s read window.
QString sendRequest(const QString& socketPath, const QJsonObject& request,
                    int readTimeoutMs = 30'000) {
    QLocalSocket sock;
    sock.connectToServer(socketPath);
    if (!sock.waitForConnected(5'000)) {
        return errorJson(QStringLiteral("connect to %1 failed: %2")
                             .arg(socketPath, sock.errorString()));
    }

    const QByteArray body = QJsonDocument(request).toJson(QJsonDocument::Compact);
    if (body.size() > MAX_FRAME_BYTES) {
        return errorJson(QStringLiteral("request frame too large: %1 bytes").arg(body.size()));
    }
    const quint32 len = static_cast<quint32>(body.size());
    QByteArray header(4, '\0');
    header[0] = static_cast<char>(len & 0xff);
    header[1] = static_cast<char>((len >> 8) & 0xff);
    header[2] = static_cast<char>((len >> 16) & 0xff);
    header[3] = static_cast<char>((len >> 24) & 0xff);
    sock.write(header);
    sock.write(body);
    if (!sock.waitForBytesWritten(5'000)) {
        return errorJson(QStringLiteral("write timed out: %1").arg(sock.errorString()));
    }

    while (sock.bytesAvailable() < 4) {
        if (!sock.waitForReadyRead(readTimeoutMs)) {
            return errorJson(QStringLiteral("read len timed out: %1").arg(sock.errorString()));
        }
    }
    QByteArray lenBuf = sock.read(4);
    const quint32 respLen = static_cast<quint8>(lenBuf[0])
                          | (static_cast<quint8>(lenBuf[1]) << 8)
                          | (static_cast<quint8>(lenBuf[2]) << 16)
                          | (static_cast<quint8>(lenBuf[3]) << 24);
    if (respLen > MAX_FRAME_BYTES) {
        return errorJson(QStringLiteral("response frame too large: %1 bytes").arg(respLen));
    }

    QByteArray respBody;
    while (respBody.size() < static_cast<int>(respLen)) {
        if (sock.bytesAvailable() == 0 && !sock.waitForReadyRead(readTimeoutMs)) {
            return errorJson(QStringLiteral("read body timed out: %1").arg(sock.errorString()));
        }
        respBody.append(sock.read(static_cast<int>(respLen) - respBody.size()));
    }
    return QString::fromUtf8(respBody);
}

QString simpleRequest(const QString& socketPath, const QString& kind,
                      int readTimeoutMs = 30'000) {
    QJsonObject obj;
    obj["kind"] = kind;
    return sendRequest(socketPath, obj, readTimeoutMs);
}

} // namespace

std::string AgentImpl::info() {
    if (!m_state->started.load(std::memory_order_acquire)) return errorJson("daemon not running").toStdString();
    return simpleRequest(m_state->socketPath, "info").toStdString();
}

std::string AgentImpl::peers(const std::string& capability_filter) {
    if (!m_state->started.load(std::memory_order_acquire)) return errorJson("daemon not running").toStdString();
    QJsonObject req;
    req["kind"] = "presence_peers";
    req["capability"] = capability_filter.empty()
                            ? QJsonValue(QJsonValue::Null)
                            : QJsonValue(QString::fromStdString(capability_filter));
    return sendRequest(m_state->socketPath, req).toStdString();
}

std::string AgentImpl::delegate(const std::string& capability,
                                const std::string& text) {
    if (!m_state->started.load(std::memory_order_acquire)) return errorJson("daemon not running").toStdString();
    QJsonObject req;
    req["kind"] = "task_delegate";
    req["to"] = QJsonValue::Null;
    req["capability"] = QString::fromStdString(capability);
    req["text"] = QString::fromStdString(text);
    req["parent_id"] = QStringLiteral("basecamp-%1")
                           .arg(QDateTime::currentMSecsSinceEpoch());
    // Worker-response poll timeout. Configurable via
    // LMAO_AGENT_DELEGATE_TIMEOUT_SECS — default 180 s. Local 35B
    // models on lemonade routinely take 60-120 s on the first turn
    // (cold KV cache); 60 s was too tight and surfaced as spurious
    // "delegation timed out".
    req["timeout_secs"] = delegateTimeoutSecs();
    req["broadcast"] = false;
    req["strategy"] = QJsonValue::Null;
    return sendRequest(m_state->socketPath, req, delegateEnvelopeMs()).toStdString();
}

std::string AgentImpl::send_task(const std::string& recipient_pubkey,
                                 const std::string& text) {
    if (!m_state->started.load(std::memory_order_acquire)) return errorJson("daemon not running").toStdString();
    QJsonObject req;
    req["kind"] = "task_send";
    req["to"] = QString::fromStdString(recipient_pubkey);
    req["text"] = QString::fromStdString(text);
    return sendRequest(m_state->socketPath, req).toStdString();
}

std::string AgentImpl::fetch_cid(const std::string& cid) {
    if (!m_state->started.load(std::memory_order_acquire)) return errorJson("daemon not running").toStdString();
    QJsonObject req;
    req["kind"] = "storage_fetch";
    req["cid"] = QString::fromStdString(cid);
    // Codex CID resolution can walk the DHT for tens of seconds when
    // the content is on a remote node — give the IPC a 90 s ceiling
    // so a slow first fetch doesn't surface as "Socket operation
    // timed out" to the operator.
    return sendRequest(m_state->socketPath, req, 90'000).toStdString();
}

// ── Async API ───────────────────────────────────────────────────
//
// Both `start_*` methods follow the same shape:
//   1. Generate a request_id
//   2. Spawn a detached worker thread that captures everything by
//      value (shared_ptr<State> for the socket path + shutdown flag,
//      std::function for emitEvent, plain strings for inputs)
//   3. Worker runs the blocking IPC, packs the response into a JSON
//      event payload, fires emitEvent if the host hasn't shut down
//   4. Caller gets back the request_id immediately so the QML side
//      can show a "running" placeholder
//
// Workers don't touch *this — only their captured State copy. If
// AgentImpl is destroyed while a worker runs, State stays alive via
// the worker's shared_ptr; the worker checks `state->shuttingDown`
// before emitting, so destroyed callbacks aren't fired.

std::string AgentImpl::start_delegate(const std::string& capability,
                                      const std::string& text,
                                      const std::string& session_id) {
    if (!m_state->started.load(std::memory_order_acquire)) {
        return errorJson("daemon not running").toStdString();
    }
    const std::string task_id = newRequestId();
    // Stamp a session_id even on first-turn delegations. Without one,
    // the receiver runs the executor in `--no-session` mode (pi-exec
    // doesn't write a sidecar), and a later "Follow up" can't resume
    // anything — pi creates a fresh session that doesn't know about
    // the original task. Auto-stamping makes the first task's session
    // exist, so the follow-up actually continues the same pi thread.
    const std::string effective_session =
        session_id.empty() ? newRequestId() : session_id;
    auto state = m_state;
    auto cb = emitEvent;
    QString cap = QString::fromStdString(capability);
    QString txt = QString::fromStdString(text);
    QString taskIdQ = QString::fromStdString(task_id);
    QString sessionQ = QString::fromStdString(effective_session);
    // Snapshot the configured timeout in the foreground so the worker
    // sees a stable value even if the env changes mid-run.
    int timeoutSecs = delegateTimeoutSecs();
    int envelopeMs = delegateEnvelopeMs();

    std::thread([state, cb, cap, txt, taskIdQ, sessionQ, timeoutSecs, envelopeMs]() {
        const auto t0 = std::chrono::steady_clock::now();
        QJsonObject req;
        req["kind"]         = "task_delegate";
        req["to"]           = QJsonValue::Null;
        req["capability"]   = cap;
        req["text"]         = txt;
        req["parent_id"]    = QStringLiteral("basecamp-%1").arg(taskIdQ);
        req["timeout_secs"] = timeoutSecs;
        req["broadcast"]    = false;
        req["strategy"]     = QJsonValue::Null;
        // Always send session_id — first-turn delegations get an
        // auto-stamped one (above) so the receiver's executor creates
        // a real session sidecar that a later "Follow up" can resume.
        req["session_id"] = sessionQ;

        QString resp = sendRequest(state->socketPath, req, envelopeMs);
        const auto t1 = std::chrono::steady_clock::now();
        const auto elapsedMs = std::chrono::duration_cast<std::chrono::milliseconds>(t1 - t0).count();

        QJsonObject event;
        event["task_id"]    = taskIdQ;
        event["elapsed_ms"] = (qint64)elapsedMs;

        QJsonDocument doc = QJsonDocument::fromJson(resp.toUtf8());
        if (doc.isObject()) {
            QJsonObject obj = doc.object();
            // Daemon serializes Response::Error as
            //   {"kind": "error", "message": "<text>"}
            // (see crates/.../daemon/protocol.rs — `#[serde(tag = "kind",
            // rename_all = "snake_case")]`). The earlier code looked for
            // a top-level `error` key that the daemon never emits, so
            // every daemon-side failure (no live peers, trust filtered,
            // delegation timed out before a single tick of the strategy
            // selector) collapsed to a misleading "no matching peer
            // responded". Match the actual shape, AND keep the legacy
            // `error` key as a fallback for any older daemon someone
            // might still be talking to.
            if (obj.value("kind").toString() == "error" && obj.contains("message")) {
                event["success"] = false;
                event["error"]   = obj["message"];
            } else if (obj.contains("error")) {
                event["success"] = false;
                event["error"]   = obj["error"];
            } else {
                QJsonArray results = obj["results"].toArray();
                if (results.isEmpty()) {
                    event["success"] = false;
                    event["error"]   = "no matching peer responded";
                } else {
                    QJsonObject r = results[0].toObject();
                    event["success"]  = r["success"].toBool();
                    event["agent_id"] = r["agent_id"];
                    event["body"]     = r["result_text"];
                    event["error"]    = r["error"];
                    QString body = r["result_text"].toString();
                    QRegularExpression re("codex://([A-Za-z0-9]+)");
                    auto m = re.match(body);
                    if (m.hasMatch()) {
                        event["cid"] = m.captured(1);
                    }
                }
            }
        } else {
            event["success"] = false;
            event["error"]   = "unparseable response";
        }

        if (state->shuttingDown.load(std::memory_order_acquire)) return;
        if (cb) {
            QString json = QString::fromUtf8(
                QJsonDocument(event).toJson(QJsonDocument::Compact));
            cb("delegate_complete", json.toStdString());
        }
    }).detach();

    QJsonObject ack;
    ack["task_id"]    = taskIdQ;
    // Echo the (possibly auto-stamped) session_id back so the QML can
    // store it on the task card. Follow-up uses model.session_id ?? task_id;
    // if we didn't return it, the QML would never see the auto-stamped
    // value and would fall through to task_id as a session-id, which
    // doesn't match the receiver's sidecar.
    ack["session_id"] = QString::fromStdString(effective_session);
    return QString::fromUtf8(QJsonDocument(ack).toJson(QJsonDocument::Compact)).toStdString();
}

std::string AgentImpl::start_fetch_cid(const std::string& cid) {
    if (!m_state->started.load(std::memory_order_acquire)) {
        return errorJson("daemon not running").toStdString();
    }
    const std::string request_id = newRequestId();
    auto state = m_state;
    auto cb = emitEvent;
    QString cidQ = QString::fromStdString(cid);
    QString reqIdQ = QString::fromStdString(request_id);

    std::thread([state, cb, cidQ, reqIdQ]() {
        QJsonObject req;
        req["kind"] = "storage_fetch";
        req["cid"]  = cidQ;
        QString resp = sendRequest(state->socketPath, req, 90'000);

        QJsonObject event;
        event["request_id"] = reqIdQ;
        event["cid"]        = cidQ;

        QJsonDocument doc = QJsonDocument::fromJson(resp.toUtf8());
        if (doc.isObject()) {
            QJsonObject obj = doc.object();
            // Same daemon-error shape as start_delegate above:
            // `{kind: "error", message}` is the wire format, but legacy
            // code checked the wrong key. Surface the real message.
            if (obj.value("kind").toString() == "error" && obj.contains("message")) {
                event["success"] = false;
                event["error"]   = obj["message"];
            } else if (obj.contains("error")) {
                event["success"] = false;
                event["error"]   = obj["error"];
            } else {
                event["success"]     = true;
                event["payload_b64"] = obj["payload_b64"];
            }
        } else {
            event["success"] = false;
            event["error"]   = "unparseable response";
        }

        if (state->shuttingDown.load(std::memory_order_acquire)) return;
        if (cb) {
            QString json = QString::fromUtf8(
                QJsonDocument(event).toJson(QJsonDocument::Compact));
            cb("fetch_cid_complete", json.toStdString());
        }
    }).detach();

    QJsonObject ack;
    ack["request_id"] = reqIdQ;
    return QString::fromUtf8(QJsonDocument(ack).toJson(QJsonDocument::Compact)).toStdString();
}

std::string AgentImpl::trust_list() {
    if (!m_state->started.load(std::memory_order_acquire)) return errorJson("daemon not running").toStdString();
    return simpleRequest(m_state->socketPath, "trust_list").toStdString();
}

std::string AgentImpl::trust_add(const std::string& pubkey,
                                 const std::string& nickname,
                                 const std::string& capabilities,
                                 const std::string& notes) {
    if (!m_state->started.load(std::memory_order_acquire)) return errorJson("daemon not running").toStdString();
    QJsonObject req;
    req["kind"] = "trust_add";
    req["pubkey"] = QString::fromStdString(pubkey);
    req["nickname"] = QString::fromStdString(nickname);
    QJsonArray caps;
    if (!capabilities.empty()) {
        const auto qcaps = QString::fromStdString(capabilities).split(
            ',', Qt::SkipEmptyParts);
        for (const auto& c : qcaps) caps.append(c.trimmed());
    }
    req["capabilities"] = caps;
    req["notes"] = notes.empty() ? QJsonValue(QJsonValue::Null)
                                 : QJsonValue(QString::fromStdString(notes));
    return sendRequest(m_state->socketPath, req).toStdString();
}

std::string AgentImpl::trust_remove(const std::string& target) {
    if (!m_state->started.load(std::memory_order_acquire)) return errorJson("daemon not running").toStdString();
    QJsonObject req;
    req["kind"] = "trust_remove";
    req["target"] = QString::fromStdString(target);
    return sendRequest(m_state->socketPath, req).toStdString();
}

std::string AgentImpl::trust_mode(const std::string& new_mode) {
    if (!m_state->started.load(std::memory_order_acquire)) return errorJson("daemon not running").toStdString();
    QJsonObject req;
    req["kind"] = "trust_mode";
    req["mode"] = new_mode.empty() ? QJsonValue(QJsonValue::Null)
                                   : QJsonValue(QString::fromStdString(new_mode));
    return sendRequest(m_state->socketPath, req).toStdString();
}

std::string AgentImpl::task_history_list(int64_t limit,
                                         int64_t offset,
                                         const std::string& direction,
                                         const std::string& capability) {
    if (!m_state->started.load(std::memory_order_acquire)) return errorJson("daemon not running").toStdString();
    QJsonObject req;
    req["kind"]      = "task_history_list";
    req["limit"]     = limit > 0 ? QJsonValue((qint64)limit) : QJsonValue(QJsonValue::Null);
    req["offset"]    = offset > 0 ? QJsonValue((qint64)offset) : QJsonValue(QJsonValue::Null);
    req["direction"] = direction.empty()  ? QJsonValue(QJsonValue::Null)
                                          : QJsonValue(QString::fromStdString(direction));
    req["capability"] = capability.empty() ? QJsonValue(QJsonValue::Null)
                                           : QJsonValue(QString::fromStdString(capability));
    req["since_ms"]  = QJsonValue(QJsonValue::Null);
    return sendRequest(m_state->socketPath, req).toStdString();
}

std::string AgentImpl::task_history_get(const std::string& task_id) {
    if (!m_state->started.load(std::memory_order_acquire)) return errorJson("daemon not running").toStdString();
    QJsonObject req;
    req["kind"]    = "task_history_get";
    req["task_id"] = QString::fromStdString(task_id);
    return sendRequest(m_state->socketPath, req).toStdString();
}

std::string AgentImpl::stop_daemon() {
    if (!m_state->started.load(std::memory_order_acquire)) return errorJson("daemon not running").toStdString();
    auto out = simpleRequest(m_state->socketPath, "shutdown").toStdString();
    m_state->started.store(false, std::memory_order_release);
    return out;
}

bool AgentImpl::is_running() {
    return m_state->started.load(std::memory_order_acquire)
        && m_state->process.state() == QProcess::Running
        && QFileInfo::exists(m_state->socketPath);
}

std::string AgentImpl::daemon_state() {
    if (m_state->started.load(std::memory_order_acquire)
        && m_state->process.state() == QProcess::Running) {
        return "ready";
    }
    if (m_state->process.state() == QProcess::Running) {
        return "starting";
    }
    return "offline";
}
