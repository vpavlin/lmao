// agent_info_probe — minimal Remote-mode consumer of LogosAPI.
//
// What it answers:
//   "Can a thin client process attach to a running `logoscore --mode 0`
//    daemon over Qt Remote Objects and synchronously invoke a method
//    on a loaded module?"
//
// If yes: the same pattern wraps trivially in a Rust crate via a
// C-callable shim + `bindgen`, which is the green-light for the
// CLI-as-Remote-consumer roll-out path in
// https://github.com/vpavlin/lmao/issues/19 (workstream 3 option A).
//
// If no: we know the bindings story is not "just `bindgen`, ship it"
// and we route through other options (Local mode, sidecar daemon,
// stdio bridge) before committing to the migration.
//
// Usage:
//   1. In one terminal, start a Remote-mode logoscore loaded with the
//      modules we depend on:
//
//        export LOGOS_INSTANCE_ID=$(uuidgen | tr -d - | head -c12)
//        logoscore --mode 0 \
//          -m ~/.local/share/Logos/LogosBasecamp/modules \
//          -l delivery_module,storage_module,accounts_module,agent
//
//   2. In another terminal — same env (LOGOS_INSTANCE_ID matters):
//
//        export LOGOS_INSTANCE_ID=<same as above>
//        ./build/agent_info_probe
//
//      Should print agent.info()'s JSON to stdout.
//
// Build (see CMakeLists.txt — needs Qt6 + logos-cpp-sdk on CMAKE_PREFIX_PATH):
//   cmake -B build -G Ninja
//   cmake --build build

#include <QCoreApplication>
#include <QDebug>
#include <QString>
#include <QVariant>
#include <QVariantList>
#include <QTimer>
#include <cstdio>

#include "logos_api.h"
#include "logos_api_client.h"
#include "logos_mode.h"

namespace {

constexpr int CALL_TIMEOUT_MS = 10'000;

void runProbe(int& exitCode) {
    // We're a Remote-mode CONSUMER. Default mode is already Remote, but
    // making the choice explicit guards against the env / config of the
    // host process accidentally flipping it.
    LogosModeConfig::setMode(LogosMode::Remote);

    // The module name is purely a label other modules see. Pick anything
    // distinct from existing names so logoscore-side logs stay readable.
    LogosAPI api(QStringLiteral("agent_info_probe"));

    auto* client = api.getClient(QStringLiteral("agent"));
    if (!client) {
        qCritical() << "getClient(\"agent\") returned null — is logoscore"
                    << "running with the agent module loaded? Same"
                    << "LOGOS_INSTANCE_ID exported in both processes?";
        exitCode = 2;
        QCoreApplication::quit();
        return;
    }

    // Synchronous call. The 0-arg overload is
    //     invokeRemoteMethod(objectName, methodName, args, timeout)
    // with `args` defaulting to an empty QVariantList; we have to pass
    // it explicitly to disambiguate from the (varargs ... Timeout) form.
    // `Timeout` is the SDK's typed wrapper so it doesn't accidentally
    // collapse into a positional QVariant argument.
    const QVariant raw = client->invokeRemoteMethod(
        QStringLiteral("agent"),
        QStringLiteral("info"),
        QVariantList(),
        Timeout(CALL_TIMEOUT_MS));

    if (!raw.isValid()) {
        qCritical() << "invokeRemoteMethod returned an invalid QVariant —"
                    << "method dispatch failed (no such method? wrong"
                    << "module-side dispatch table? IPC timeout?)";
        exitCode = 3;
        QCoreApplication::quit();
        return;
    }

    // agent.info() returns a JSON-encoded std::string from the
    // universal-module generator's wrapper, so `raw` should be a QString
    // (or, on some Basecamp builds, a JSON-encoded QString that needs a
    // re-decode — same gotcha that bit Main.qml's parseModuleJson).
    const QString result = raw.toString();
    qInfo().noquote() << "agent.info() →" << result;
    fputs(result.toUtf8().constData(), stdout);
    fputc('\n', stdout);

    exitCode = 0;
    QCoreApplication::quit();
}

}  // namespace

int main(int argc, char* argv[]) {
    QCoreApplication app(argc, argv);
    app.setApplicationName(QStringLiteral("agent_info_probe"));

    int exitCode = 1;
    // Run the probe on the next tick of the event loop so QtRO has a
    // chance to settle its connection state before we issue the call.
    QTimer::singleShot(0, [&]() { runProbe(exitCode); });

    app.exec();
    return exitCode;
}
