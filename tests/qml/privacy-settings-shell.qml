import QtQuick
import Quickshell
import "plugin" as Photo
ShellRoot {
    id: root
    property var canonical: ({revision: 4, sources: [{kind: "album", id: "fixture-album"}], interval_seconds: 900, enhancements: {exposure: false, white_balance: false, contrast: false}, match_colors: false, theme: {}, privacy: {meeting: false, filter_personal: false}})
    property var saved: null
    QtObject {
        id: fake
        property bool connected: true
        property var status: ({daemon_running: true, settings_revision: 4, privacy: {protected: false, requested: false, state: "off"}})
        property bool busy: false
        property string error: ""
        signal completed(string action, var result)
        function refreshStatus() {}
        function runAction(action, args, input) {
            if (action === "config" && args[0] === "get") Qt.callLater(function() { fake.completed(action, root.canonical) })
            else if (action === "config") {
                root.saved = input
                if (Number(args[2]) !== root.canonical.revision) Qt.callLater(function() { fake.completed(action, {error: "settings_conflict"}) })
                else { root.canonical = Object.assign({}, root.canonical, input, {revision: 5}); Qt.callLater(function() { fake.completed(action, root.canonical) }) }
            } else if (action === "sources") Qt.callLater(function() { fake.completed(action, {sources: []}) })
            else if (action === "meeting") {
                root.canonical = Object.assign({}, root.canonical, {revision: 6, privacy: {meeting: true, filter_personal: root.canonical.privacy.filter_personal}})
                status = Object.assign({}, status, {settings_revision: 6, privacy: {protected: true, requested: true, state: "arming"}})
            }
        }
    }
    QtObject { id: kept; property var engine: fake }
    QtObject { id: facade; function serviceFor(id) { return kept } function hide(id) { settings.close() } }
    Photo.Settings { id: settings; shell: facade }
    function find(item, name, visited) {
        if (!item || visited.indexOf(item) >= 0) return null
        visited.push(item); if (item.objectName === name) return item
        if (item.contentItem) { var content = find(item.contentItem, name, visited); if (content) return content }
        for (var key of ["data", "children"]) for (var child of item[key] || []) { var result = find(child, name, visited); if (result) return result }
        return null
    }
    function check(condition, message) { if (!condition) throw new Error(message) }
    function finish(error) { settings.close(); console.log(error ? "PHOTO_WALLPAPERS_QML_CHECKS_FAIL " + error : "PHOTO_WALLPAPERS_QML_CHECKS_PASS"); Qt.quit() }
    Component.onCompleted: Qt.callLater(function() {
        fake.completed("config", root.canonical)
        Qt.callLater(function() {
            try {
                var editor=find(settings, "privacyEditor", [])
                check(!!editor && !editor.filterPersonal, "privacy defaults off")
                editor.filterPersonal=true; editor.edited(); settings.save()
                check(root.saved.privacy.filter_personal && root.saved.privacy.meeting === undefined, "Save contains only the draft Personal filter")
                check(root.saved.sources[0].id === "fixture-album" && root.saved.interval_seconds === 900, "privacy Save preserves sources and interval")
                Qt.callLater(function() {
                    try {
                        editor.filterPersonal=false; editor.edited(); settings.close(); settings.open("{}")
                        Qt.callLater(function() { Qt.callLater(function() {
                            try {
                                check(editor.filterPersonal, "Cancel/reopen restores the canonical filter")
                                editor.filterPersonal=false; editor.edited()
                                editor.meetingRequested("on")
                                check(settings.dirty && settings.expectedRevision === 5, "immediate mode does not discard or bless a dirty draft")
                                settings.save()
                                Qt.callLater(function() {
                                    try { check(settings.notice.indexOf("Settings changed elsewhere") >= 0 && root.canonical.privacy.meeting, "stale draft conflicts and leaves Meeting Mode on"); finish(null) } catch(error) { finish(error) }
                                })
                            } catch(error) { finish(error) }
                        }) })
                    } catch(error) { finish(error) }
                })
            } catch(error) { finish(error) }
        })
    })
}
