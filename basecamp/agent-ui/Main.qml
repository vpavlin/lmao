import QtQuick
import QtQuick.Controls
import QtQuick.Layouts

// LMAO agent UI — talks to the `agent` core module which proxies a
// running `lmao agent run` daemon over Unix-socket IPC. All operations
// route through `logos.callModule("agent", method, args)`.
//
// Four panes:
//   1. Status     — daemon identity, uptime, capabilities
//   2. Peers      — live PeerMap from presence broadcasts, capability filter
//   3. Delegate   — capability + text → routed task → response
//   4. Audit      — paste a codex:// CID, fetch the bytes
Item {
    id: root

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

    function refreshStatus() {
        const raw = logos.callModule("agent", "info", []);
        const obj = parseModuleJson(raw);
        if (!obj || obj.error) {
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
    }
    Timer {
        id: statusTimer
        interval: 5000
        repeat: true
        onTriggered: root.refreshStatus()
    }

    ColumnLayout {
        anchors.fill: parent
        anchors.margins: 16
        spacing: 12

        // ── Header ──────────────────────────────────────────────
        RowLayout {
            Layout.fillWidth: true

            ColumnLayout {
                Layout.fillWidth: true
                spacing: 2

                Text {
                    text: "LMAO Agent"
                    font.pixelSize: 22
                    font.weight: Font.DemiBold
                    color: "#ffffff"
                }
                Text {
                    text: "A2A coordination over Logos Messaging — local, decentralized, verifiable"
                    font.pixelSize: 11
                    color: "#8b949e"
                }
            }

            // Status badge
            Rectangle {
                Layout.preferredWidth: badge.implicitWidth + 16
                Layout.preferredHeight: 24
                radius: 12
                color: root.statusError ? "#572421" : "#1a3f2e"
                border.color: root.statusError ? "#f85149" : "#56d364"
                border.width: 1

                Row {
                    id: badge
                    anchors.centerIn: parent
                    spacing: 6

                    Rectangle {
                        width: 8; height: 8; radius: 4
                        anchors.verticalCenter: parent.verticalCenter
                        color: root.statusError ? "#f85149" : "#56d364"
                    }
                    Text {
                        text: root.statusError ? "daemon offline" : "daemon ready"
                        color: root.statusError ? "#f85149" : "#56d364"
                        font.pixelSize: 11
                        anchors.verticalCenter: parent.verticalCenter
                    }
                }
            }
        }

        // ── Pane 1: Status ──────────────────────────────────────
        Rectangle {
            Layout.fillWidth: true
            Layout.preferredHeight: statusGrid.implicitHeight + 24
            color: "#161b22"
            radius: 6
            border.color: "#30363d"
            border.width: 1

            GridLayout {
                id: statusGrid
                anchors.fill: parent
                anchors.margins: 12
                columns: 2
                columnSpacing: 16
                rowSpacing: 4

                Text { text: "Name";        color: "#8b949e"; font.pixelSize: 12 }
                Text { text: root.statusName || "—"; color: "#ffffff"; font.pixelSize: 12 }

                Text { text: "Public key";  color: "#8b949e"; font.pixelSize: 12 }
                Text { text: root.shorten(root.statusPubkey, 40) || "—"
                       color: "#7ee787"; font.pixelSize: 12; font.family: "monospace" }

                Text { text: "Capabilities"; color: "#8b949e"; font.pixelSize: 12 }
                Text { text: root.statusCapabilities.join(", ") || "—"
                       color: "#ffffff"; font.pixelSize: 12 }

                Text { text: "Uptime";       color: "#8b949e"; font.pixelSize: 12 }
                Text { text: root.statusUptimeSecs + " s";  color: "#ffffff"; font.pixelSize: 12 }

                Text { text: "Storage";      color: "#8b949e"; font.pixelSize: 12 }
                Text { text: root.statusStorageEnabled ? "enabled (libstorage)" : "disabled"
                       color: root.statusStorageEnabled ? "#7ee787" : "#f0883e"; font.pixelSize: 12 }
            }
        }

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
                color: "#161b22"
                radius: 6
                border.color: "#30363d"
                border.width: 1

                ColumnLayout {
                    anchors.fill: parent
                    anchors.margins: 12
                    spacing: 8

                    RowLayout {
                        Layout.fillWidth: true
                        Text {
                            text: "Peers"
                            color: "#ffffff"
                            font.pixelSize: 14
                            font.weight: Font.DemiBold
                            Layout.fillWidth: true
                        }
                        TextField {
                            id: peersFilter
                            placeholderText: "filter capability"
                            Layout.preferredWidth: 140
                        }
                        Button {
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
                            color: "#0d1117"
                            radius: 4
                            border.color: "#21262d"
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
                                    color: "#7ee787"
                                    font.pixelSize: 12
                                    font.weight: Font.DemiBold
                                }
                                Text {
                                    text: "caps: " + (model.capabilities || []).join(", ")
                                    color: "#8b949e"
                                    font.pixelSize: 10
                                }
                                Text {
                                    text: root.shorten(model.agent_id || "", 32)
                                    color: "#6e7681"
                                    font.pixelSize: 10
                                    font.family: "monospace"
                                }
                            }
                        }
                    }

                    Text {
                        visible: peersModel.count === 0
                        text: "No live peers yet — try a filter or refresh."
                        color: "#6e7681"
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
                color: "#161b22"
                radius: 6
                border.color: "#30363d"
                border.width: 1

                ColumnLayout {
                    anchors.fill: parent
                    anchors.margins: 12
                    spacing: 8

                    Text {
                        text: "Delegate task"
                        color: "#ffffff"
                        font.pixelSize: 14
                        font.weight: Font.DemiBold
                    }

                    RowLayout {
                        Layout.fillWidth: true
                        Text {
                            text: "Capability"
                            color: "#8b949e"
                            font.pixelSize: 12
                            Layout.preferredWidth: 80
                        }
                        TextField {
                            id: delegateCap
                            Layout.fillWidth: true
                            placeholderText: "e.g. code, summarize, text"
                        }
                    }

                    Text {
                        text: "Task text"
                        color: "#8b949e"
                        font.pixelSize: 12
                    }
                    ScrollView {
                        Layout.fillWidth: true
                        Layout.preferredHeight: 80

                        TextArea {
                            id: delegateText
                            placeholderText: "What do you want a peer to do?"
                            wrapMode: TextArea.Wrap
                            background: Rectangle { color: "#0d1117"; border.color: "#21262d"; radius: 4 }
                            color: "#ffffff"
                        }
                    }

                    Button {
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
                        color: "#7ee787"
                        font.pixelSize: 12
                        wrapMode: Text.Wrap
                    }
                    Text {
                        id: delegateCidLink
                        Layout.fillWidth: true
                        text: ""
                        visible: text.length > 0
                        color: "#79c0ff"
                        font.pixelSize: 10
                        font.family: "monospace"
                        wrapMode: Text.Wrap
                    }
                }
            }
        }

        // ── Pane 4: Audit ──────────────────────────────────────
        Rectangle {
            Layout.fillWidth: true
            Layout.preferredHeight: 130
            color: "#161b22"
            radius: 6
            border.color: "#30363d"
            border.width: 1

            ColumnLayout {
                anchors.fill: parent
                anchors.margins: 12
                spacing: 6

                RowLayout {
                    Layout.fillWidth: true

                    Text {
                        text: "Audit log fetch"
                        color: "#ffffff"
                        font.pixelSize: 14
                        font.weight: Font.DemiBold
                        Layout.fillWidth: true
                    }
                    TextField {
                        id: cidInput
                        placeholderText: "codex://CID (paste here)"
                        Layout.fillWidth: true
                    }
                    Button {
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

                ScrollView {
                    Layout.fillWidth: true
                    Layout.fillHeight: true
                    TextArea {
                        id: cidOut
                        readOnly: true
                        placeholderText: "Fetched payload appears here."
                        wrapMode: TextArea.Wrap
                        background: Rectangle { color: "#0d1117"; border.color: "#21262d"; radius: 4 }
                        color: "#ffffff"
                        font.family: "monospace"
                        font.pixelSize: 11
                    }
                }
            }
        }
    }
}
