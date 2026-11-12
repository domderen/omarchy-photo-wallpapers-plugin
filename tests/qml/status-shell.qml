import QtQuick
import Quickshell
import "plugin/components"
ShellRoot {
    StatusSummary { id: summary }
    Component.onCompleted: Qt.callLater(function() {
        summary.status = {daemon_running:true, protocol_version:1, connection:"online", sync_progress:{phase:"photos",library:"shared",completed:120,total:500}}
        if (summary.text.indexOf("Reading shared photos: 120 / 500") < 0) { console.error("PHOTO_WALLPAPERS_QML_CHECKS_FAIL", "Photo progress is not visible"); Qt.quit(); return }
        summary.status = {daemon_running:true, protocol_version:1, connection:"online", sync_progress:{phase:"albums",completed:2,total:9}}
        if (summary.text.indexOf("Reading albums: 2 / 9") < 0) { console.error("PHOTO_WALLPAPERS_QML_CHECKS_FAIL", "Album progress is not visible"); Qt.quit(); return }
        console.log("PHOTO_WALLPAPERS_QML_CHECKS_PASS")
        Qt.quit()
    })
}
