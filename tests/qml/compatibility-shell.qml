import QtQuick
import Quickshell
import "plugin/components"
ShellRoot {
    EngineBridge { id: bridge; polling: false }
    StatusSummary { id: summary; status: bridge.status }
    property string result: ""
    Connections { target: bridge; function onCompleted(action, value) { result = value.error || "" } }
    Component.onCompleted: Qt.callLater(function() {
        try {
            bridge.status = {daemon_running:true, protocol_version:99, backend_version:"99.0.0"}
            bridge.runAction("auth", ["login"], {password:"synthetic-secret"})
            if (result !== "backend_incompatible" || Object.keys(bridge.pending).length)
                throw new Error("An incompatible backend must not receive credentials or queue actions")
            if (summary.text.toLowerCase().indexOf("update") < 0)
                throw new Error("Incompatible setup must explain how to recover")
            bridge.status = {daemon_running:true}
            bridge.runAction("next", [], null)
            if (result !== "backend_incompatible") throw new Error("Old backends without a protocol need an update")
            bridge.status = {daemon_running:true, protocol_version:1, backend_version:"0.4.0"}
            bridge.runAction("pause", [], null)
            if (result !== "engine_unavailable") throw new Error("Compatible request must reach the real disconnected transport")
            bridge.status = {daemon_running:false}
            if (summary.text.indexOf("README") < 0) throw new Error("Missing service needs setup guidance")
            console.log("PHOTO_WALLPAPERS_QML_CHECKS_PASS")
        } catch (error) { console.error("PHOTO_WALLPAPERS_QML_CHECKS_FAIL", error) }
        Qt.quit()
    })
}
