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
        // Reserve breathing room top + bottom so the text doesn't
        // hug the rounded border (the editable mode's internal
        // TextField is what tightens it). Inheritable padding props
        // — picked up by both the contentItem and Qt's editable
        // TextField path.
        topPadding: 6
        bottomPadding: 6
        leftPadding: theme.spaceSmall + 2
        rightPadding: 24    // room for the caret indicator

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
            elide: Text.ElideRight
            // Padding handled by the parent ComboBox.
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
            // Don't shrink-wrap to the combo's parent layout — the
            // capability popup should always be wide enough to show
            // full strings comfortably even when the combo itself
            // sits in a tight row.
            width: Math.max(dcb.width, 220)
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
            width: parent ? parent.width : dcb.width
            height: 28
            highlighted: dcb.highlightedIndex === index
            contentItem: Text {
                text: modelData
                color: theme.text
                font: dcb.font
                verticalAlignment: Text.AlignVCenter
                leftPadding: theme.spaceSmall
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

    /// Look up a peer's display name by pubkey from the live peer
    /// list. Used in the task cards so completed delegations show
    /// "→ alice" instead of the unfriendly "→ 0242c474c3c0a4…" hex.
    /// Falls back to a shortened pubkey when the peer isn't in the
    /// current map (likely went offline since the task was sent).
    function peerLabel(pubkey) {
        if (!pubkey) return "";
        for (let i = 0; i < peersModel.count; i++) {
            const p = peersModel.get(i);
            if (p.agent_id === pubkey && p.name) return p.name;
        }
        return shorten(pubkey, 14);
    }

    // Live capability index, deduped + sorted from the latest peers
    // refresh. Drives the Delegate-pane combo so the operator picks
    // from what's actually online rather than guessing capability
    // strings.
    property var availableCapabilities: []

    // One-shot session id consumed by the next Delegate click. Set by
    // the "Follow up" button on a finished task card; cleared in
    // delegateBtn.onClicked so the next ad-hoc delegate starts fresh.
    // When non-empty, the receiver's exec runs with LMAO_SESSION_ID set
    // and reuses a per-thread session (pi --session, lemonade history).
    property string pendingSessionId: ""

    // Transient hint shown next to the Delegate input after the
    // operator clicks a peer in the Peers list. Cleared by the timer
    // below so it doesn't linger forever.
    property string targetedPeerName: ""
    Timer {
        id: targetedPeerHintTimer
        interval: 4000
        onTriggered: root.targetedPeerName = ""
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
        // First time the daemon comes up — pull persisted history.
        // refreshStatus is called every 5s by statusTimer, so this
        // also covers the case where the daemon was offline at QML
        // load time.
        if (!historyLoaded) loadHistory();
    }

    Component.onCompleted: {
        refreshStatus();
        statusTimer.start();
        // Faster poll while the daemon is starting up — the operator
        // sees the badge flip to "ready" within a second or two of the
        // socket appearing rather than waiting for the next 5 s tick.
        startingPollTimer.start();
        // Subscribe to async-completion events from the agent module.
        // Required before any start_delegate / start_fetch_cid call:
        // the Logos QML bridge only forwards events the consumer has
        // explicitly registered for.
        if (typeof logos !== "undefined" && logos.onModuleEvent) {
            logos.onModuleEvent("agent", "delegate_complete");
            logos.onModuleEvent("agent", "fetch_cid_complete");
        }
        // Try to load persisted history. Daemon may not be ready yet —
        // historyPollTimer retries until it gets a parseable response.
        loadHistory();
    }

    // ── History bootstrap ────────────────────────────────────────
    // The daemon owns task history (JSONL log next to its storage dir).
    // On startup we ask for the most recent N rows and seed tasksModel
    // with them so the operator sees their previous tasks across
    // basecamp restarts. Live updates from delegate_complete still
    // append at index 0 the same way.

    property bool historyLoaded: false

    function loadHistory() {
        if (historyLoaded) return;
        if (root.daemonState !== "ready") return;  // wait for daemon
        const raw = logos.callModule("agent", "task_history_list",
                                     [50, 0, "delegated", ""]);
        const obj = root.parseModuleJson(raw);
        if (!obj || obj.error) {
            console.warn("agent_ui: history load failed:",
                         obj && obj.error ? obj.error : raw);
            return;
        }
        const entries = obj.entries || [];
        // tasksModel is empty at this point (fresh QML). Append in
        // arrival order; reorderTasksBySession() at the end groups
        // session blocks contiguously and bubbles the latest thread
        // to the top.
        for (let i = 0; i < entries.length; i++) {
            const e = entries[i];
            tasksModel.append({
                task_id:     e.task_id || "",
                status:      e.success ? "done" : "error",
                capability:  e.capability || "",
                text:        e.text || "",
                agent_id:    e.peer_pubkey || "",
                body:        e.body || "",
                cid:         e.cid || "",
                cidPayload:  "",
                cidLoading:  false,
                cidExpanded: false,
                elapsedSecs: ((e.elapsed_ms || 0) / 1000).toFixed(1),
                error:       e.error || "",
                startedAt:   e.created_at_ms || 0,
                // Field must always be present so the QML model
                // declares the role (otherwise delegate onClicked
                // hits ReferenceError when reading it). Real session
                // ids come from the daemon's HistoryEntry.session_id.
                session_id:  e.session_id || ""
            });
        }
        reorderTasksBySession();
        historyLoaded = true;
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

    // ── Async event dispatcher ───────────────────────────────────
    // The C++ module's `start_delegate` and `start_fetch_cid` return
    // immediately with a request_id; the actual IPC runs on a worker
    // thread and fires `<name>_complete` events here when done. Each
    // event carries a JSON payload (wrapped as data[0] by the
    // universal-module bridge); we route by event name and update the
    // tasksModel by task_id / cid.
    Connections {
        target: logos
        ignoreUnknownSignals: true
        function onModuleEventReceived(moduleName, eventName, data) {
            if (moduleName !== "agent") return;
            const raw = (data && data.length > 0)
                ? (typeof data[0] === "string" ? data[0] : JSON.stringify(data[0]))
                : "";
            const obj = root.parseModuleJson(raw);
            if (!obj) return;
            if (eventName === "delegate_complete") {
                root.handleDelegateComplete(obj);
            } else if (eventName === "fetch_cid_complete") {
                root.handleFetchCidComplete(obj);
            }
        }
    }

    // Tasks: a single ListModel for both running and completed
    // delegations. Each row carries everything the UI needs to render
    // it. Order is "session-grouped, recency-first": same-session
    // cards are adjacent so they read as a thread; threads are sorted
    // so the most recently active one floats to the top. Mutate via
    // insertTaskInOrder()/reorderTasksBySession() — never `insert(0)`
    // directly, or the visual threading breaks.
    ListModel { id: tasksModel }

    // ── Session grouping helpers ────────────────────────────────────
    //
    // We don't change the row schema or build a derived model — too
    // invasive given the size of the existing card delegate. Instead
    // tasksModel is kept session-grouped so the existing flat ListView
    // *looks* threaded, and the delegate adds head/tail decoration by
    // peeking at adjacent rows.

    function _sessionKey(t) {
        if (!t) return "";
        if (t.session_id) return t.session_id;
        // Backward-compat: tasks created before auto-session-stamping
        // landed have an empty session_id. If any newer task in the
        // model references THIS task's task_id as their session_id
        // (i.e. they were a Follow-up of us), treat us as the head of
        // that thread by adopting our task_id as the session key.
        for (let i = 0; i < tasksModel.count; i++) {
            const r = tasksModel.get(i);
            if (r && r.session_id && r.session_id === t.task_id) {
                return t.task_id;
            }
        }
        // Otherwise it's a true single-shot. "solo:<id>" keeps it in
        // its own group so the chain visuals don't kick in.
        return "solo:" + t.task_id;
    }

    /// Count how many turns share `key` in the current tasksModel.
    /// Used by the head-card's thread badge.
    function _threadTurnCount(key) {
        let n = 0;
        for (let i = 0; i < tasksModel.count; i++) {
            if (_sessionKey(tasksModel.get(i)) === key) n++;
        }
        return n;
    }

    /// Insert a freshly-created task so its session block stays
    /// contiguous and bubbles to the top. Handles three cases:
    ///   1. New session: prepend, becomes the top thread.
    ///   2. Existing session: append at the end of that block, then
    ///      bubble the whole block to the top of the list.
    function insertTaskInOrder(t) {
        const sid = _sessionKey(t);
        let firstIdx = -1, lastIdx = -1;
        for (let i = 0; i < tasksModel.count; i++) {
            const k = _sessionKey(tasksModel.get(i));
            if (k === sid) {
                if (firstIdx === -1) firstIdx = i;
                lastIdx = i;
            }
        }
        if (firstIdx === -1) {
            tasksModel.insert(0, t);
            return;
        }
        const insertAt = lastIdx + 1;
        tasksModel.insert(insertAt, t);
        // Block now occupies [firstIdx .. insertAt]. Bubble it to top.
        const blockLen = insertAt - firstIdx + 1;
        if (firstIdx > 0) {
            tasksModel.move(firstIdx, 0, blockLen);
        }
    }

    /// One-shot reorder used after batch loads (history). Sorts so
    /// (1) sessions are grouped contiguously and (2) sessions sort by
    /// most recent turn descending.
    function reorderTasksBySession() {
        if (tasksModel.count <= 1) return;
        const rows = [];
        const recency = {};
        for (let i = 0; i < tasksModel.count; i++) {
            const r = tasksModel.get(i);
            const copy = {};
            // Copy each role explicitly — JSON.parse(stringify(get(i)))
            // drops nested QML types defensively.
            const keys = ["task_id", "status", "capability", "text",
                          "agent_id", "body", "cid", "cidPayload",
                          "cidLoading", "cidExpanded", "elapsedSecs",
                          "error", "startedAt", "session_id"];
            for (const k of keys) copy[k] = r[k];
            const sk = _sessionKey(r);
            copy._sk = sk;
            rows.push(copy);
            const t = r.startedAt || 0;
            if (!recency[sk] || t > recency[sk]) recency[sk] = t;
        }
        rows.sort(function(a, b) {
            if (recency[b._sk] !== recency[a._sk]) {
                return recency[b._sk] - recency[a._sk];
            }
            return (a.startedAt || 0) - (b.startedAt || 0);
        });
        tasksModel.clear();
        for (const r of rows) {
            delete r._sk;
            tasksModel.append(r);
        }
    }

    function handleDelegateComplete(obj) {
        if (!obj.task_id) return;
        for (let i = 0; i < tasksModel.count; i++) {
            if (tasksModel.get(i).task_id !== obj.task_id) continue;

            let body = obj.body || "";
            // Strip the trailing "execution log: codex://..." footer
            // — surfaced separately as a CID chip.
            body = body.replace(/\s*[-]+\s*execution log:\s*codex:\/\/[A-Za-z0-9]+\s*$/, "");

            tasksModel.setProperty(i, "status",
                obj.success ? "done" : "error");
            tasksModel.setProperty(i, "agent_id", obj.agent_id || "");
            tasksModel.setProperty(i, "body", body.trim());
            tasksModel.setProperty(i, "cid", obj.cid || "");
            tasksModel.setProperty(i, "elapsedSecs",
                ((obj.elapsed_ms || 0) / 1000).toFixed(1));
            tasksModel.setProperty(i, "error", obj.error || "");

            // Audit-log fetch is opt-in — click "View log". We previously
            // auto-prefetched here, but calling start_fetch_cid synchronously
            // from inside the moduleEventReceived handler hung the agent's
            // logos_host reply path (the InvokeReplyPacket never went out and
            // every subsequent callModule queued for 20s). Triggering on user
            // click sidesteps the re-entrant call.
            return;
        }
    }

    function handleFetchCidComplete(obj) {
        if (!obj.cid) return;
        let payload = "";
        if (obj.success) {
            const raw = obj.payload_b64 || "";
            console.log("[fetch_cid] payload_b64 len=" + raw.length
                + " typeof=" + (typeof raw)
                + " head=" + raw.substring(0, 80));
            payload = root._decodeBase64Utf8(raw);
        } else {
            payload = "Error: " + (obj.error || "fetch failed");
        }
        // Update every task that's waiting on this CID.
        for (let i = 0; i < tasksModel.count; i++) {
            const t = tasksModel.get(i);
            if (t.cid === obj.cid) {
                tasksModel.setProperty(i, "cidPayload", payload);
                tasksModel.setProperty(i, "cidLoading", false);
            }
        }
    }

    // Standard base64 alphabet for the pure-JS decoder. Index = value.
    readonly property string _b64Alphabet:
        "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/"

    /// Decode a base64 string into UTF-8 text. We can't rely on the
    /// browser-only `atob()` here — QML's JS engine doesn't expose it,
    /// so the obvious `atob` call throws ReferenceError.
    /// Implement the decode in pure JS, then UTF-8-decode the byte
    /// stream via the classic `decodeURIComponent(escape(...))` trick.
    function _decodeBase64Utf8(b64) {
        if (!b64) return "";
        // Strip whitespace + URL-safe variants the daemon shouldn't
        // emit but might in the future.
        let s = b64.replace(/[\r\n\t ]/g, "")
                   .replace(/-/g, "+").replace(/_/g, "/");
        // Strip trailing `=` padding for parsing simplicity (we drive
        // the loop by index, padding doesn't carry data).
        while (s.length > 0 && s[s.length - 1] === "=") {
            s = s.substring(0, s.length - 1);
        }
        const alpha = root._b64Alphabet;
        const lookup = {};
        for (let i = 0; i < alpha.length; i++) lookup[alpha[i]] = i;
        let bin = "";
        let buf = 0, bits = 0;
        for (let i = 0; i < s.length; i++) {
            const v = lookup[s[i]];
            if (v === undefined) {
                // Unknown char — bail out so caller sees something.
                return "(unrecognised character in base64 at offset " + i + ")";
            }
            buf = (buf << 6) | v;
            bits += 6;
            if (bits >= 8) {
                bits -= 8;
                bin += String.fromCharCode((buf >> bits) & 0xff);
            }
        }
        // bin is a "binary string" — each char is one byte. Convert
        // it to UTF-8 via the escape/decodeURIComponent trick. If the
        // bytes happen to not be valid UTF-8, fall back to the raw
        // string so we still show something.
        try {
            return decodeURIComponent(escape(bin));
        } catch (e) {
            return bin;
        }
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
                    font.pixelSize: tab.font.pixelSize
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

        // Task history — in-memory across the session. Each successful
        // delegation appends a row; each row remembers the inputs and
        // the response so the operator can pick an old task and run it
        // again or follow up.

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

            // ── Peers pane (sidebar — narrow column) ──
            Rectangle {
                Layout.fillWidth: false
                Layout.fillHeight: true
                Layout.preferredWidth: 320
                Layout.minimumWidth: 280
                Layout.maximumWidth: 380
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
                        DarkComboBox {
                            id: peersFilter
                            Layout.preferredWidth: 160
                            // "" = no filter, then each available
                            // capability. Refresh fires on selection
                            // change — keeps the list responsive.
                            model: [""].concat(root.availableCapabilities)
                            displayText: currentText.length === 0
                                ? "all capabilities"
                                : "cap: " + currentText
                            // Helper used by the refresh() function to
                            // pass the same arg shape as the old
                            // text field (empty = no filter).
                            property string text: currentText
                            onActivated: peersList.refresh()
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
                                root.availableCapabilities = [];
                                return;
                            }
                            const peers = obj.peers || [];
                            const seen = {};
                            for (let i = 0; i < peers.length; i++) {
                                const p = peers[i];
                                // Pre-flatten capabilities into a comma-string
                                // because ListModel.append loses nested arrays
                                // through the universal-module bridge.
                                const caps = Array.isArray(p.capabilities)
                                    ? p.capabilities
                                    : (p.capabilities ? [p.capabilities] : []);
                                const load = p.load || {};
                                peersModel.append({
                                    name: p.name || "",
                                    agent_id: p.agent_id || "",
                                    capsCsv: caps.join(", "),
                                    firstCap: caps[0] || "",
                                    loadBucket: load.bucket || "",
                                    queueDepth: load.queue_depth || 0,
                                    maxConcurrent: load.max_concurrent || 0,
                                });
                                for (let j = 0; j < caps.length; j++) {
                                    seen[caps[j]] = true;
                                }
                            }
                            // Capabilities surfaced as a deduped, sorted list
                            // so the Delegate-pane combo stays stable as
                            // peers come and go.
                            root.availableCapabilities = Object.keys(seen).sort();
                        }

                        delegate: Rectangle {
                            id: peerRow
                            width: ListView.view.width
                            height: peerCol.implicitHeight + 16
                            color: peerArea.containsMouse
                                ? Qt.lighter(theme.backgroundElevated, 1.3)
                                : theme.backgroundElevated
                            radius: theme.radiusMedium
                            border.color: peerArea.containsMouse ? theme.primary : theme.borderSubtle
                            border.width: 1

                            // Click-to-target: prefill the Delegate
                            // pane's Capability with this peer's first
                            // capability AND raise a brief "→ targeting
                            // <peer>" hint so the click feels alive.
                            // The trust filter still applies — peers
                            // that aren't trusted will be skipped even
                            // if "selected" here.
                            MouseArea {
                                id: peerArea
                                anchors.fill: parent
                                hoverEnabled: true
                                cursorShape: Qt.PointingHandCursor
                                onClicked: {
                                    if (model.firstCap) {
                                        delegateCap.setText(model.firstCap);
                                    }
                                    root.targetedPeerName = model.name || "";
                                    targetedPeerHintTimer.restart();
                                    delegateText.forceActiveFocus();
                                }
                            }

                            ColumnLayout {
                                id: peerCol
                                anchors.left: parent.left
                                anchors.right: parent.right
                                anchors.verticalCenter: parent.verticalCenter
                                anchors.leftMargin: 10
                                anchors.rightMargin: 10
                                spacing: 4

                                RowLayout {
                                    Layout.fillWidth: true
                                    spacing: 6

                                    Text {
                                        text: model.name
                                        color: theme.successSoft
                                        font.pixelSize: 13
                                        font.weight: Font.DemiBold
                                    }
                                    Rectangle {
                                        // Capacity chip — only shown when the
                                        // peer has shipped a sealed envelope
                                        // we could decrypt. "Free" is green,
                                        // "Busy" amber, "Full" red.
                                        visible: model.loadBucket && model.loadBucket.length > 0
                                        radius: 3
                                        implicitWidth: loadLabel.implicitWidth + 10
                                        implicitHeight: loadLabel.implicitHeight + 4
                                        color: {
                                            if (model.loadBucket === "free") return Qt.rgba(0.49, 0.83, 0.39, 0.18);
                                            if (model.loadBucket === "busy") return Qt.rgba(0.95, 0.70, 0.20, 0.20);
                                            if (model.loadBucket === "full") return Qt.rgba(0.95, 0.30, 0.30, 0.22);
                                            return "transparent";
                                        }
                                        border.width: 1
                                        border.color: {
                                            if (model.loadBucket === "free") return Qt.rgba(0.49, 0.83, 0.39, 0.5);
                                            if (model.loadBucket === "busy") return Qt.rgba(0.95, 0.70, 0.20, 0.6);
                                            if (model.loadBucket === "full") return Qt.rgba(0.95, 0.30, 0.30, 0.7);
                                            return "transparent";
                                        }
                                        Text {
                                            id: loadLabel
                                            anchors.centerIn: parent
                                            text: model.loadBucket + " " +
                                                  model.queueDepth + "/" + model.maxConcurrent
                                            color: theme.text
                                            font.pixelSize: 9
                                            font.weight: Font.Medium
                                        }
                                    }
                                    Text {
                                        text: "·"
                                        color: theme.textMuted
                                        font.pixelSize: 12
                                        visible: capsRow.children.length > 0
                                    }
                                    Flow {
                                        id: capsRow
                                        Layout.fillWidth: true
                                        spacing: 4

                                        Repeater {
                                            model: model.capsCsv
                                                ? model.capsCsv.split(", ").filter(s => s.length)
                                                : []
                                            delegate: Rectangle {
                                                radius: 3
                                                color: Qt.rgba(0.49, 0.83, 0.39, 0.12)  // soft green tint
                                                border.color: Qt.rgba(0.49, 0.83, 0.39, 0.4)
                                                border.width: 1
                                                implicitWidth: capLabel.implicitWidth + 10
                                                implicitHeight: capLabel.implicitHeight + 4
                                                Text {
                                                    id: capLabel
                                                    anchors.centerIn: parent
                                                    text: modelData
                                                    color: theme.successSoft
                                                    font.pixelSize: 9
                                                    font.weight: Font.Medium
                                                }
                                            }
                                        }
                                    }
                                }
                                Text {
                                    text: model.agent_id
                                    color: theme.textMuted
                                    font.pixelSize: 10
                                    font.family: "monospace"
                                    elide: Text.ElideMiddle
                                    Layout.fillWidth: true
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

            // ── Delegate + Tasks pane (hero — fills remaining width) ──
            Rectangle {
                Layout.fillWidth: true
                Layout.fillHeight: true
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
                            text: "Delegate task"
                            color: theme.text
                            font.pixelSize: 14
                            font.weight: Font.DemiBold
                            Layout.fillWidth: true
                        }
                        Text {
                            // Brief flash when a peer is clicked in
                            // the left pane — gives the click feedback
                            // and tells the operator their next
                            // delegate will favour that peer.
                            visible: root.targetedPeerName.length > 0
                            text: "→ targeting " + root.targetedPeerName
                            color: theme.primary
                            font.pixelSize: 11
                            font.italic: true
                        }
                    }

                    RowLayout {
                        Layout.fillWidth: true
                        Text {
                            text: "Capability"
                            color: theme.textSecondary
                            font.pixelSize: 12
                            Layout.preferredWidth: 80
                        }
                        // Capability picker. Editable so the operator
                        // can still type a capability that isn't in
                        // the live peer list (useful for offline-first
                        // demos where the receiver hasn't announced
                        // yet). Exposes a `text` alias matching the
                        // old TextField API so call sites keep working.
                        DarkComboBox {
                            id: delegateCap
                            Layout.fillWidth: true
                            editable: true
                            model: root.availableCapabilities
                            // Auto-select the first available capability
                            // once peers have announced and the operator
                            // hasn't typed anything yet.
                            onModelChanged: {
                                if (editText.length === 0
                                    && root.availableCapabilities.length > 0) {
                                    editText = root.availableCapabilities[0];
                                }
                            }
                            // ComboBox uses `editText` for the editable
                            // text. Old call sites used `.text =` /
                            // `.text.length`. Forward to editText.
                            function setText(s) { editText = s }
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

                    // Follow-up banner — visible only after the operator
                    // clicks "Follow up" on a finished task card. Shows
                    // which thread the next Delegate click will join, and
                    // gives them a Cancel out so they can fire a fresh
                    // task instead.
                    Rectangle {
                        Layout.fillWidth: true
                        visible: root.pendingSessionId.length > 0
                        Layout.preferredHeight: visible ? 30 : 0
                        radius: theme.radiusSmall
                        color: Qt.rgba(0.93, 0.48, 0.35, 0.10)  // primary tint
                        border.color: theme.primary
                        border.width: 1

                        RowLayout {
                            anchors.fill: parent
                            anchors.leftMargin: 8
                            anchors.rightMargin: 4
                            spacing: 8

                            Text {
                                text: "↪  Following up on " +
                                      root.shorten(root.pendingSessionId, 12) +
                                      " — receiver reuses the conversation"
                                color: theme.primary
                                font.pixelSize: 11
                                Layout.fillWidth: true
                                elide: Text.ElideRight
                            }
                            Text {
                                text: "Cancel"
                                color: theme.primary
                                font.pixelSize: 11
                                font.underline: true
                                MouseArea {
                                    anchors.fill: parent
                                    cursorShape: Qt.PointingHandCursor
                                    onClicked: root.pendingSessionId = ""
                                }
                            }
                        }
                    }

                    // Delegate launcher — fire-and-forget. The
                    // start_delegate IPC returns instantly with a
                    // task_id; the work runs on a worker thread and
                    // emits "delegate_complete" when done. Multiple
                    // tasks can be in flight simultaneously.
                    RowLayout {
                        Layout.fillWidth: true
                        spacing: theme.spaceMedium

                        DarkPrimaryButton {
                            id: delegateBtn
                            // Button label changes to "Follow up" when
                            // pendingSessionId is armed so the operator
                            // sees what the click is going to do.
                            text: root.pendingSessionId.length > 0 ? "Follow up" : "Delegate"
                            enabled: delegateCap.editText.length > 0
                                     && delegateText.text.length > 0
                                     && root.daemonState === "ready"

                            onClicked: {
                                // Pending-session-id is set by "Follow up"
                                // (line below) so the receiver reuses the
                                // same conversation thread instead of
                                // cold-starting. Cleared after each click
                                // so the next ad-hoc delegate is fresh.
                                const sid = root.pendingSessionId;
                                root.pendingSessionId = "";
                                const ackRaw = logos.callModule("agent", "start_delegate",
                                    [delegateCap.editText, delegateText.text, sid]);
                                const ack = root.parseModuleJson(ackRaw);
                                if (!ack || ack.error || !ack.task_id) {
                                    // Surface the failure as a synthetic
                                    // task card so the user sees something.
                                    insertTaskInOrder({
                                        task_id: "err-" + Date.now(),
                                        status: "error",
                                        capability: delegateCap.editText,
                                        text: delegateText.text,
                                        agent_id: "",
                                        body: "",
                                        cid: "",
                                        cidPayload: "",
                                        cidLoading: false,
                                        cidExpanded: false,
                                        elapsedSecs: "0",
                                        error: (ack && ack.error)
                                            ? ack.error : "no task_id from agent",
                                        startedAt: Date.now(),
                                        session_id: sid
                                    });
                                    return;
                                }
                                insertTaskInOrder({
                                    task_id: ack.task_id,
                                    status: "running",
                                    capability: delegateCap.editText,
                                    text: delegateText.text,
                                    agent_id: "",
                                    body: "",
                                    cid: "",
                                    cidPayload: "",
                                    cidLoading: false,
                                    cidExpanded: false,
                                    elapsedSecs: "0",
                                    error: "",
                                    startedAt: Date.now(),
                                    // Use the agent-module's echoed
                                    // session_id — auto-stamped when sid
                                    // was empty so Follow up has a real
                                    // session to attach to.
                                    session_id: ack.session_id || sid
                                });
                                // Clear the task text so the user can
                                // start typing the next one immediately.
                                delegateText.text = "";
                            }
                        }
                        Text {
                            color: theme.textMuted
                            font.pixelSize: theme.fontSmall
                            text: tasksModel.count + (tasksModel.count === 1 ? " task" : " tasks")
                            Layout.leftMargin: theme.spaceSmall
                        }
                        Item { Layout.fillWidth: true }
                        DarkButton {
                            text: "Clear done"
                            enabled: {
                                for (let i = 0; i < tasksModel.count; i++) {
                                    if (tasksModel.get(i).status !== "running") return true;
                                }
                                return false;
                            }
                            onClicked: {
                                for (let i = tasksModel.count - 1; i >= 0; i--) {
                                    if (tasksModel.get(i).status !== "running") {
                                        tasksModel.remove(i);
                                    }
                                }
                            }
                        }
                    }

                    // ── Tasks list (running + completed) ───────────
                    // Each card renders one delegation. New ones land
                    // at index 0 in "running" state; the
                    // delegate_complete handler flips status + fills
                    // body/cid in place. Multiple cards can be
                    // running at once.
                    // Wrap the ListView in a sized container so the
                    // surrounding ColumnLayout gives it a bounded
                    // height. A bare `Layout.fillHeight: true` on a
                    // ListView inside a ColumnLayout sometimes lets
                    // the implicit content height override the
                    // available space, and the cards spill off the
                    // visible area instead of scrolling.
                    Rectangle {
                        Layout.fillWidth: true
                        Layout.fillHeight: true
                        Layout.minimumHeight: 120
                        color: "transparent"

                    ListView {
                        id: tasksList
                        anchors.fill: parent
                        clip: true
                        spacing: 8
                        model: tasksModel
                        boundsBehavior: Flickable.StopAtBounds
                        cacheBuffer: 200
                        ScrollBar.vertical: ScrollBar {
                            policy: ScrollBar.AsNeeded
                            active: true
                        }

                        delegate: Rectangle {
                            id: taskCard
                            property bool expanded: status === "running"
                            // ── threading: peek at adjacent rows so we
                            // can render a connected-thread frame
                            // without restructuring the model. A row is
                            // a "thread head" if the row above has a
                            // different session, "tail" if the row
                            // below differs. Single-card threads are
                            // both head AND tail and render normally.
                            property string mySessionKey: root._sessionKey({
                                session_id: session_id, task_id: task_id
                            })
                            property string prevSessionKey: index > 0
                                ? root._sessionKey(tasksModel.get(index - 1))
                                : ""
                            property string nextSessionKey: index < tasksModel.count - 1
                                ? root._sessionKey(tasksModel.get(index + 1))
                                : ""
                            property bool isThreadHead: mySessionKey !== prevSessionKey
                            property bool isThreadTail: mySessionKey !== nextSessionKey
                            property bool inThread: !(isThreadHead && isThreadTail)
                            // Indent follow-up cards so the chain reads
                            // as nested under the head. Use a child
                            // wrapper that's anchored with a left
                            // margin instead of resizing the delegate
                            // itself — that way `ListView.view.width`
                            // attached-property quirks during reorder
                            // can't collapse the card to zero-width.
                            property int threadIndent: (inThread && !isThreadHead) ? 24 : 0
                            width: ListView.view ? ListView.view.width : 0
                            height: cardCol.implicitHeight + 16
                            color: "transparent"
                            border.width: 0
                            // Visual frame moved into a child rect so
                            // we can offset it by threadIndent without
                            // touching the delegate geometry. The
                            // delegate itself stays transparent and
                            // full-width.
                            Rectangle {
                                id: cardFrame
                                anchors.fill: parent
                                anchors.leftMargin: taskCard.threadIndent
                                color: theme.backgroundElevated
                                radius: theme.radiusMedium
                                border.color: status === "running" ? theme.primary
                                    : status === "error"   ? theme.error
                                    : theme.borderSubtle
                                border.width: 1

                                // Vertical connector line on the LEFT
                                // edge of follow-up cards so the chain
                                // reads visually. Drawn inside cardFrame
                                // so it inherits the indent.
                                Rectangle {
                                    visible: taskCard.inThread && !taskCard.isThreadHead
                                    width: 2
                                    color: theme.borderSubtle
                                    anchors.left: parent.left
                                    anchors.leftMargin: -14  // sits in the indent gap
                                    anchors.top: parent.top
                                    anchors.topMargin: -8    // bridge spacing above
                                    anchors.bottom: parent.bottom
                                }
                            }

                            Behavior on height { NumberAnimation { duration: 120; easing.type: Easing.OutQuad } }

                            ColumnLayout {
                                id: cardCol
                                anchors.left: cardFrame.left
                                anchors.right: cardFrame.right
                                anchors.top: cardFrame.top
                                anchors.leftMargin: 12
                                anchors.rightMargin: 12
                                anchors.topMargin: 8
                                spacing: 6

                                // ── header row: status + caps + peer + time
                                RowLayout {
                                    id: cardHeaderRow
                                    Layout.fillWidth: true
                                    spacing: 8

                                    // Thread badge — only on the head
                                    // card when this thread has > 1
                                    // turn. Inline so it doesn't add a
                                    // dedicated row of vertical space.
                                    Rectangle {
                                        visible: taskCard.isThreadHead
                                            && root._threadTurnCount(taskCard.mySessionKey) > 1
                                        radius: 3
                                        color: Qt.rgba(0.475, 0.753, 1, 0.12)
                                        border.color: theme.info
                                        border.width: 1
                                        implicitWidth: threadBadge.implicitWidth + 10
                                        implicitHeight: threadBadge.implicitHeight + 3
                                        Text {
                                            id: threadBadge
                                            anchors.centerIn: parent
                                            text: "thread · " +
                                                root._threadTurnCount(taskCard.mySessionKey) +
                                                " turns"
                                            color: theme.info
                                            font.pixelSize: 9
                                            font.weight: Font.Medium
                                        }
                                    }

                                    // Status indicator. Running: pulsing
                                    // dot only (no pill — would compete
                                    // with the capability pill next to
                                    // it). Done: ✓ glyph in success
                                    // colour. Error: ✗ glyph in error
                                    // colour. Clean and quiet.
                                    Item {
                                        implicitWidth: 14
                                        implicitHeight: 14
                                        Rectangle {
                                            visible: status === "running"
                                            anchors.centerIn: parent
                                            width: 8; height: 8; radius: 4
                                            color: theme.primary
                                            SequentialAnimation on opacity {
                                                running: status === "running"
                                                loops: Animation.Infinite
                                                NumberAnimation { from: 1.0; to: 0.4; duration: 600 }
                                                NumberAnimation { from: 0.4; to: 1.0; duration: 600 }
                                                onRunningChanged: if (!running) parent.opacity = 1.0
                                            }
                                        }
                                        Text {
                                            visible: status === "done"
                                            anchors.centerIn: parent
                                            text: "✓"
                                            color: theme.success
                                            font.pixelSize: 14
                                            font.weight: Font.Bold
                                        }
                                        Text {
                                            visible: status === "error"
                                            anchors.centerIn: parent
                                            text: "✕"
                                            color: theme.error
                                            font.pixelSize: 13
                                            font.weight: Font.Bold
                                        }
                                    }
                                    Rectangle {
                                        // capability pill
                                        radius: 3
                                        color: Qt.rgba(0.49, 0.83, 0.39, 0.12)
                                        border.color: Qt.rgba(0.49, 0.83, 0.39, 0.4)
                                        border.width: 1
                                        implicitWidth: capLabel.implicitWidth + 10
                                        implicitHeight: capLabel.implicitHeight + 4
                                        Text {
                                            id: capLabel
                                            anchors.centerIn: parent
                                            text: capability
                                            color: theme.successSoft
                                            font.pixelSize: 9
                                            font.weight: Font.Medium
                                        }
                                    }
                                    Text {
                                        visible: agent_id.length > 0
                                        text: "→ " + root.peerLabel(agent_id)
                                        color: theme.successSoft
                                        font.pixelSize: 11
                                        // Stay monospace for plain hex
                                        // fallback; switch to default
                                        // font when we have a name.
                                        font.family: root.peerLabel(agent_id)
                                            === root.shorten(agent_id, 14)
                                                ? "monospace" : ""
                                    }
                                    Text {
                                        visible: status !== "running"
                                        text: elapsedSecs + "s"
                                        color: theme.textMuted
                                        font.pixelSize: 11
                                    }
                                    // One-line prompt preview — fills
                                    // the otherwise-empty header space
                                    // on collapsed cards. Elided so the
                                    // chevron stays at the right edge.
                                    // Hidden when expanded because the
                                    // full prompt is rendered in the
                                    // body below.
                                    //
                                    // Use `model.text` instead of bare
                                    // `text` because the Text item's
                                    // own `text` property would shadow
                                    // the model role.
                                    Text {
                                        visible: !taskCard.expanded
                                            && (model.text || "").length > 0
                                        Layout.fillWidth: true
                                        text: "“" + (model.text || "").replace(/\s+/g, " ").trim() + "”"
                                        color: theme.textSecondary
                                        font.pixelSize: 11
                                        font.italic: true
                                        elide: Text.ElideRight
                                        maximumLineCount: 1
                                    }
                                    // Spacer only when no preview is
                                    // visible (running state, blank
                                    // prompt) so the chevron stays
                                    // right-justified.
                                    Item {
                                        visible: taskCard.expanded
                                            || (model.text || "").length === 0
                                        Layout.fillWidth: true
                                    }
                                    Text {
                                        // Chevron is a visual affordance.
                                        // The whole header row is clickable
                                        // via the MouseArea anchored to it
                                        // below the RowLayout in z-order.
                                        text: taskCard.expanded ? "▾" : "▸"
                                        color: theme.textSecondary
                                        font.pixelSize: 11
                                    }
                                }

                                // Header click target. Anchored to the
                                // header row so only the header toggles
                                // expand/collapse — the expanded body's
                                // buttons keep their own click handling.
                                // Sits at the top of cardCol; z=-1 so
                                // child Text/Rectangle items in the
                                // RowLayout don't intercept its clicks
                                // (they have no MouseArea of their own).
                                MouseArea {
                                    anchors.fill: cardHeaderRow
                                    z: -1
                                    cursorShape: Qt.PointingHandCursor
                                    onClicked: taskCard.expanded = !taskCard.expanded
                                }

                                // Body-row prompt display — was shown
                                // both collapsed and expanded. Now
                                // redundant: collapsed cards show the
                                // prompt as an inline preview in the
                                // header row, expanded cards show the
                                // full prompt in the scrollable
                                // "Prompt" panel below. Hidden so the
                                // collapsed card stays compact.
                                Text {
                                    visible: false
                                    text: text
                                    color: theme.textSecondary
                                    font.pixelSize: 11
                                    wrapMode: Text.Wrap
                                    elide: taskCard.expanded ? Text.ElideNone : Text.ElideRight
                                    maximumLineCount: taskCard.expanded ? 999 : 1
                                    Layout.fillWidth: true
                                }

                                // ── expanded section: prompt + response + actions
                                ColumnLayout {
                                    visible: taskCard.expanded && status !== "running"
                                    Layout.fillWidth: true
                                    spacing: 6

                                    // Prompt panel — re-display the task
                                    // input so the operator can see what
                                    // they're about to re-run / follow up
                                    // on without having to remember.
                                    Text {
                                        text: "Prompt"
                                        color: theme.textMuted
                                        font.pixelSize: 10
                                        font.weight: Font.Medium
                                    }
                                    Rectangle {
                                        Layout.fillWidth: true
                                        Layout.preferredHeight: Math.min(promptTxt.implicitHeight + 16, 160)
                                        color: theme.background
                                        border.color: theme.borderSubtle
                                        border.width: 1
                                        radius: theme.radiusSmall

                                        ScrollView {
                                            anchors.fill: parent
                                            anchors.margins: 1
                                            clip: true
                                            TextArea {
                                                id: promptTxt
                                                readOnly: true
                                                text: model.text
                                                color: theme.textSecondary
                                                font.pixelSize: 11
                                                wrapMode: TextArea.Wrap
                                                selectionColor: theme.primary
                                                selectedTextColor: theme.text
                                                background: Item {}
                                                padding: 8
                                            }
                                        }
                                    }

                                    Text {
                                        text: status === "error" ? "Error" : "Response"
                                        color: theme.textMuted
                                        font.pixelSize: 10
                                        font.weight: Font.Medium
                                    }
                                    Rectangle {
                                        Layout.fillWidth: true
                                        Layout.preferredHeight: Math.min(bodyTxt.implicitHeight + 16, 240)
                                        color: theme.background
                                        border.color: theme.borderSubtle
                                        border.width: 1
                                        radius: theme.radiusSmall

                                        ScrollView {
                                            anchors.fill: parent
                                            anchors.margins: 1
                                            clip: true
                                            TextArea {
                                                id: bodyTxt
                                                readOnly: true
                                                text: status === "error"
                                                    ? ("Error: " + (model.error || "(unknown)"))
                                                    : model.body
                                                color: status === "error" ? theme.error : theme.text
                                                font.pixelSize: 12
                                                wrapMode: TextArea.Wrap
                                                selectionColor: theme.primary
                                                selectedTextColor: theme.text
                                                background: Item {}
                                                padding: 8
                                                // Pi answers are usually
                                                // markdown — render
                                                // headings, bold, italics,
                                                // lists, fenced code blocks
                                                // natively. Errors stay
                                                // plain (the "Error: ..."
                                                // prefix doesn't need
                                                // formatting).
                                                textFormat: status === "error"
                                                    ? TextEdit.PlainText
                                                    : TextEdit.MarkdownText
                                            }
                                        }
                                    }

                                    // Audit-log inline panel (only when
                                    // a CID exists, and only when the
                                    // user clicks View log).
                                    Rectangle {
                                        Layout.fillWidth: true
                                        visible: cid.length > 0 && cidExpanded
                                        Layout.preferredHeight: Math.min(auditTxt.implicitHeight + 16, 200)
                                        color: theme.background
                                        border.color: theme.info
                                        border.width: 1
                                        radius: theme.radiusSmall

                                        ScrollView {
                                            anchors.fill: parent
                                            anchors.margins: 1
                                            clip: true
                                            TextArea {
                                                id: auditTxt
                                                readOnly: true
                                                text: cidLoading ? "Fetching audit log…"
                                                    : cidPayload || "(no payload yet — click View log)"
                                                color: theme.text
                                                font.pixelSize: 11
                                                font.family: "monospace"
                                                wrapMode: TextArea.Wrap
                                                background: Item {}
                                                padding: 8
                                            }
                                        }
                                    }

                                    RowLayout {
                                        Layout.fillWidth: true
                                        spacing: 6
                                        DarkButton {
                                            text: "Re-run"
                                            onClicked: {
                                                // model.* prefix avoids
                                                // the trap where `text`
                                                // resolves to this
                                                // Button's "Re-run" label
                                                // instead of the row's
                                                // task text. Same trap
                                                // hits `capability` if a
                                                // child sets it, so
                                                // prefix both for safety.
                                                delegateCap.setText(model.capability);
                                                delegateText.text = model.text;
                                                delegateText.forceActiveFocus();
                                            }
                                        }
                                        DarkButton {
                                            text: "Follow up"
                                            visible: status === "done"
                                            onClicked: {
                                                // Stamp the next Delegate
                                                // click with a session id
                                                // tied to this task. The
                                                // receiver's wrapper sees
                                                // LMAO_SESSION_ID and
                                                // reuses pi/lemonade
                                                // conversation state, so
                                                // the operator only needs
                                                // to type the new question
                                                // — no rolled-up history.
                                                root.pendingSessionId =
                                                    model.session_id || model.task_id;
                                                delegateCap.setText(model.capability);
                                                delegateText.text = "";
                                                delegateText.forceActiveFocus();
                                            }
                                        }
                                        Item { Layout.fillWidth: true }
                                        Rectangle {
                                            visible: cid.length > 0
                                            radius: theme.radiusSmall
                                            color: cidExpanded
                                                ? theme.info
                                                : Qt.rgba(0.475, 0.753, 1, 0.12)
                                            border.color: theme.info
                                            border.width: 1
                                            implicitWidth: cidLbl.implicitWidth + 14
                                            implicitHeight: theme.controlHeight - 6
                                            Text {
                                                id: cidLbl
                                                anchors.centerIn: parent
                                                text: cidExpanded ? "Hide log"
                                                    : (cidLoading ? "Loading…" : "View log")
                                                color: cidExpanded ? "#ffffff" : theme.info
                                                font.pixelSize: theme.fontSmall
                                                font.weight: Font.Medium
                                            }
                                            MouseArea {
                                                anchors.fill: parent
                                                cursorShape: Qt.PointingHandCursor
                                                onClicked: {
                                                    if (cidExpanded) {
                                                        tasksModel.setProperty(index, "cidExpanded", false);
                                                    } else if (cidPayload) {
                                                        // Already prefetched — show instantly
                                                        tasksModel.setProperty(index, "cidExpanded", true);
                                                    } else {
                                                        tasksModel.setProperty(index, "cidLoading", true);
                                                        tasksModel.setProperty(index, "cidExpanded", true);
                                                        logos.callModule("agent",
                                                            "start_fetch_cid", [cid]);
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }

                        Text {
                            visible: tasksModel.count === 0
                            anchors.centerIn: parent
                            text: "Run a delegation above to populate this list."
                            color: theme.textMuted
                            font.pixelSize: 11
                            font.italic: true
                        }
                    } // end ListView
                    } // end ListView wrapper Rectangle
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

                // Auto-refresh whenever the daemon transitions to
                // ready. Without this, opening the Trust tab while
                // the daemon is still warming up sticks the pane on
                // "daemon not running" until the operator clicks
                // Refresh manually. Fires once per ready-edge.
                Connections {
                    target: root
                    function onDaemonStateChanged() {
                        if (root.daemonState === "ready") {
                            trustCol.refreshTrust();
                        }
                    }
                }

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
