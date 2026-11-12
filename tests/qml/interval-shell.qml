import QtQuick
import Quickshell
import "../../plugin/components"
ShellRoot {
    IntervalEditor { id: editor; width: 700; height: 80; visible: false }
    function check(value, expected, label) { if (value !== expected) throw new Error(label) }
    function findInput(item) {
        if (item.objectName === "intervalInput") return item
        for (var child of item.children || []) { var found = findInput(child); if (found) return found }
        return null
    }
    function runChecks() {
        editor.setSeconds(3600)
        check(editor.seconds, 3600, "default")
        var input = findInput(editor)
        input.text = "6"
        input.textEdited()
        Qt.callLater(function() {
            input.text = "60"
            input.textEdited()
            Qt.callLater(function() {
                try {
                    check(editor.valid, true, "accept sixty seconds")
                    check(editor.seconds, 60, "read current edited field value")
                    editor.setSeconds(900)
                    check(editor.seconds, 900, "preset")
                    console.log("PHOTO_WALLPAPERS_QML_CHECKS_PASS")
                } catch (error) { console.error("PHOTO_WALLPAPERS_QML_CHECKS_FAIL", error) }
                Qt.quit()
            })
        })
    }
    Component.onCompleted: Qt.callLater(runChecks)
}
