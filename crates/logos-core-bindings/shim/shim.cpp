// shim.cpp — see shim.h for the contract + Phase A's findings on why
// this exists.

#include "shim.h"

#include <atomic>
#include <chrono>
#include <condition_variable>
#include <cstdlib>
#include <cstring>
#include <deque>
#include <memory>
#include <mutex>
#include <string>
#include <thread>
#include <vector>

#include <QCoreApplication>
#include <QDebug>
#include <QJsonArray>
#include <QJsonDocument>
#include <QJsonObject>
#include <QLoggingCategory>
#include <QMetaObject>
#include <QString>
#include <QVariant>
#include <QVariantList>

#include "logos_api.h"
#include "logos_api_client.h"
#include "logos_mode.h"
#include "logos_object.h"
#include "logos_types.h"

namespace {

// Drop Qt's debug / info logging at the message-handler level so TUI
// callers don't get their alternate-screen rendering trashed by
// QtRO's chatty per-call traces. Warnings + critical messages still
// pass through. Operators who want the verbose output can set
// `LOGOS_SHIM_VERBOSE=1` in the env.
void quietQtMessageHandler(QtMsgType type, const QMessageLogContext& ctx, const QString& msg) {
    if (type == QtDebugMsg || type == QtInfoMsg) return;
    QByteArray utf8 = msg.toUtf8();
    fprintf(stderr, "[%s] %s\n",
            type == QtWarningMsg  ? "warning" :
            type == QtCriticalMsg ? "critical" :
            type == QtFatalMsg    ? "fatal" : "?",
            utf8.constData());
    if (type == QtFatalMsg) std::abort();
    (void)ctx;
}

}  // namespace

namespace {

// argc / argv kept alive for the life of QCoreApplication, which holds
// references to them. Static so they outlive the lambda that builds the
// app.
int g_qt_argc = 1;
char g_qt_arg0[] = "logos_shim";
char* g_qt_argv[] = { g_qt_arg0, nullptr };

// Build a heap-allocated null-terminated copy of a QString's UTF-8.
// Caller frees with free().
char* dup_qstring(const QString& s) {
    QByteArray utf8 = s.toUtf8();
    char* out = static_cast<char*>(std::malloc(utf8.size() + 1));
    if (!out) return nullptr;
    std::memcpy(out, utf8.constData(), utf8.size());
    out[utf8.size()] = '\0';
    return out;
}

char* dup_cstr(const char* s) {
    const size_t n = std::strlen(s);
    char* out = static_cast<char*>(std::malloc(n + 1));
    if (!out) return nullptr;
    std::memcpy(out, s, n + 1);
    return out;
}

// Convert a QVariant returned from invokeRemoteMethod into a JSON
// string the caller can parse uniformly. The Logos SDK / universal
// module generator typically returns `std::string` of JSON, which the
// QtRO bridge wraps as QString — so the common case is "raw is a
// QString that's already JSON". Some modules return QVariantMap or
// QVariantList; convert those via QJsonDocument::fromVariant.
//
// As of logos-cpp-sdk's "Abstraction & Refactor part 1" the SDK can
// also return a `LogosResult` struct ({success, value, error}) — we
// unwrap that here so callers see the underlying value or a
// daemon-shape `{"error": "..."}` payload.
QString variant_to_json(const QVariant& raw) {
    if (!raw.isValid())
        return QStringLiteral("{\"error\":\"invalid response (no method dispatched?)\"}");

    // Unwrap LogosResult before anything else — the inner value goes
    // through the same conversion path so we get the same JSON shape
    // as pre-refactor SDKs that returned the bare value. Some modules'
    // failure paths leave r.error as an invalid QVariant; substitute a
    // generic message so the caller doesn't see `{"error":""}` which
    // would otherwise look like a successful empty response.
    if (raw.canConvert<LogosResult>() &&
        QString::fromUtf8(raw.typeName()) == QStringLiteral("LogosResult")) {
        const LogosResult r = raw.value<LogosResult>();
        if (!r.success) {
            QString msg = r.error.toString();
            if (msg.isEmpty()) {
                msg = QStringLiteral("LogosResult: success=false, error unset");
            }
            QJsonObject err;
            err["error"] = msg;
            return QString::fromUtf8(
                QJsonDocument(err).toJson(QJsonDocument::Compact));
        }
        // Successful LogosResult with an invalid (void/empty) inner
        // value — the method ran but had nothing to return. Surface
        // this as a literal `true` so callers expecting a bool result
        // see a success, and `{"value":true}` parsers also work.
        if (!r.value.isValid()) {
            return QStringLiteral("true");
        }
        return variant_to_json(r.value);
    }

    if (raw.canConvert<QString>()) {
        QString s = raw.toString();
        // Heuristic: if the string is already a JSON object/array, return
        // it as-is. Otherwise wrap it as a JSON string literal so
        // downstream parsers don't choke.
        const QString trimmed = s.trimmed();
        if (trimmed.startsWith('{') || trimmed.startsWith('[') ||
            trimmed.startsWith('"') || trimmed == QStringLiteral("null") ||
            trimmed == QStringLiteral("true") || trimmed == QStringLiteral("false")) {
            return s;
        }
        // Plain string — encode as a JSON string literal.
        // QJsonValue(s).toObject() returns an empty object, so we use an
        // array wrapper trick: JSON=["s"], strip the outer [ ].
        QByteArray arr = QJsonDocument(QJsonArray{QJsonValue(s)}).toJson(QJsonDocument::Compact);
        return QString::fromUtf8(arr.mid(1, arr.size() - 2));
    }
    // QJsonArray / QJsonObject stored directly in a QVariant — returned by
    // getPluginMethods and other QtProviderObject-based calls. Try explicit
    // casts before falling back to fromVariant, which doesn't handle these
    // types reliably across Qt versions.
    if (raw.canConvert<QJsonArray>()) {
        QJsonArray arr = raw.value<QJsonArray>();
        return QString::fromUtf8(QJsonDocument(arr).toJson(QJsonDocument::Compact));
    }
    if (raw.canConvert<QJsonObject>()) {
        QJsonObject obj = raw.value<QJsonObject>();
        return QString::fromUtf8(QJsonDocument(obj).toJson(QJsonDocument::Compact));
    }
    QJsonDocument d = QJsonDocument::fromVariant(raw);
    if (d.isObject() || d.isArray())
        return QString::fromUtf8(d.toJson(QJsonDocument::Compact));

    // Last resort: stringify whatever it is.
    return QStringLiteral("{\"error\":\"unsupported response type: %1\"}")
        .arg(QString::fromUtf8(raw.typeName()));
}

}  // namespace

