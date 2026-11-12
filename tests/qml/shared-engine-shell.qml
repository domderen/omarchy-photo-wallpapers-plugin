import QtQuick
import Quickshell
import "plugin" as Photo

ShellRoot {
    Photo.Service { id: kept; engine.polling: false }
    QtObject {
        id: facade
        function serviceFor(id) { return id === "domderen.photo-wallpapers" ? kept : null }
    }
    QtObject {
        id: hostBar
        property var shell: facade
        property bool vertical: false
        property int barSize: 32
        property string fontFamily: "Sans Serif"
        property color barForeground: "white"
        property bool foregroundAnimationEnabled: false
    }
    Photo.BarWidget { id: first; bar: hostBar }
    Photo.BarWidget { id: second; bar: hostBar }
    Component.onCompleted: Qt.callLater(function() {
        try {
            if (first.engine !== kept.engine || second.engine !== kept.engine)
                throw new Error("Bar instances must reuse the kept service's status poller")
            kept.engine.status = {paused: true, selected_source_count: 2}
            if (!first.status.paused || !second.status.paused)
                throw new Error("Both bars must reflect shared status updates")
            first.bar = null
            if (first.status.paused !== undefined)
                throw new Error("A missing service must leave a safe empty status")
            console.log("PHOTO_WALLPAPERS_QML_CHECKS_PASS")
        } catch (error) { console.error("PHOTO_WALLPAPERS_QML_CHECKS_FAIL", error) }
        Qt.quit()
    })
}
