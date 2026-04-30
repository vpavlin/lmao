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
#include <QStandardPaths>
#include <QString>

#include <atomic>
#include <chrono>
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

QString errorJson(const QString& message) {
    QJsonObject obj;
    obj["error"] = message;
    return QString::fromUtf8(QJsonDocument(obj).toJson(QJsonDocument::Compact));
}

/// Resolve the `lmao` binary path. Honours `LMAO_BIN`, then falls back
/// to the conventional release path inside this repo, then PATH.
QString resolveLmaoBinary() {
    if (auto env = qEnvironmentVariable("LMAO_BIN"); !env.isEmpty()
        && QFileInfo::exists(env)) {
        return env;
    }
    const QStringList candidates = {
        QDir::homePath() + "/.cargo/bin/lmao",
        QDir::homePath() + "/devel/github.com/vpavlin/lmao/target/release/logos-messaging-a2a",
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

AgentImpl::AgentImpl() : m_state(new State) {
    m_state->lmaoBinary = resolveLmaoBinary();
    if (m_state->lmaoBinary.isEmpty()) {
        qWarning().noquote()
            << "AgentImpl: lmao binary not found. Set $LMAO_BIN to the"
            << "logos-messaging-a2a release binary, or place it on PATH.";
        return;
    }
    m_state->socketPath = socketPathFor(QCoreApplication::applicationPid());
    QFile::remove(m_state->socketPath);

    QStringList args;
    args << "--daemon-socket"     << m_state->socketPath
         << "--transport"         << "logos-delivery"
         << "--storage"           << "libstorage"
         << "--storage-data-dir"  << QDir::homePath() + "/.local/share/lmao/storage"
         << "agent" << "run"
         << "--name"              << "basecamp"
         << "--capabilities"      << "text"
         << "--exec"              << qEnvironmentVariable(
                "LMAO_AGENT_EXEC",
                "sed s/^/[basecamp]\\ /");

    m_state->process.setProgram(m_state->lmaoBinary);
    m_state->process.setArguments(args);
    m_state->process.setProcessChannelMode(QProcess::ForwardedChannels);
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
    State* state = m_state;
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
    delete m_state;
}

namespace {

/// One-shot IPC: open the socket, write a length-prefixed JSON request
/// frame, read the length-prefixed JSON response frame, return its raw
/// JSON bytes. Mirrors the Rust-side framing in
/// `crates/logos-messaging-a2a-cli/src/daemon/`.
QString sendRequest(const QString& socketPath, const QJsonObject& request) {
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
        if (!sock.waitForReadyRead(30'000)) {
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
        if (sock.bytesAvailable() == 0 && !sock.waitForReadyRead(30'000)) {
            return errorJson(QStringLiteral("read body timed out: %1").arg(sock.errorString()));
        }
        respBody.append(sock.read(static_cast<int>(respLen) - respBody.size()));
    }
    return QString::fromUtf8(respBody);
}

QString simpleRequest(const QString& socketPath, const QString& kind) {
    QJsonObject obj;
    obj["kind"] = kind;
    return sendRequest(socketPath, obj);
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
    req["timeout_secs"] = 25;
    req["broadcast"] = false;
    req["strategy"] = QJsonValue::Null;
    return sendRequest(m_state->socketPath, req).toStdString();
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
    return sendRequest(m_state->socketPath, req).toStdString();
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
