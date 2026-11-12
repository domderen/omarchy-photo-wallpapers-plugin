import QtQuick
import Quickshell
import "plugin" as Photo
ShellRoot {
    Photo.Service { id: kept; engine.polling: false }
    QtObject { id: facade; function serviceFor(id) { return id === "domderen.photo-wallpapers" ? kept : null } }
    Photo.Settings { id: settings; shell: facade }
    Component.onCompleted: Qt.callLater(function() {
        if (settings.engine === kept.engine) console.log("PHOTO_WALLPAPERS_QML_CHECKS_PASS")
        else console.error("PHOTO_WALLPAPERS_QML_CHECKS_FAIL", "Settings must share the kept socket bridge")
        Qt.quit()
    })
}
