import QtQuick
import Quickshell
import "../../plugin/components"
ShellRoot {
    id: root
    property int preloaded: 0
    property int frames: 0
    QtObject {
        id: fake
        property bool connected: true
        property bool protectionRequestedFromShadow: false
        property var status: ({daemon_running: true, privacy: {requested: true, protected: true, generation: 1, transition_id: "native-fixture", state: "arming", phase: "preload", pending_path: Quickshell.env("PHOTO_WALLPAPERS_QML_FIXTURE"), outputs: Quickshell.screens.map(function(s) { return s.name })}, current: null})
        function privacyAck(operation, input) {
            if (operation === "preloaded") {
                root.preloaded++
                if (root.preloaded === Quickshell.screens.length) status = Object.assign({}, status, {privacy: Object.assign({}, status.privacy, {phase: "frame"})})
            } else if (operation === "frame") {
                root.frames++
                if (root.frames === Quickshell.screens.length) {
                    status = Object.assign({}, status, {current: {render_path: status.privacy.pending_path}, privacy: Object.assign({}, status.privacy, {state: "active", phase: "active"})})
                    Qt.callLater(checkActive)
                }
            } else fail("render failed")
        }
    }
    PrivacyOverlay { id: overlay; engine: fake }
    function fail(error) { console.error("PHOTO_WALLPAPERS_QML_CHECKS_FAIL", error); Qt.quit() }
    function checkActive() {
        if (!overlay.protectedMode || !overlay.showTarget || root.frames !== Quickshell.screens.length) return fail("active requires an actually displayed frame on each output")
        fake.connected = false
        Qt.callLater(function() {
            if (!overlay.protectedMode || overlay.showTarget || overlay.imagePath) return fail("disconnect must retain an opaque neutral surface")
            fake.status = Object.assign({}, fake.status, {privacy: {protected: false, requested: false, state: "off", phase: "off"}})
            fake.connected = true
            Qt.callLater(function() {
                if (overlay.protectedMode) return fail("explicit exit releases the surface")
                console.log("PHOTO_WALLPAPERS_QML_CHECKS_PASS")
                Qt.quit()
            })
        })
    }
    Timer { interval: 12000; running: true; onTriggered: root.fail("frame confirmation deadline; preloaded=" + root.preloaded + " frames=" + root.frames) }
}
