import QtQuick
import Quickshell
import "plugin/components"
ShellRoot {
    EngineBridge { id: bridge }
    property string response: ""
    Connections {
        target: bridge
        function onCompleted(action, value) { if (action === "auth") response = value.error || "" }
        function onConnectedChanged() {
            if (!bridge.connected) return
            Qt.callLater(function() {
                bridge.runAction("auth", ["login"], {password:"synthetic-do-not-send"})
                if (response !== "engine_unavailable" || Object.keys(bridge.pending).length)
                    console.error("PHOTO_WALLPAPERS_QML_CHECKS_FAIL", "Credentials were queued before the protocol handshake")
                else console.log("PHOTO_WALLPAPERS_QML_CHECKS_PASS")
                Qt.quit()
            })
        }
    }
}
