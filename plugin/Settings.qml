import QtQuick
import QtQuick.Layouts
import Quickshell
import Quickshell.Wayland
import qs.Commons
import qs.Commons as Commons
import qs.Ui as Ui
import "components"

Item {
    id: root
    property string omarchyPath: Quickshell.env("OMARCHY_PATH")
    property var shell: null
    property var manifest: null
    property bool opened: false
    property var draftSources: []
    property var draftEnhancements: ({exposure: false, white_balance: false, contrast: false})
    property int expectedRevision: 0
    property bool savePending: false
    property bool pairPending: false
    property bool dirty: false
    property string sourceStamp: ""
    readonly property var sharedEngine: shell?.serviceFor("domderen.photo-wallpapers")?.engine ?? null
    readonly property var engine: sharedEngine || fallbackEngine
    property string notice: ""
    property var sourceRows: []
    function open(payloadJson) {
        notice = ""
        opened = true
        dirty = false
        engine.refreshStatus()
        engine.runAction("config", ["get"], null)
        engine.runAction("sources", [], null)
        Qt.callLater(function() { card.forceActiveFocus() })
    }
    function close() { opened = false; connection.clearToken(); apple.clearSecrets() }
    function dismiss() {
        if (shell && typeof shell.hide === "function") shell.hide("domderen.photo-wallpapers")
        else close()
    }
    function save() {
        if (!interval.valid || !themeEditor.valid || engine.busy) return
        savePending = true
        engine.runAction("config", ["apply", "--expected-revision", String(expectedRevision), "--stdin-json"], {sources: draftSources, interval_seconds: interval.seconds, enhancements: draftEnhancements, match_colors: themeEditor.matchColors, theme: themeEditor.settings(), privacy: {filter_personal: privacyEditor.filterPersonal}})
    }
    EngineBridge { id: fallbackEngine; polling: root.opened && !root.sharedEngine }
    Connections {
        target: root.engine
        function onStatusChanged() {
            if (!root.opened) return
            var value = root.engine.status
            if (root.pairPending && !value.auth_busy) { root.pairPending = false; root.notice = value.error ? "Could not import Mac metadata: " + value.error.replace(/_/g, " ") : value.helper?.url ? "Mac metadata imported." : "" }
            var stamp = JSON.stringify([value.last_sync, value.helper?.last_refresh, value.helper?.stale])
            if (stamp !== root.sourceStamp && value.daemon_running) { root.sourceStamp = stamp; root.engine.runAction("sources", [], null) }
            if (!root.dirty && !root.savePending && value.settings_revision !== undefined && value.settings_revision !== root.expectedRevision) root.engine.runAction("config", ["get"], null)
        }
        function onCompleted(action, result) {
            if (result.error) {
                root.savePending = false
                root.notice = result.error === "settings_conflict" ? "Settings changed elsewhere. Cancel and reopen to reload." : "Could not complete the action: " + result.error.replace(/_/g, " ")
                return
            }
            if (action === "config") {
                root.expectedRevision = result.revision
                root.draftSources = result.sources.slice()
                root.draftEnhancements = Object.assign({}, result.enhancements)
                interval.setSeconds(result.interval_seconds)
                themeEditor.load(result.match_colors, result.theme)
                privacyEditor.load(result.privacy || {})
                root.dirty = false
                if (root.savePending) { root.notice = "Saved"; root.savePending = false; engine.runAction("sources", [], null) }
            } else if (action === "sources") root.sourceRows = result.sources || []
            else if (action === "pair") { connection.clearToken() }
        }
    }
    PanelWindow {
        id: window
        visible: root.opened
        color: "transparent"
        exclusionMode: ExclusionMode.Ignore
        anchors { top: true; bottom: true; left: true; right: true }
        WlrLayershell.namespace: "photo-wallpapers-settings"
        WlrLayershell.layer: WlrLayer.Overlay
        WlrLayershell.keyboardFocus: root.opened ? WlrKeyboardFocus.Exclusive : WlrKeyboardFocus.None
        Rectangle { anchors.fill: parent; color: Qt.rgba(0, 0, 0, 0.5); MouseArea { anchors.fill: parent; onClicked: root.dismiss() } }
        Ui.BorderSurface {
            id: card
            anchors.centerIn: parent
            width: Math.min(760, parent.width - 48)
            height: Math.min(content.implicitHeight + footer.implicitHeight + 48, parent.height - 48)
            color: Commons.Color.popups.background
            borderSpec: Border.surfaceSpec("popups", "border", Commons.Color.popups.border, 1)
            radius: Style.cornerRadius
            focus: true
            Keys.onEscapePressed: root.dismiss()
            MouseArea { anchors.fill: parent; onClicked: card.forceActiveFocus(); z: -1 }
            Flickable {
                anchors.fill: parent
                anchors.margins: 16
                anchors.bottomMargin: footer.implicitHeight + 32
                clip: true
                contentHeight: content.implicitHeight
                boundsBehavior: Flickable.StopAtBounds
                ColumnLayout {
                    id: content
                    width: parent.width
                    spacing: 10
                    RowLayout {
                        Layout.fillWidth: true
                        Text { text: "Photo Wallpapers"; color: Commons.Color.foreground; font.family: Style.font.family; font.pixelSize: Style.font.title; font.bold: true; Layout.fillWidth: true }
                        Ui.Button { text: "Close"; focusable: true; onClicked: root.dismiss() }
                    }
                    Text { text: "ICLOUD PHOTOS"; color: Commons.Color.accent; font.family: Style.font.family; font.pixelSize: Style.font.caption }
                    AuthEditor { id: apple; Layout.fillWidth: true; auth: root.engine.status.auth || ({}); busy: !!root.engine.status.auth_busy; daemonOnline: !!root.engine.status.daemon_running; onRequested: function(operation, input) { root.engine.runAction("auth", [operation], input) } }
                    StatusSummary { Layout.fillWidth: true; status: engine.status }
                    PhotoPreview { Layout.fillWidth: true; Layout.preferredHeight: engine.status.current && engine.status.current.provenance !== "meeting-neutral" ? 145 : 80; current: engine.status.current || null; privacyProtected: !!root.engine.privacyProtected || !!root.engine.status.privacy?.protected; modeConfirmed: !!root.engine.status.daemon_running && root.engine.status.privacy?.state === "active" }
                    Controls { status: engine.status; busy: engine.busy; onAction: function(action, arguments) { engine.runAction(action, arguments, null) } }
                    PrivacyEditor { id: privacyEditor; Layout.fillWidth: true; status: root.engine.status.privacy || ({}); online: !!root.engine.status.daemon_running; hasCurrentPhoto: !!root.engine.status.current && root.engine.status.current.provenance !== "meeting-neutral"; canAllow: !!root.engine.status.current?.privacy_allow_available; currentChoice: root.engine.status.current?.privacy_choice || "auto"; onEdited: root.dirty = true; onMeetingRequested: function(operation) { root.engine.runAction("meeting", [operation], null) } onModelsRequested: function(operation) { root.engine.runAction("models", [operation], null) } onChoiceRequested: function(choice) { root.engine.runAction("feedback", ["--action", "privacy"], {choice: choice}) } }
                    Text { text: "PHOTO SOURCES"; color: Commons.Color.accent; font.family: Style.font.family; font.pixelSize: Style.font.caption }
                    SourcePicker { Layout.fillWidth: true; sources: root.sourceRows; selected: root.draftSources; onSelectionChangedByUser: function(value) { root.draftSources = value; root.dirty = true } }
                    IntervalEditor { id: interval; Layout.fillWidth: true; onEdited: root.dirty = true }
                    RowLayout {
                        Text { text: "Adjust copies"; color: Commons.Color.foreground; font.family: Style.font.family }
                        Repeater {
                            model: [{key: "exposure", name: "Exposure"}, {key: "white_balance", name: "White balance"}, {key: "contrast", name: "Contrast"}]
                            Ui.Button {
                                required property var modelData
                                text: modelData.name; selected: root.draftEnhancements[modelData.key]; focusable: true
                                onClicked: { var next = Object.assign({}, root.draftEnhancements); next[modelData.key] = !next[modelData.key]; root.draftEnhancements = next; root.dirty = true }
                            }
                        }
                    }
                    ThemeEditor { id: themeEditor; Layout.fillWidth: true; status: root.engine.status.theme || ({}); hasCurrentPhoto: !!root.engine.status.current && root.engine.status.current.provenance !== "meeting-neutral"; currentPeriod: root.engine.status.current?.period || "auto"; onEdited: root.dirty = true; onPeriodRequested: function(period) { root.engine.runAction("feedback", ["--action", "period"], {period: period, asset_id: root.engine.status.current?.asset_id}) } }
                    Text { text: "OPTIONAL MAC METADATA"; color: Commons.Color.accent; font.family: Style.font.family; font.pixelSize: Style.font.caption }
                    Text { text: "A paired Mac adds named People. iCloud library and album sources work independently."; color: Commons.Color.foreground; textFormat: Text.PlainText; wrapMode: Text.WordWrap; Layout.fillWidth: true; font.pixelSize: Style.font.caption }
                    ConnectionEditor { id: connection; Layout.fillWidth: true; helperUrl: engine.status.helper_url || ""; onPairRequested: function(value) { root.pairPending = true; root.notice = "Checking Mac metadata…"; engine.runAction("pair", ["--stdin-json"], value) } }
                }
            }
            RowLayout {
                id: footer
                anchors { left: parent.left; right: parent.right; bottom: parent.bottom; margins: 16 }
                Text { text: root.notice || engine.error; textFormat: Text.PlainText; color: Commons.Color.accent; wrapMode: Text.WordWrap; Layout.fillWidth: true }
                Ui.Button { text: "Cancel"; focusable: true; onClicked: root.dismiss() }
                Ui.Button { text: "Save"; focusable: true; selected: true; enabled: interval.valid && themeEditor.valid && !engine.busy; onClicked: root.save() }
            }
        }
    }
}
