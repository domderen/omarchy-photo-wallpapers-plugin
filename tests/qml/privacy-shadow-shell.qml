import QtQuick
import Quickshell
import "../../plugin/components"
ShellRoot {
    PrivacyOverlay { id: overlay }
    Component.onCompleted: Qt.callLater(function() {
        if (overlay.protectedMode && !overlay.showTarget && !overlay.imagePath) console.log("PHOTO_WALLPAPERS_QML_CHECKS_PASS")
        else console.error("PHOTO_WALLPAPERS_QML_CHECKS_FAIL startup must cover old pixels without a daemon")
        Qt.quit()
    })
}
