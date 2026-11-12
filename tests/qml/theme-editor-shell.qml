import QtQuick
import Quickshell
import "../../plugin/components"
ShellRoot {
    ThemeEditor { id: editor; width: 700; visible: false; hasCurrentPhoto: true; onPeriodRequested: function(period) { root.requested = period } }
    id: root
    property string requested: ""
    function find(item, name) {
        if (item.objectName === name) return item
        for (var child of item.children || []) { var result = find(child, name); if (result) return result }
        return null
    }
    function check(condition, message) { if (!condition) throw new Error(message) }
    Component.onCompleted: Qt.callLater(function() {
        try {
            editor.load(false, {})
            check(editor.mode === "auto" && editor.locationMode === "current" && editor.style === "neutral", "additive defaults")
            editor.matchColors = true
            editor.locationMode = "fixed"
            check(!editor.valid, "fixed location needs valid coordinates")
            find(editor, "themeLatitude").text = "52.2"
            find(editor, "themeLongitude").text = "21.0"
            check(editor.valid, "valid fixed location")
            find(editor, "sunriseOffset").text = "181"
            check(!editor.valid, "offset bound")
            find(editor, "sunriseOffset").text = "30"
            editor.mode = "dark"
            find(editor, "style-rich").clicked()
            var value = editor.settings()
            check(value.style === "rich" && value.mode === "dark" && value.location.latitude === 52.2 && value.sunrise_offset_minutes === 30, "current draft values")
            find(editor, "period-night").clicked()
            check(root.requested === "night" && editor.currentPeriod === "auto", "feedback emits without detaching canonical period")
            find(editor, "themeLatitude").text = "91"
            check(!editor.valid, "latitude bound")
            editor.matchColors = false
            check(editor.valid && editor.settings().location_mode === "current", "disable remains possible with an invalid unsaved location draft")
            editor.load(true, {mode: "light", location_mode: "current", sunrise_offset_minutes: -15, sunset_offset_minutes: 20, prefer_time_of_day: false})
            check(editor.settings().mode === "light" && editor.settings().sunrise_offset_minutes === -15 && !editor.settings().prefer_time_of_day, "canonical reload discards draft")
            editor.status = {state: "active", mode: "light", location: {name: "Fixture area", provider: "omarchy-weather-ip-estimate", stale: false, resolved_at: "2026-10-29T12:00:00.041200539Z"}}
            var locationStatus = find(editor, "themeOperationStatus")
            check(locationStatus && locationStatus.text.indexOf("Estimated area: Fixture area") >= 0 && /updated [^ ]+ \d{2}:\d{2}/.test(locationStatus.text), "visible current location discloses estimate and freshness")
            editor.status = {state: "active", mode: "light", location: {name: "Fixture city", provider: "omarchy-weather", stale: true}}
            check(locationStatus.text.indexOf("Weather location: Fixture city") >= 0 && locationStatus.text.indexOf("last known") >= 0, "visible configured location identifies cached fallback")
            editor.status = {state: "active", mode: "dark", location: {name: "Fixture fixed", provider: "fixed", stale: false}}
            check(locationStatus.text.indexOf("Fixed coordinates: Fixture fixed") >= 0, "visible fixed location identifies its source")
            console.log("PHOTO_WALLPAPERS_QML_CHECKS_PASS")
        } catch (error) { console.error("PHOTO_WALLPAPERS_QML_CHECKS_FAIL", error) }
        Qt.quit()
    })
}
