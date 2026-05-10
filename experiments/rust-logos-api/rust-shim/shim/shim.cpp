// shim.cpp — see shim.h for the contract + Phase A's findings on why
// this exists.

#include "shim.h"

#include <atomic>
#include <chrono>
#include <condition_variable>
#include <cstdlib>
#include <cstring>
#include <memory>
#include <mutex>
#include <string>
#include <thread>

#include <QCoreApplication>
#include <QDebug>
#include <QJsonArray>
#include <QJsonDocument>
#include <QJsonObject>
#include <QMetaObject>
#include <QString>
#include <QVariant>
#include <QVariantList>

#include "logos_api.h"
#include "logos_api_client.h"
#include "logos_mode.h"

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
QString variant_to_json(const QVariant& raw) {
    if (!raw.isValid())
        return QStringLiteral("{\"error\":\"invalid response (no method dispatched?)\"}");

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
        return QJsonDocument(QJsonValue(s).toObject()).toJson(QJsonDocument::Compact);
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
};

LogosShim* logos_shim_new(const char* module_name) {
    if (!module_name) return nullptr;
    auto* shim = new LogosShim();
    const std::string mn = module_name;

    shim->qt_thread = std::thread([shim, mn]() {
        // Quiet by default; the experiment's run procedure can flip
        // QT_LOGGING_RULES if we need verbose tracing.
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
                vargs = doc.array().toVariantList();
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
