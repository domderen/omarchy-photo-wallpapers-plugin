import QtQuick
import Quickshell
import qs.Commons
import qs.Commons as Commons
import qs.Ui as Ui

Ui.BarWidget {
    id: root
    implicitWidth: icons.implicitWidth
    implicitHeight: icons.implicitHeight
    // One kept socket subscription is shared by every monitor and the settings panel.
    readonly property var engine: bar?.shell?.serviceFor("domderen.photo-wallpapers")?.engine ?? null
    readonly property var status: engine ? engine.status : ({})
    Row {
        id: icons
    Ui.BarIconButton {
        id: icon
        bar: root.bar
        text: root.status.paused ? "󰏤" : "󰸉"
        foreground: Commons.Color.foreground
        tooltipText: "Photo Wallpapers · " + (root.status.paused ? "Paused" : (root.status.selected_source_count || 0) + " sources")
        onPressed: Util.execArgv(["omarchy-shell", "shell", "toggle", "domderen.photo-wallpapers", "{}"])
    }
    Ui.BarIconButton {
        bar: root.bar
        text: root.status.privacy?.requested ? "󰒃" : "󰒙"
        foreground: root.status.privacy?.requested ? Commons.Color.accent : Commons.Color.foreground
        tooltipText: "Meeting Mode · " + (root.status.privacy?.requested ? (root.status.privacy?.state || "Preparing") : "Off") + " · Click to " + (root.status.privacy?.requested ? "end" : "start")
        onPressed: if (root.engine) root.engine.runAction("meeting", [root.status.privacy?.requested ? "off" : "on"], null)
    }
    }
}
