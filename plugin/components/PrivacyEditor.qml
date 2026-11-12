import QtQuick
import QtQuick.Layouts
import qs.Commons
import qs.Commons as Commons
import qs.Ui as Ui

ColumnLayout {
    id: root
    objectName: "privacyEditor"
    property var status: ({})
    property bool online: true
    property bool filterPersonal: false
    property bool hasCurrentPhoto: false
    property bool canAllow: false
    property string currentChoice: "auto"
    readonly property string modelState: status.models?.state || status.model_state || "unavailable"
    readonly property bool installing: ["downloading", "cancelling"].indexOf(modelState) >= 0
    signal edited()
    signal meetingRequested(string operation)
    signal modelsRequested(string operation)
    signal choiceRequested(string choice)
    function fallbackText() {
        var reasons = {no_sources: "Choose photo sources", empty_source_pool: "No photos in the selected sources", rotation_paused: "Resume or press Next for a screened photo", privacy_models_unavailable: "Install or repair screening models", preparing_screened_photo: "Preparing a screened photo", screening_waiting: "Screening will continue after a short pause", no_screened_photo: "No screened photo is ready yet"}
        return reasons[status.reason] ? " · Neutral background · " + reasons[status.reason] : ""
    }
    function load(value) { filterPersonal = !!value?.filter_personal }
    spacing: 8
    Text { text: "PHOTO PRIVACY"; color: Commons.Color.accent; font.family: Style.font.family; font.pixelSize: Style.font.caption }
    Text {
        id: operationStatus; objectName: "privacyStatus"
        text: !root.online && root.status.protected ? "Disconnected · neutral background retained" : root.status.state === "arming" ? "Preparing protection · wait before sharing" : root.status.state === "error" ? "Protection needs attention · neutral background retained" : root.status.state === "active" ? (root.status.requested ? "Meeting Mode · Active" + root.fallbackText() : "Personal filtering · Active" + root.fallbackText()) : "Meeting Mode is off"
        color: Commons.Color.foreground; textFormat: Text.PlainText; wrapMode: Text.WordWrap; Layout.fillWidth: true
    }
    RowLayout {
        Ui.Button { objectName: "meetingOn"; text: root.status.state === "error" ? "Retry protection" : "Start meeting"; focusable: true; selected: !!root.status.requested; enabled: root.online && (!root.status.requested || root.status.state === "error"); onClicked: root.meetingRequested("on") }
        Ui.Button { objectName: "meetingOff"; text: "End meeting"; focusable: true; enabled: root.online && !!root.status.requested; onClicked: root.meetingRequested("off") }
    }
    Text { text: "Swimwear is allowed. Sleepwear, underwear, nudity and bath/shower photos are excluded. Uncertain or unscanned photos stay out automatically. No individual approval is required."; color: Commons.Color.foreground; textFormat: Text.PlainText; wrapMode: Text.WordWrap; Layout.fillWidth: true; font.pixelSize: Style.font.caption }
    Text { text: "Screening runs locally. It can miss details and excludes many ordinary photos. Check the visible desktop before sharing; a neutral background is used when no eligible photo is ready."; color: Commons.Color.foreground; wrapMode: Text.WordWrap; Layout.fillWidth: true; font.pixelSize: Style.font.caption }
    RowLayout {
        Ui.Button { objectName: "privacyInstall"; text: root.modelState === "ready" ? "Verify / repair models" : "Install screening models"; focusable: true; enabled: root.online && !root.installing && root.status.models?.supported !== false; onClicked: root.modelsRequested("install") }
        Ui.Button { text: "Cancel download"; visible: root.installing; enabled: root.modelState !== "cancelling"; onClicked: root.modelsRequested("cancel") }
    }
    Text { text: root.installing ? "Downloading verified models · " + (root.status.models?.completed || 0) + "/4 · " + (root.status.models?.item || "") : root.modelState === "ready" ? "Local models installed · no cloud analysis" : "Models unavailable · about 300 MB to install; the neutral background works offline."; color: Commons.Color.foreground; wrapMode: Text.WordWrap; textFormat: Text.PlainText; Layout.fillWidth: true; font.pixelSize: Style.font.caption }
    Text { visible: !!root.status.counts; text: "Selected sources: " + (root.status.counts?.clear || 0) + " clear, " + (root.status.counts?.sensitive || 0) + " excluded, " + (root.status.counts?.uncertain || 0) + " uncertain, " + (root.status.counts?.unscanned || 0) + " unscanned"; color: Commons.Color.foreground; wrapMode: Text.WordWrap; Layout.fillWidth: true; font.pixelSize: Style.font.caption }
    Ui.Button { objectName: "privacyPersonalFilter"; text: "Also filter Personal Mode"; focusable: true; selected: root.filterPersonal; onClicked: { root.filterPersonal = !root.filterPersonal; root.edited() } }
    Text { text: "Personal Mode filtering follows Save / Cancel. Start / End meeting takes effect immediately and stays enabled until you end it."; color: Commons.Color.foreground; wrapMode: Text.WordWrap; Layout.fillWidth: true; font.pixelSize: Style.font.caption }
    RowLayout {
        visible: root.hasCurrentPhoto
        Text { text: "This photo"; color: Commons.Color.foreground; font.family: Style.font.family }
        Repeater {
            model: [{id: "auto", label: "Auto"}, {id: "personal_only", label: "Personal only"}, {id: "allow", label: "Allow"}]
            Ui.Button { required property var modelData; objectName: "privacy-" + modelData.id; text: modelData.label; focusable: true; selected: root.currentChoice === modelData.id; enabled: root.online && (modelData.id !== "allow" || root.canAllow); onClicked: root.choiceRequested(modelData.id) }
        }
    }
    Text { visible: root.hasCurrentPhoto; text: "Optional correction. Allow explicitly overrides automatic screening for these exact image pixels. Never show again always wins. Allow becomes available after this photo has been checked locally."; color: Commons.Color.foreground; wrapMode: Text.WordWrap; Layout.fillWidth: true; font.pixelSize: Style.font.caption }
}