struct LogosShim {
    std::thread qt_thread;
    QCoreApplication* app = nullptr;
    LogosAPI* api = nullptr;

    // Set true once `app` and `api` are both constructed and the Qt
    // event loop is about to enter exec(). Callers wait on this before
    // their first call.
    std::atomic<bool> ready{false};
    std::mutex ready_mu;
    std::condition_variable ready_cv;

    // ── Event subscription state ─────────────────────────────────
    //
    // Cross-thread queue between the Qt thread (eventResponse slots
    // enqueue here) and the Rust caller's polling thread
    // (logos_shim_poll_event dequeues). The queue stores ready-to-
    // hand-out JSON strings already in the
    //   {"module": …, "event": …, "data": …}
    // shape so dequeue is just a string copy.
    std::mutex events_mu;
    std::condition_variable events_cv;
    std::deque<std::string> events;
    // Modules we've already connected an eventResponse slot for.
    // Calling logos_shim_listen twice for the same module is a no-op.
    std::vector<std::string> listened;
};

LogosShim* logos_shim_new(const char* module_name) {
    if (!module_name) return nullptr;
    auto* shim = new LogosShim();
    const std::string mn = module_name;

    shim->qt_thread = std::thread([shim, mn]() {
        // Install the quiet message handler before any Qt class logs.
        // Operators who want verbose tracing (debugging the bridge,
        // not running the TUI) can opt in via env.
        if (!std::getenv("LOGOS_SHIM_VERBOSE")) {
            qInstallMessageHandler(quietQtMessageHandler);
        }

        QCoreApplication app(g_qt_argc, g_qt_argv);
        app.setApplicationName(QStringLiteral("logos_shim"));

        LogosModeConfig::setMode(LogosMode::Remote);
        LogosAPI api(QString::fromStdString(mn));

        shim->app = &app;
        shim->api = &api;
        {
            std::lock_guard<std::mutex> lk(shim->ready_mu);
            shim->ready.store(true, std::memory_order_release);
            shim->ready_cv.notify_all();
        }
        app.exec();
        // app and api destruct here as the function unwinds.
        shim->app = nullptr;
        shim->api = nullptr;
    });

    // Wait for the Qt thread to publish its app + api pointers.
    std::unique_lock<std::mutex> lk(shim->ready_mu);
    shim->ready_cv.wait(lk, [&]() { return shim->ready.load(std::memory_order_acquire); });
    return shim;
}

