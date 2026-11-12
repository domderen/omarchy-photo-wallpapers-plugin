import QtQuick
import Quickshell
import "../../plugin/components"
ShellRoot {
    id: root
    property string mode: ""
    property string modelAction: ""
    property string choice: ""
    PrivacyEditor {
        id: editor; width: 700; visible: false; hasCurrentPhoto: true
        onMeetingRequested: function(operation) { root.mode = operation }
        onModelsRequested: function(operation) { root.modelAction = operation }
        onChoiceRequested: function(value) { root.choice = value }
    }
    function find(item, name) { if (item.objectName === name) return item; for (var child of item.children || []) { var result = find(child, name); if (result) return result } return null }
    function check(condition, message) { if (!condition) throw new Error(message) }
    Component.onCompleted: Qt.callLater(function() {
        try {
            editor.load({filter_personal: false})
            find(editor, "meetingOn").clicked()
            check(root.mode === "on" && !editor.filterPersonal, "immediate meeting action leaves the settings draft alone")
            editor.status = {requested: true, protected: true, state: "arming", models: {state: "unavailable"}}
            check(find(editor, "privacyStatus").text.indexOf("Preparing") >= 0, "arming never says ready")
            find(editor, "meetingOff").clicked()
            check(root.mode === "off", "explicit immediate exit")
            find(editor, "privacyInstall").clicked()
            check(root.modelAction === "install", "model installation is explicit")
            find(editor, "privacyPersonalFilter").clicked()
            check(editor.filterPersonal, "optional personal filter draft")
            editor.load({filter_personal: false})
            check(!editor.filterPersonal, "canonical reload discards unsaved draft")
            find(editor, "privacy-personal_only").clicked()
            check(root.choice === "personal_only", "optional correction")
            editor.status = {requested: true, protected: true, state: "error", models: {state: "ready"}}
            check(find(editor, "meetingOn").enabled, "failed entry can retry without disabling protection")
            editor.status = {requested: true, protected: true, state: "active", reason: "no_sources", models: {state: "ready"}}
            check(find(editor, "privacyStatus").text.indexOf("Active") >= 0, "confirmed mode is active")
            check(find(editor, "privacyStatus").text.indexOf("Choose photo sources") >= 0, "fallback explains how to obtain photos")
            console.log("PHOTO_WALLPAPERS_QML_CHECKS_PASS")
        } catch(error) { console.error("PHOTO_WALLPAPERS_QML_CHECKS_FAIL", error) }
        Qt.quit()
    })
}
