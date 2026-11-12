import QtQuick
import Quickshell
import "plugin" as Photo
ShellRoot {
    id: root
    property var canonical: ({revision: 4, sources: [{kind: "album", id: "fixture-album"}], interval_seconds: 900, enhancements: {exposure: false, white_balance: false, contrast: false}, match_colors: false, theme: {mode: "auto", location_mode: "current", location: null, sunrise_offset_minutes: 0, sunset_offset_minutes: 0, prefer_time_of_day: true}})
    property var saved: null
    QtObject {
        id: fake
        property var status: ({daemon_running: true, protocol_version: 1, settings_revision: 4, current: null, theme: {state: "disabled"}})
        property bool busy: false
        property string error: ""
        signal completed(string action, var result)
        function refreshStatus() {}
        function runAction(action, args, input) {
            if (action === "config" && args[0] === "get") Qt.callLater(function() { fake.completed(action, root.canonical) })
            else if (action === "config") { root.saved = input; root.canonical = Object.assign({}, root.canonical, input, {revision: 5}); Qt.callLater(function() { fake.completed(action, root.canonical) }) }
            else if (action === "sources") Qt.callLater(function() { fake.completed(action, {sources: []}) })
        }
    }
    QtObject { id: kept; property var engine: fake }
    QtObject { id: facade; function serviceFor(id) { return kept } function hide(id) { settings.close() } }
    Photo.Settings { id: settings; shell: facade }
    function find(item, name, visited) {
        if (!item || visited.indexOf(item) >= 0) return null
        visited.push(item)
        if (item.objectName === name) return item
        if (item.contentItem) { var content = find(item.contentItem, name, visited); if (content) return content }
        for (var key of ["data", "children"]) for (var child of item[key] || []) { var result = find(child, name, visited); if (result) return result }
        return null
    }
    function check(condition, message) { if (!condition) throw new Error(message) }
    function finish(error) { console.log(error ? "PHOTO_WALLPAPERS_QML_CHECKS_FAIL " + error : "PHOTO_WALLPAPERS_QML_CHECKS_PASS"); Qt.quit() }
    Component.onCompleted: Qt.callLater(function() {
        fake.completed("config", root.canonical)
        Qt.callLater(function() {
            try {
                var editor = find(settings, "themeEditor", [])
                check(!!editor, "native settings exposes the theme editor")
                editor.matchColors = true; editor.mode = "dark"; editor.style = "rich"; editor.edited()
                settings.save()
                check(root.saved && root.saved.match_colors && root.saved.theme.mode === "dark" && root.saved.theme.style === "rich", "save includes theme draft")
                check(root.saved.sources[0].id === "fixture-album" && root.saved.interval_seconds === 900, "theme save preserves sources and interval")
                Qt.callLater(function() {
                    try {
                        editor.mode = "light"; editor.style = "subtle"; editor.edited(); settings.close(); settings.open("{}")
                        Qt.callLater(function() { Qt.callLater(function() {
                            try { settings.close(); check(editor.mode === "dark" && editor.style === "rich", "cancel/reopen restores canonical theme and style"); finish(null) } catch (error) { finish(error) }
                        }) })
                    } catch (error) { finish(error) }
                })
            } catch (error) { finish(error) }
        })
    })
}