char* logos_shim_call(LogosShim* shim,
                      const char* target_module,
                      const char* method,
                      const char* args_json,
                      int timeout_ms) {
    if (!shim || !target_module || !method)
        return dup_cstr("{\"error\":\"shim or required arg is null\"}");
    if (!shim->api || !shim->app)
        return dup_cstr("{\"error\":\"shim not ready\"}");
    if (timeout_ms <= 0) timeout_ms = 30'000;

    // Per-call sync state. Captured by the lambda; the lambda runs on
    // the Qt thread, this function's stack stays alive because we
    // block on done_cv until either the lambda finishes or the outer
    // timeout fires.
    struct Slot {
        std::mutex mu;
        std::condition_variable cv;
        bool done = false;
        QString result;
    };
    auto slot = std::make_shared<Slot>();

    const std::string target = target_module;
    const std::string meth = method;
    const std::string args = args_json ? args_json : "[]";

    QMetaObject::invokeMethod(shim->app, [shim, slot, target, meth, args, timeout_ms]() {
        QString out;
        QVariantList vargs;
        if (!args.empty()) {
            QJsonParseError err{};
            QJsonDocument doc = QJsonDocument::fromJson(QByteArray::fromStdString(args), &err);
            if (err.error != QJsonParseError::NoError) {
                out = QStringLiteral("{\"error\":\"args_json parse failed: %1\"}").arg(err.errorString());
            } else if (!doc.isArray()) {
                out = QStringLiteral("{\"error\":\"args_json must be a JSON array\"}");
            } else {
                // toVariantList() maps JSON numbers to QVariant(double).
                // The module provider dispatches by Qt type, and most slots
                // use int/qlonglong — so coerce whole-number doubles to int
                // (or qlonglong for larger values) to allow correct dispatch.
                for (const QJsonValue& jv : doc.array()) {
                    if (jv.isDouble()) {
                        double d = jv.toDouble();
                        double intpart;
                        if (std::modf(d, &intpart) == 0.0) {
                            if (d >= INT_MIN && d <= INT_MAX)
                                vargs.append(QVariant(static_cast<int>(d)));
                            else
                                vargs.append(QVariant(static_cast<qlonglong>(d)));
                        } else {
                            vargs.append(QVariant(d));
                        }
                    } else if (jv.isObject()) {
                        // {"__base64__": "<base64>"} → QVariant(QByteArray) for binary
                        // parameters (e.g. storage_module uploadChunk(sessionId, QByteArray)).
                        const QJsonObject obj = jv.toObject();
                        const QJsonValue b64v = obj.value(QStringLiteral("__base64__"));
                        if (!b64v.isUndefined() && b64v.isString()) {
                            vargs.append(QVariant(QByteArray::fromBase64(
                                b64v.toString().toLatin1())));
                        } else {
                            vargs.append(jv.toVariant());
                        }
                    } else {
                        vargs.append(jv.toVariant());
                    }
                }
            }
        }

        if (out.isEmpty()) {
            auto* client = shim->api->getClient(QString::fromStdString(target));
            if (!client) {
                out = QStringLiteral("{\"error\":\"getClient(\\\"%1\\\") returned null\"}").arg(QString::fromStdString(target));
            } else {
                QVariant raw = client->invokeRemoteMethod(
                    QString::fromStdString(target),
                    QString::fromStdString(meth),
                    vargs,
                    Timeout(timeout_ms));
                out = variant_to_json(raw);
            }
        }

        {
            std::lock_guard<std::mutex> lk(slot->mu);
            slot->result = std::move(out);
            slot->done = true;
            slot->cv.notify_all();
        }
    }, Qt::QueuedConnection);

    // Outer timeout: SDK timeout + a 5 s grace window. If we hit it,
    // QtRO is wedged (registry not reachable) and we want to surface
    // that rather than hang the caller forever.
    const auto deadline = std::chrono::steady_clock::now() +
                          std::chrono::milliseconds(timeout_ms + 5'000);
    std::unique_lock<std::mutex> lk(slot->mu);
    if (!slot->cv.wait_until(lk, deadline, [&]() { return slot->done; })) {
        // Note: the Qt thread may still be holding the lambda alive
        // when this fires. The shared_ptr<Slot> keeps the slot alive
        // until the lambda also drops its reference, so a later
        // notification doesn't dangle.
        return dup_cstr("{\"error\":\"shim outer timeout exceeded — QtRO registry probably unreachable\"}");
    }

    char* result = dup_qstring(slot->result);
    return result ? result : dup_cstr("{\"error\":\"shim oom on result copy\"}");
}

int logos_shim_listen(LogosShim* shim, const char* module_name, const char* event_name) {
    if (!shim || !shim->app || !shim->api || !module_name || !event_name) return 0;

    const std::string mn = module_name;
    const std::string en = event_name;
    std::atomic<bool> ok{false};

    // Run the actual connect() on the Qt thread (Qt object affinity).
    // BlockingQueuedConnection so we don't return before the slot is wired.
    QMetaObject::invokeMethod(
        shim->app,
        [shim, mn, en, &ok]() {
            const std::string key = mn + "\0" + en;
            // De-dupe: skip if we've already registered this (module, event).
            for (const auto& k : shim->listened) {
                if (k == key) {
                    ok.store(true);
                    return;
                }
            }
            auto* client = shim->api->getClient(QString::fromStdString(mn));
            if (!client) return;
            auto* obj = client->requestObject(QString::fromStdString(mn), Timeout(5000));
            if (!obj) return;
            client->onEvent(
                obj,
                QString::fromStdString(en),
                [shim, mn, en](const QString& eventName, const QVariantList& data) {
                    // Serialise (module, event, data) into a JSON string and
                    // enqueue. `data` is a QVariantList — already a JSON-array
                    // shape; convert and embed.
                    QJsonArray jsonData = QJsonArray::fromVariantList(data);
                    QJsonObject event;
                    event["module"] = QString::fromStdString(mn);
                    event["event"] = eventName;
                    event["data"] = jsonData;
                    const std::string s =
                        QJsonDocument(event).toJson(QJsonDocument::Compact).toStdString();
                    {
                        std::lock_guard<std::mutex> lk(shim->events_mu);
                        shim->events.push_back(s);
                    }
                    shim->events_cv.notify_one();
                    (void)en;
                });
            shim->listened.push_back(key);
            ok.store(true);
        },
        Qt::BlockingQueuedConnection);

    return ok.load() ? 1 : 0;
}

char* logos_shim_poll_event(LogosShim* shim, int timeout_ms) {
    if (!shim) return nullptr;
    if (timeout_ms < 0) timeout_ms = 0;

    std::unique_lock<std::mutex> lk(shim->events_mu);
    if (shim->events.empty()) {
        // Wait up to timeout_ms for an event.
        shim->events_cv.wait_for(
            lk,
            std::chrono::milliseconds(timeout_ms),
            [shim]() { return !shim->events.empty(); });
    }
    if (shim->events.empty()) return nullptr;
    std::string ev = std::move(shim->events.front());
    shim->events.pop_front();
    lk.unlock();
    return dup_cstr(ev.c_str());
}

void logos_shim_free_str(char* s) {
    if (s) std::free(s);
}

void logos_shim_destroy(LogosShim* shim) {
    if (!shim) return;
    if (shim->app) {
        // Quit the event loop on its own thread; exec() returns and
        // the Qt thread function finishes.
        QMetaObject::invokeMethod(shim->app, []() {
            QCoreApplication::quit();
        }, Qt::QueuedConnection);
    }
    if (shim->qt_thread.joinable()) {
        shim->qt_thread.join();
    }
    delete shim;
}
