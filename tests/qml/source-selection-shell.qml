import QtQuick
import Quickshell
import "plugin/components"
ShellRoot {
    QtObject { id: canonical; property var selections: [] }
    SourcePicker {
        id: picker
        sources: [{kind:"library",id:"all",name:"All photos",count:2,available:true}]
        selected: canonical.selections
        onSelectionChangedByUser: function(value) { canonical.selections = value }
    }
    Component.onCompleted: Qt.callLater(function() {
        picker.toggle(picker.sources[0])
        if (canonical.selections.length !== 1) { console.error("PHOTO_WALLPAPERS_QML_CHECKS_FAIL", "Selection was not emitted"); Qt.quit(); return }
        canonical.selections = []
        if (picker.selected.length !== 0) { console.error("PHOTO_WALLPAPERS_QML_CHECKS_FAIL", "Reloaded canonical settings must reset source checks"); Qt.quit(); return }
        picker.toggle(picker.sources[0])
        canonical.selections = [{kind:"album",id:"other"}]
        if (picker.isSelected(picker.sources[0])) { console.error("PHOTO_WALLPAPERS_QML_CHECKS_FAIL", "Cancel must restore canonical selection"); Qt.quit(); return }
        console.log("PHOTO_WALLPAPERS_QML_CHECKS_PASS")
        Qt.quit()
    })
}
