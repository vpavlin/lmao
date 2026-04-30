import QtQuick
import QtQuick.Controls.Basic
import QtQuick.Layouts

// LMAO agent UI — talks to the `agent` core module which proxies a
// running `lmao agent run` daemon over Unix-socket IPC. All operations
// route through `logos.callModule("agent", method, args)`.
//
// Five panes:
//   1. Status     — daemon identity, uptime, capabilities
//   2. Peers      — live PeerMap from presence broadcasts, capability filter
//   3. Delegate   — capability + text → routed task → response
//   4. Trust      — friend-keyring management (mode, list, add/remove)
//   5. Audit      — paste a codex:// CID, fetch the bytes
//
// Visual tokens (`theme`) — token-driven palette so the colors live in
// one place and a future swap to the upstream Logos.Theme module is a
// few-line change. The values themselves intentionally diverge from
// Logos.Theme's gray850/gray900 base: those tokens are designed against
// Logos.Controls' surfaces and gradients, and look muddy when applied
// directly to a flat plugin pane against Basecamp's light app chrome.
// Spacing + typography scales follow Logos.Spacing.
Item {
    id: root

    // Solid dark background — covers Basecamp's light app chrome that
    // otherwise bleeds through and makes everything look out of place.
    Rectangle {
        anchors.fill: parent
        color: "#0d1117"
    }

    // ── Visual tokens ────────────────────────────────────────────
    QtObject {
        id: theme

        // Backgrounds — the original GitHub-inspired dark palette.
        // High contrast against Basecamp's light app chrome; reads
        // crisp at the panel borders.
        readonly property color background:         "#0d1117"
        readonly property color backgroundElevated: "#0d1117"  // header, inputs
        readonly property color backgroundSecondary: "#161b22" // panes
        readonly property color backgroundInset:    "#0d1117"  // list rows

        // Text
        readonly property color text:          "#ffffff"
        readonly property color textSecondary: "#8b949e"
        readonly property color textTertiary:  "#7d8590"
        readonly property color textMuted:     "#6e7681"

        // Borders
        readonly property color border:        "#30363d"
        readonly property color borderSubtle:  "#21262d"
        readonly property color borderDark:    "#21262d"

        // Status / accents — vivid where they need to be readable
        // against the dark panes.
        readonly property color success:    "#56d364"
        readonly property color successSoft:"#7ee787"   // pubkey hex
        readonly property color warning:    "#f0883e"   // "starting" badge
        readonly property color error:      "#f85149"
        readonly property color info:       "#79c0ff"   // codex link
        readonly property color primary:    "#ED7B58"   // orange300 — Logos accent

        // Tints for status badges (low-alpha background washes)
        readonly property color tintSuccess: "#1a3f2e"
        readonly property color tintWarning: "#3a2d10"
        readonly property color tintError:   "#572421"

        // Spacing scale (Logos.Spacing-aligned)
        readonly property int spaceTiny:   4
        readonly property int spaceSmall:  8
        readonly property int spaceMedium: 12
        readonly property int spaceLarge:  16
        readonly property int spaceXLarge: 20

        readonly property int radiusSmall:  4
        readonly property int radiusMedium: 6
        readonly property int radiusLarge:  8

        // Typography
        readonly property int fontTiny:   10
        readonly property int fontSmall:  11
        readonly property int fontBody:   12
        readonly property int fontMedium: 14
        readonly property int fontLarge:  18

        // Standard control height — keeps TextFields, Buttons, and
        // ComboBoxes vertically aligned on the same row.
        readonly property int controlHeight: 32
    }

    // ── Styled controls ─────────────────────────────────────────
    // Inline component definitions (Qt 6.3+) so we get rounded,
    // dark, theme-aware buttons / inputs / combos throughout without
    // a separate file per control. QtQuick.Controls.Basic is the
    // designable style — Material/Fusion defaults resist override.

    component DarkButton: Button {
        id: db
        height: theme.controlHeight
        padding: theme.spaceMedium
        font.pixelSize: theme.fontBody
        hoverEnabled: true

        background: Rectangle {
            radius: theme.radiusMedium
            color: !db.enabled ? Qt.rgba(0, 0, 0, 0.25)
                  : db.down    ? theme.borderDark
                  : db.hovered ? Qt.darker(theme.backgroundSecondary, 0.85)
                               : theme.backgroundElevated
            border.color: db.down || db.hovered ? theme.primary : theme.border
            border.width: 1
        }
        contentItem: Text {
            text: db.text
            color: db.enabled ? theme.text : theme.textMuted
            font: db.font
            horizontalAlignment: Text.AlignHCenter
            verticalAlignment: Text.AlignVCenter
            elide: Text.ElideRight
        }
    }

    component DarkPrimaryButton: Button {
        id: dpb
        height: theme.controlHeight
        padding: theme.spaceMedium
        font.pixelSize: theme.fontBody
        font.weight: Font.Medium
        hoverEnabled: true

        background: Rectangle {
            radius: theme.radiusMedium
            color: !dpb.enabled ? Qt.rgba(0, 0, 0, 0.25)
                  : dpb.down    ? Qt.darker(theme.primary, 1.3)
                  : dpb.hovered ? Qt.lighter(theme.primary, 1.1)
                                : theme.primary
            border.width: 0
        }
        contentItem: Text {
            text: dpb.text
            color: dpb.enabled ? "#ffffff" : theme.textMuted
            font: dpb.font
            horizontalAlignment: Text.AlignHCenter
            verticalAlignment: Text.AlignVCenter
            elide: Text.ElideRight
        }
    }

    component DarkTextField: TextField {
        id: dtf
        height: theme.controlHeight
        font.pixelSize: theme.fontBody
        color: theme.text
        placeholderTextColor: theme.textMuted
        selectionColor: theme.primary
        selectedTextColor: theme.text
        leftPadding: theme.spaceSmall + 2
        rightPadding: theme.spaceSmall + 2
        verticalAlignment: TextInput.AlignVCenter

        background: Rectangle {
            radius: theme.radiusMedium
            color: theme.backgroundElevated
            border.color: dtf.activeFocus ? theme.primary : theme.border
            border.width: 1
        }
    }

    component DarkComboBox: ComboBox {
        id: dcb
        height: theme.controlHeight
        font.pixelSize: theme.fontBody

        background: Rectangle {
            radius: theme.radiusMedium
            color: dcb.pressed ? theme.borderDark : theme.backgroundElevated
            border.color: dcb.activeFocus || dcb.pressed ? theme.primary : theme.border
            border.width: 1
        }
        contentItem: Text {
            text: dcb.displayText
            color: theme.text
            font: dcb.font
            verticalAlignment: Text.AlignVCenter
            leftPadding: theme.spaceSmall + 2
            elide: Text.ElideRight
        }
        indicator: Canvas {
            id: caret
            width: 10; height: 6
            anchors.right: parent.right
            anchors.rightMargin: theme.spaceSmall
            anchors.verticalCenter: parent.verticalCenter
            contextType: "2d"
            onPaint: {
                const ctx = getContext("2d");
                ctx.reset();
                ctx.beginPath();
                ctx.moveTo(0, 0);
                ctx.lineTo(width, 0);
                ctx.lineTo(width / 2, height);
                ctx.closePath();
                ctx.fillStyle = theme.textSecondary;
                ctx.fill();
            }
        }
        popup: Popup {
            y: dcb.height + 2
            width: dcb.width
            implicitHeight: Math.min(contentItem.implicitHeight + 8, 240)
            padding: 4
            background: Rectangle {
                color: theme.backgroundSecondary
                border.color: theme.border
                radius: theme.radiusMedium
            }
            contentItem: ListView {
                clip: true
                implicitHeight: contentHeight
                model: dcb.popup.visible ? dcb.delegateModel : null
                currentIndex: dcb.highlightedIndex
                ScrollBar.vertical: ScrollBar { policy: ScrollBar.AsNeeded }
            }
        }
        delegate: ItemDelegate {
            width: dcb.width - 8
            height: 28
            highlighted: dcb.highlightedIndex === index
            contentItem: Text {
                text: modelData
                color: theme.text
                font: dcb.font
                verticalAlignment: Text.AlignVCenter
            }
            background: Rectangle {
                color: highlighted ? theme.borderDark : "transparent"
                radius: theme.radiusSmall
            }
        }
    }


    /// Helper: parse the JSON string returned by the agent module. The
    /// module wraps everything as JSON; on errors it returns
    /// `{"error": "..."}`. Returns the parsed object, or null.
    function parseModuleJson(raw) {
        try {
            return JSON.parse(raw);
        } catch (e) {
            console.warn("agent_ui: invalid JSON from module:", raw);
            return null;
        }
    }

    function shorten(s, n) {
        if (!s) return "";
        return s.length > n ? s.substring(0, n) + "…" : s;
    }

    // ── Status pane state ────────────────────────────────────────
    property string statusName: ""
    property string statusPubkey: ""
    property var    statusCapabilities: []
    property int    statusUptimeSecs: 0
    property bool   statusStorageEnabled: false
    property string statusError: ""

    // Tri-state badge: "offline" | "starting" | "ready". Read cheaply
    // from the agent module via daemon_state() — no IPC round-trip.
    property string daemonState: "offline"

    function refreshStatus() {
        // Local-only liveness check first. While starting/offline, skip
        // the info() IPC entirely so we don't block the QML thread on a
        // 5 s waitForConnected against a socket that doesn't exist yet.
        const stateRaw = logos.callModule("agent", "daemon_state", []);
        const stateObj = parseModuleJson(stateRaw);
        const next = (stateObj && typeof stateObj === "string")
            ? stateObj
            : (stateRaw || "").replace(/"/g, "").trim();
        // The daemon_state Q_INVOKABLE returns a bare std::string,
        // which the universal-module bridge wraps as a JSON-encoded
        // string. Strip quotes if present.
        daemonState = (next === "ready" || next === "starting" || next === "offline")
            ? next : "offline";

        if (daemonState !== "ready") {
            statusError = daemonState === "starting"
                ? "daemon starting…"
                : "daemon offline";
            // Clear stale identity fields so we don't show last-known
            // values for a daemon that's no longer up.
            if (daemonState === "offline") {
                statusName = "";
                statusPubkey = "";
                statusCapabilities = [];
                statusUptimeSecs = 0;
                statusStorageEnabled = false;
            }
            return;
        }

        const raw = logos.callModule("agent", "info", []);
        const obj = parseModuleJson(raw);
        if (!obj || obj.error) {
            // Daemon claimed ready locally but IPC errored — flip back
            // to a transient "starting" while the operator waits for
            // the next refresh.
            statusError = obj && obj.error ? obj.error : "no response";
            return;
        }
        statusError = "";
        statusName = obj.name || "";
        statusPubkey = obj.pubkey || "";
        statusCapabilities = obj.capabilities || [];
        statusUptimeSecs = obj.uptime_secs || 0;
        statusStorageEnabled = obj.storage_enabled === true;
    }

    Component.onCompleted: {
        refreshStatus();
        statusTimer.start();
        // Faster poll while the daemon is starting up — the operator
        // sees the badge flip to "ready" within a second or two of the
        // socket appearing rather than waiting for the next 5 s tick.
        startingPollTimer.start();
    }
    Timer {
        id: statusTimer
        interval: 5000
        repeat: true
        onTriggered: root.refreshStatus()
    }
    Timer {
        id: startingPollTimer
        interval: 1000
        repeat: true
        running: root.daemonState === "starting"
        onTriggered: root.refreshStatus()
    }

    ColumnLayout {
        anchors.fill: parent
        anchors.margins: 16
        spacing: 12

        // ── Header ──────────────────────────────────────────────
        // Wrapped in a dark Rectangle so the title is visible regardless of
        // the Basecamp host theme — older builds put the chrome on a light
        // background and white-on-white text vanishes.
        Rectangle {
            Layout.fillWidth: true
            Layout.preferredHeight: 56
            color: theme.backgroundElevated
            radius: 6
            border.color: theme.border
            border.width: 1

            RowLayout {
                anchors.fill: parent
                anchors.leftMargin: 14
                anchors.rightMargin: 14
                spacing: 12

                Image {
                    Layout.preferredWidth: 38
                    Layout.preferredHeight: 38
                    source: "icon.png"
                    fillMode: Image.PreserveAspectFit
                    smooth: true
                    asynchronous: true
                    // Hide the broken-image glyph if the icon ever fails to
                    // load (Qt picks the source path relative to the
                    // installed plugin dir).
                    onStatusChanged: if (status === Image.Error) visible = false
                }

                ColumnLayout {
                    Layout.fillWidth: true
                    spacing: 2

                    Text {
                        text: "LMAO Agent"
                        font.pixelSize: 18
                        font.weight: Font.DemiBold
                        color: theme.text
                    }
                    Text {
                        text: "A2A coordination over Logos Messaging — local, decentralized, verifiable"
                        font.pixelSize: 10
                        color: theme.textSecondary
                    }
                }

                // Status badge — three states. Starting pulses so the
                // operator can see the daemon is dialling the mesh
                // rather than dead.
                Rectangle {
                    id: statusBadge
                    Layout.preferredWidth: badge.implicitWidth + 16
                    Layout.preferredHeight: 24
                    radius: 12

                    readonly property color tintReady:    theme.tintSuccess
                    readonly property color tintStarting: theme.tintWarning
                    readonly property color tintOffline:  theme.tintError
                    readonly property color edgeReady:    theme.success
                    readonly property color edgeStarting: theme.warning
                    readonly property color edgeOffline:  theme.error

                    color: root.daemonState === "ready"   ? tintReady
                         : root.daemonState === "starting" ? tintStarting
                         : tintOffline
                    border.color: root.daemonState === "ready"   ? edgeReady
                                : root.daemonState === "starting" ? edgeStarting
                                : edgeOffline
                    border.width: 1

                    Row {
                        id: badge
                        anchors.centerIn: parent
                        spacing: 6

                        // Dot. Pulses while starting via the
                        // SequentialAnimation below.
                        Rectangle {
                            id: badgeDot
                            width: 8; height: 8; radius: 4
                            anchors.verticalCenter: parent.verticalCenter
                            color: statusBadge.border.color
                            opacity: 1.0
                        }
                        Text {
                            text: root.daemonState === "ready"   ? "daemon ready"
                                : root.daemonState === "starting" ? "daemon starting"
                                : "daemon offline"
                            color: statusBadge.border.color
                            font.pixelSize: 11
                            anchors.verticalCenter: parent.verticalCenter
                        }
                    }

                    // Pulse animation — runs only while daemonState is
                    // "starting" so a steady ready/offline indicator
                    // doesn't flicker.
                    SequentialAnimation on opacity {
                        running: root.daemonState === "starting"
                        loops: Animation.Infinite
                        NumberAnimation { from: 1.0; to: 0.45; duration: 700; easing.type: Easing.InOutQuad }
                        NumberAnimation { from: 0.45; to: 1.0; duration: 700; easing.type: Easing.InOutQuad }
                        // Reset on stop so ready/offline aren't dimmed.
                        onRunningChanged: if (!running) statusBadge.opacity = 1.0
                    }
                }
            }
        }

        // ── Pane 1: Status ──────────────────────────────────────
        Rectangle {
            Layout.fillWidth: true
            Layout.preferredHeight: 110
            color: theme.backgroundSecondary
            radius: 6
            border.color: theme.border
            border.width: 1

            GridLayout {
                id: statusGrid
                anchors.fill: parent
                anchors.margins: 12
                columns: 2
                columnSpacing: 16
                rowSpacing: 4

                Text { text: "Name";        color: theme.textSecondary; font.pixelSize: 12 }
                Text { text: root.statusName || "—"; color: theme.text; font.pixelSize: 12 }

                Text { text: "Public key";  color: theme.textSecondary; font.pixelSize: 12 }
                Text { text: root.shorten(root.statusPubkey, 40) || "—"
                       color: theme.successSoft; font.pixelSize: 12; font.family: "monospace" }

                Text { text: "Capabilities"; color: theme.textSecondary; font.pixelSize: 12 }
                Text { text: root.statusCapabilities.join(", ") || "—"
                       color: theme.text; font.pixelSize: 12 }

                Text { text: "Uptime";       color: theme.textSecondary; font.pixelSize: 12 }
                Text { text: root.statusUptimeSecs + " s";  color: theme.text; font.pixelSize: 12 }

                Text { text: "Storage";      color: theme.textSecondary; font.pixelSize: 12 }
                Text { text: root.statusStorageEnabled ? "enabled (libstorage)" : "disabled"
                       color: root.statusStorageEnabled ? theme.successSoft : theme.warning; font.pixelSize: 12 }
            }
        }

        // ── Tabs (Network / Trust) ─────────────────────────────
        // Network is the main flow (peers, delegate, audit-log fetch).
        // Trust gets its own tab so the friend-keyring pane doesn't eat
        // 260 px of vertical space on every interaction.
        TabBar {
            id: tabs
            Layout.fillWidth: true
            background: Rectangle {
                color: "transparent"
                Rectangle {
                    anchors.bottom: parent.bottom
                    width: parent.width; height: 1
                    color: theme.borderSubtle
                }
            }

            component DarkTab: TabButton {
                id: tab
                height: 32
                font.pixelSize: theme.fontBody
                contentItem: Text {
                    text: tab.text
                    color: tab.checked ? theme.text : theme.textSecondary
                    font: tab.font
                    font.weight: tab.checked ? Font.Medium : Font.Normal
                    horizontalAlignment: Text.AlignHCenter
                    verticalAlignment: Text.AlignVCenter
                    elide: Text.ElideRight
                }
                background: Rectangle {
                    color: tab.checked ? theme.backgroundSecondary
                         : tab.hovered ? Qt.rgba(1, 1, 1, 0.03)
                                       : "transparent"
                    Rectangle {
                        // Active-tab indicator on the bottom edge.
                        anchors.bottom: parent.bottom
                        width: parent.width; height: 2
                        color: tab.checked ? theme.primary : "transparent"
                    }
                }
            }

            DarkTab { text: "Network" }
            DarkTab { text: "Trust" }
        }

        StackLayout {
            id: tabStack
            Layout.fillWidth: true
            Layout.fillHeight: true
            currentIndex: tabs.currentIndex

        // ── Network tab ────────────────────────────────────────
        ColumnLayout {
            spacing: 12

        // ── Pane 2 + 3 side by side ────────────────────────────
        RowLayout {
            Layout.fillWidth: true
            Layout.fillHeight: true
            spacing: 12

            // ── Peers pane ──
            Rectangle {
                Layout.fillWidth: true
                Layout.fillHeight: true
                Layout.preferredWidth: 1
                color: theme.backgroundSecondary
                radius: 6
                border.color: theme.border
                border.width: 1

                ColumnLayout {
                    anchors.fill: parent
                    anchors.margins: 12
                    spacing: 8

                    RowLayout {
                        Layout.fillWidth: true
                        Text {
                            text: "Peers"
                            color: theme.text
                            font.pixelSize: 14
                            font.weight: Font.DemiBold
                            Layout.fillWidth: true
                        }
                        DarkTextField {
                            id: peersFilter
                            placeholderText: "filter capability"
                            Layout.preferredWidth: 140
                        }
                        DarkButton {
                            text: "Refresh"
                            onClicked: peersList.refresh()
                        }
                    }

                    ListView {
                        id: peersList
                        Layout.fillWidth: true
                        Layout.fillHeight: true
                        clip: true
                        spacing: 6
                        model: ListModel { id: peersModel }

                        function refresh() {
                            const raw = logos.callModule("agent", "peers",
                                                         [peersFilter.text]);
                            const obj = root.parseModuleJson(raw);
                            peersModel.clear();
                            if (!obj || obj.error) {
                                if (obj && obj.error) console.warn("peers:", obj.error);
                                return;
                            }
                            const peers = obj.peers || [];
                            for (let i = 0; i < peers.length; i++) {
                                peersModel.append(peers[i]);
                            }
                        }

                        delegate: Rectangle {
                            width: ListView.view.width
                            height: peerCol.implicitHeight + 12
                            color: theme.backgroundElevated
                            radius: 4
                            border.color: theme.borderSubtle
                            border.width: 1

                            ColumnLayout {
                                id: peerCol
                                anchors.left: parent.left
                                anchors.right: parent.right
                                anchors.verticalCenter: parent.verticalCenter
                                anchors.margins: 8
                                spacing: 2

                                Text {
                                    text: model.name
                                    color: theme.successSoft
                                    font.pixelSize: 12
                                    font.weight: Font.DemiBold
                                }
                                Text {
                                    text: "caps: " + (model.capabilities || []).join(", ")
                                    color: theme.textSecondary
                                    font.pixelSize: 10
                                }
                                Text {
                                    text: root.shorten(model.agent_id || "", 32)
                                    color: theme.textMuted
                                    font.pixelSize: 10
                                    font.family: "monospace"
                                }
                            }
                        }
                    }

                    Text {
                        visible: peersModel.count === 0
                        text: "No live peers yet — try a filter or refresh."
                        color: theme.textMuted
                        font.pixelSize: 11
                        font.italic: true
                        Layout.alignment: Qt.AlignHCenter
                    }
                }
            }

            // ── Delegate pane ──
            Rectangle {
                Layout.fillWidth: true
                Layout.fillHeight: true
                Layout.preferredWidth: 1
                color: theme.backgroundSecondary
                radius: 6
                border.color: theme.border
                border.width: 1

                ColumnLayout {
                    anchors.fill: parent
                    anchors.margins: 12
                    spacing: 8

                    Text {
                        text: "Delegate task"
                        color: theme.text
                        font.pixelSize: 14
                        font.weight: Font.DemiBold
                    }

                    RowLayout {
                        Layout.fillWidth: true
                        Text {
                            text: "Capability"
                            color: theme.textSecondary
                            font.pixelSize: 12
                            Layout.preferredWidth: 80
                        }
                        DarkTextField {
                            id: delegateCap
                            Layout.fillWidth: true
                            placeholderText: "e.g. code, summarize, text"
                        }
                    }

                    Text {
                        text: "Task text"
                        color: theme.textSecondary
                        font.pixelSize: 12
                    }
                    Rectangle {
                        Layout.fillWidth: true
                        Layout.preferredHeight: 80
                        color: theme.backgroundElevated
                        border.color: theme.border
                        border.width: 1
                        radius: theme.radiusMedium

                        ScrollView {
                            anchors.fill: parent
                            anchors.margins: 1
                            clip: true

                            TextArea {
                                id: delegateText
                                placeholderText: "What do you want a peer to do?"
                                placeholderTextColor: theme.textMuted
                                wrapMode: TextArea.Wrap
                                color: theme.text
                                font.pixelSize: theme.fontBody
                                selectionColor: theme.primary
                                selectedTextColor: theme.text
                                background: Item {}
                                padding: theme.spaceSmall
                            }
                        }
                    }

                    DarkPrimaryButton {
                        text: delegateBusy ? "Delegating…" : "Delegate"
                        enabled: !delegateBusy && delegateCap.text.length > 0
                                 && delegateText.text.length > 0
                        property bool delegateBusy: false

                        onClicked: {
                            delegateBusy = true;
                            delegateResult.text = "Working…";
                            delegateCidLink.text = "";
                            // Synchronous IPC — Logos's RPC layer marshals
                            // this off the QML thread. Can take 5-25 s
                            // depending on network conditions.
                            const raw = logos.callModule("agent", "delegate",
                                                         [delegateCap.text, delegateText.text]);
                            const obj = root.parseModuleJson(raw);
                            delegateBusy = false;

                            if (!obj || obj.error) {
                                delegateResult.text = "Error: " +
                                    (obj && obj.error ? obj.error : "no response");
                                return;
                            }
                            const results = obj.results || [];
                            if (results.length === 0) {
                                delegateResult.text = "No matching peer responded.";
                                return;
                            }
                            const r = results[0];
                            if (!r.success) {
                                delegateResult.text = "Failed: " + (r.error || "unknown error");
                                return;
                            }
                            delegateResult.text = r.result_text || "(empty)";
                            // Pull a codex:// CID out of the result text if
                            // present so the audit pane can grab it.
                            const m = (r.result_text || "").match(/codex:\/\/([A-Za-z0-9]+)/);
                            if (m) {
                                delegateCidLink.text = m[1];
                                cidInput.text = m[1];
                            }
                        }
                    }

                    Text {
                        id: delegateResult
                        Layout.fillWidth: true
                        text: "Result will appear here."
                        color: theme.successSoft
                        font.pixelSize: 12
                        wrapMode: Text.Wrap
                    }
                    Text {
                        id: delegateCidLink
                        Layout.fillWidth: true
                        text: ""
                        visible: text.length > 0
                        color: theme.info
                        font.pixelSize: 10
                        font.family: "monospace"
                        wrapMode: Text.Wrap
                    }
                }
            }
        }

        // ── Audit pane (inside Network tab, below Peers+Delegate) ──
        Rectangle {
            Layout.fillWidth: true
            Layout.preferredHeight: 130
            color: theme.backgroundSecondary
            radius: 6
            border.color: theme.border
            border.width: 1

            ColumnLayout {
                anchors.fill: parent
                anchors.margins: 12
                spacing: 6

                RowLayout {
                    Layout.fillWidth: true

                    Text {
                        text: "Audit log fetch"
                        color: theme.text
                        font.pixelSize: 14
                        font.weight: Font.DemiBold
                        Layout.fillWidth: true
                    }
                    DarkTextField {
                        id: cidInput
                        placeholderText: "codex://CID (paste here)"
                        Layout.fillWidth: true
                    }
                    DarkPrimaryButton {
                        text: "Fetch"
                        enabled: cidInput.text.length > 0
                        onClicked: {
                            // Tolerate the codex:// prefix.
                            const cid = cidInput.text.replace(/^codex:\/\//, "");
                            const raw = logos.callModule("agent", "fetch_cid", [cid]);
                            const obj = root.parseModuleJson(raw);
                            if (!obj || obj.error) {
                                cidOut.text = "Error: " + (obj && obj.error ? obj.error : "no response");
                                return;
                            }
                            // Decode base64 into UTF-8 best-effort.
                            try {
                                const decoded = atob(obj.payload_b64 || "");
                                cidOut.text = decoded;
                            } catch (e) {
                                cidOut.text = "(non-UTF-8 payload, " + (obj.payload_b64 || "").length + " base64 chars)";
                            }
                        }
                    }
                }

                Rectangle {
                    Layout.fillWidth: true
                    Layout.fillHeight: true
                    color: theme.backgroundElevated
                    border.color: theme.border
                    border.width: 1
                    radius: theme.radiusMedium

                    ScrollView {
                        anchors.fill: parent
                        anchors.margins: 1
                        clip: true
                        TextArea {
                            id: cidOut
                            readOnly: true
                            placeholderText: "Fetched payload appears here."
                            placeholderTextColor: theme.textMuted
                            wrapMode: TextArea.Wrap
                            color: theme.text
                            font.family: "monospace"
                            font.pixelSize: theme.fontSmall
                            selectionColor: theme.primary
                            selectedTextColor: theme.text
                            background: Item {}
                            padding: theme.spaceSmall
                        }
                    }
                }
            }
        }
        } // end Network tab ColumnLayout

        // ── Trust tab ──────────────────────────────────────────
        // Friend-keyring management. Now its own tab so the list
        // doesn't eat 260 px of the main flow.
        Rectangle {
            Layout.fillWidth: true
            Layout.fillHeight: true
            color: theme.backgroundSecondary
            radius: 6
            border.color: theme.border
            border.width: 1

            ColumnLayout {
                id: trustCol
                anchors.fill: parent
                anchors.margins: 12
                spacing: 8

                // Trust pane state
                property string trustMode: ""
                property string trustFile: ""
                property var    trustEntries: []
                property string trustError: ""

                function refreshTrust() {
                    const raw = logos.callModule("agent", "trust_list", []);
                    const obj = root.parseModuleJson(raw);
                    if (!obj || obj.error) {
                        trustError = obj && obj.error ? obj.error : "no response";
                        trustEntries = [];
                        return;
                    }
                    trustError = "";
                    trustMode = obj.mode || "";
                    trustFile = obj.trust_file || "";
                    trustEntries = obj.entries || [];
                    trustModel.clear();
                    for (let i = 0; i < trustEntries.length; i++) {
                        trustModel.append(trustEntries[i]);
                    }
                }

                Component.onCompleted: refreshTrust()

                RowLayout {
                    Layout.fillWidth: true

                    Text {
                        text: "Friend-keyring trust list"
                        color: theme.text
                        font.pixelSize: 14
                        font.weight: Font.DemiBold
                        Layout.fillWidth: true
                    }

                    Text {
                        text: "Mode:"
                        color: theme.textSecondary
                        font.pixelSize: 12
                    }
                    DarkComboBox {
                        id: modeBox
                        Layout.preferredWidth: 110
                        model: ["off", "enforce", "log"]
                        currentIndex: trustCol.trustMode === "enforce" ? 1 :
                                      trustCol.trustMode === "log"     ? 2 : 0
                        onActivated: {
                            const next = model[currentIndex];
                            if (next === trustCol.trustMode) return;
                            const raw = logos.callModule("agent", "trust_mode", [next]);
                            const obj = root.parseModuleJson(raw);
                            if (obj && obj.error) {
                                console.warn("trust_mode:", obj.error);
                                return;
                            }
                            trustCol.refreshTrust();
                        }
                    }

                    DarkButton {
                        text: "Refresh"
                        onClicked: trustCol.refreshTrust()
                    }
                }

                Text {
                    visible: trustCol.trustFile.length > 0
                    text: "trust file: " + trustCol.trustFile
                    color: theme.textMuted
                    font.pixelSize: 10
                    font.family: "monospace"
                    Layout.fillWidth: true
                    elide: Text.ElideMiddle
                }

                // Add new entry
                RowLayout {
                    Layout.fillWidth: true

                    DarkTextField {
                        id: addPubkey
                        placeholderText: "pubkey (hex)"
                        Layout.fillWidth: true
                    }
                    DarkTextField {
                        id: addNickname
                        placeholderText: "nickname"
                        Layout.preferredWidth: 110
                    }
                    DarkTextField {
                        id: addCaps
                        placeholderText: "caps (comma-sep, blank=any)"
                        Layout.preferredWidth: 200
                    }
                    DarkPrimaryButton {
                        text: "Add"
                        enabled: addPubkey.text.length > 0 && addNickname.text.length > 0
                        onClicked: {
                            const raw = logos.callModule("agent", "trust_add",
                                [addPubkey.text.trim(), addNickname.text.trim(),
                                 addCaps.text.trim(), ""]);
                            const obj = root.parseModuleJson(raw);
                            if (obj && obj.error) {
                                console.warn("trust_add:", obj.error);
                                return;
                            }
                            addPubkey.text = "";
                            addNickname.text = "";
                            addCaps.text = "";
                            trustCol.refreshTrust();
                        }
                    }
                }

                // Trust list (table-ish) — fillHeight inside the bounded
                // parent pane, scrolls when there are more entries than fit.
                ListView {
                    id: trustList
                    Layout.fillWidth: true
                    Layout.fillHeight: true
                    clip: true
                    spacing: 4
                    boundsBehavior: Flickable.StopAtBounds
                    ScrollBar.vertical: ScrollBar { policy: ScrollBar.AsNeeded }
                    model: ListModel { id: trustModel }

                    delegate: Rectangle {
                        width: ListView.view.width
                        height: 38
                        color: theme.backgroundElevated
                        radius: 4
                        border.color: theme.borderSubtle
                        border.width: 1

                        RowLayout {
                            anchors.fill: parent
                            anchors.margins: 8
                            spacing: 8

                            Text {
                                text: model.nickname
                                color: theme.successSoft
                                font.pixelSize: 12
                                font.weight: Font.DemiBold
                                Layout.preferredWidth: 100
                                elide: Text.ElideRight
                            }
                            Text {
                                text: root.shorten(model.pubkey, 18)
                                color: theme.textMuted
                                font.pixelSize: 10
                                font.family: "monospace"
                                Layout.preferredWidth: 160
                            }
                            Text {
                                text: "caps: " +
                                      ((model.capabilities && model.capabilities.length > 0)
                                          ? Array.from(model.capabilities).join(", ")
                                          : "(any)")
                                color: theme.textSecondary
                                font.pixelSize: 10
                                Layout.fillWidth: true
                                elide: Text.ElideRight
                            }
                            DarkButton {
                                text: "Remove"
                                onClicked: {
                                    const raw = logos.callModule("agent", "trust_remove",
                                                                 [model.pubkey]);
                                    const obj = root.parseModuleJson(raw);
                                    if (obj && obj.error) {
                                        console.warn("trust_remove:", obj.error);
                                        return;
                                    }
                                    trustCol.refreshTrust();
                                }
                            }
                        }
                    }
                }

                Text {
                    visible: trustModel.count === 0
                    text: trustCol.trustMode === "off"
                          ? "Trust mode is OFF — every peer is accepted. Add an entry to enable filtering."
                          : "No trusted peers yet — add one above."
                    color: theme.textMuted
                    font.pixelSize: 11
                    font.italic: true
                    Layout.alignment: Qt.AlignHCenter
                }

                Text {
                    visible: trustCol.trustError.length > 0
                    text: "Error: " + trustCol.trustError
                    color: theme.error
                    font.pixelSize: 11
                }
            }
        }
        } // end StackLayout
    }
}
