import QtQuick
import Quickshell
import "components"
Item {
    property var shell: null
    property alias engine: engine
    EngineBridge { id: engine }
    Loader {
        active: !!Quickshell.env("WAYLAND_DISPLAY") && Quickshell.env("QT_QPA_PLATFORM") !== "offscreen"
        source: active ? "components/PrivacyOverlay.qml" : ""
        onLoaded: item.engine = engine
    }
}
