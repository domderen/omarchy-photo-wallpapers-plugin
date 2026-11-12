import QtQuick
import Quickshell
import "../../plugin/components"
ShellRoot {
    PhotoPreview { id: preview; width: 600; height: 160; privacyProtected: true; modeConfirmed: false; current: ({preview_path: "/private/unadmitted-fixture.jpg"}) }
    function image(item) { if (item.objectName === "photoPreviewImage") return item; for (var child of item.children || []) { var result=image(child); if (result) return result } return null }
    Component.onCompleted: Qt.callLater(function() {
        if (image(preview).source.toString() === "") console.log("PHOTO_WALLPAPERS_QML_CHECKS_PASS")
        else console.error("PHOTO_WALLPAPERS_QML_CHECKS_FAIL unconfirmed preview must not load private pixels")
        Qt.quit()
    })
}
