import QtQuick
import QtQuick.Layouts
import qs.Ui as Ui
RowLayout {
    id: root
    property var status: ({})
    property bool busy: false
    readonly property bool hasPhoto: !!status.current && status.current.provenance !== "meeting-neutral"
    signal action(string action, var arguments)
    spacing: 6
    Ui.Button { text: "Previous"; focusable: true; enabled: !root.busy; onClicked: root.action("previous", []) }
    Ui.Button { text: "Next"; focusable: true; enabled: !root.busy; onClicked: root.action("next", []) }
    Ui.Button { text: root.status.paused ? "Resume" : "Pause"; focusable: true; onClicked: root.action(root.status.paused ? "resume" : "pause", []) }
    Ui.Button { text: root.status.pinned ? "Unpin" : "Pin"; focusable: true; enabled: root.hasPhoto; onClicked: root.action(root.status.pinned ? "unpin" : "pin", []) }
    Ui.Button { text: "Refresh"; focusable: true; enabled: !root.busy; onClicked: root.action("refresh", []) }
    Ui.Button { text: "Never show again"; focusable: true; enabled: root.hasPhoto; onClicked: root.action("feedback", ["--action", "never"]) }
}
