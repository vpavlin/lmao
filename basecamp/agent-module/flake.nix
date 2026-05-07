{
  description = "Logos Agent Module";

  inputs = {
    logos-module-builder.url = "github:logos-co/logos-module-builder";
    nix-bundle-lgx.url = "github:logos-co/nix-bundle-lgx";
  };

  outputs = inputs@{ logos-module-builder, ... }:
    logos-module-builder.lib.mkLogosModule {
      src = ./.;
      configFile = ./metadata.json;
      flakeInputs = inputs;
      preConfigure = ''
        logos-cpp-generator --from-header src/agent_impl.h \
          --backend qt \
          --impl-class AgentImpl \
          --impl-header agent_impl.h \
          --metadata metadata.json \
          --output-dir ./generated_code

        # logos-cpp-generator (as of 0.1.0) does NOT auto-wire the impl's
        # public emitEvent callback to LogosProviderBase::emitEvent — despite
        # CLAUDE.md claiming it does. Without this, async start_* methods that
        # fire events from worker threads no-op silently (cb is a default-
        # constructed std::function). Inject a constructor on
        # AgentProviderObject that does the wiring. Drop this awk block when
        # upstream actually emits the constructor.
        awk '
          /^#include "agent_impl.h"/ {
            print
            print "#include <QCoreApplication>"
            print "#include <QMetaObject>"
            next
          }
          !done && /^private:/ {
            print "public:"
            print "    AgentProviderObject() {"
            print "        // Worker threads in AgentImpl::start_* fire this callback. The"
            print "        // inherited emitEvent ends up writing to a QtRO socket, which is"
            print "        // not thread-safe. Direct invocation from the worker thread logs"
            print "        // QSocketNotifier warnings and the packet is dropped. Marshal to"
            print "        // the main thread via QCoreApplication event loop."
            print "        m_impl.emitEvent = [this](const std::string& name, const std::string& data) {"
            print "            QString n = QString::fromStdString(name);"
            print "            QString d = QString::fromStdString(data);"
            print "            QMetaObject::invokeMethod(QCoreApplication::instance(),"
            print "                [this, n, d]() { emitEvent(n, QVariantList{d}); },"
            print "                Qt::QueuedConnection);"
            print "        };"
            print "    }"
            print ""
            done=1
          }
          { print }
        ' generated_code/agent_qt_glue.h > generated_code/agent_qt_glue.h.patched
        mv generated_code/agent_qt_glue.h.patched generated_code/agent_qt_glue.h
      '';
    };
}
