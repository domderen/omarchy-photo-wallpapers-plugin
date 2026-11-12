import QtQuick
import QtQuick.Layouts
import qs.Commons
import qs.Commons as Commons
import qs.Ui as Ui

ColumnLayout {
    id: root
    objectName: "themeEditor"
    property bool matchColors: false
    property string mode: "auto"
    property string style: "neutral"
    property string locationMode: "current"
    property bool preferTimeOfDay: true
    property var status: ({})
    property bool hasCurrentPhoto: false
    property string currentPeriod: "auto"
    property var canonical: ({mode: "auto", style: "neutral", location_mode: "current", location: null, sunrise_offset_minutes: 0, sunset_offset_minutes: 0, prefer_time_of_day: true})
    readonly property bool fieldsValid: validOffset(sunrise.text) && validOffset(sunset.text) && (locationMode === "current" || (validCoordinate(latitude.text, 90) && validCoordinate(longitude.text, 180) && label.text.length <= 160))
    readonly property bool valid: !matchColors || fieldsValid
    signal edited()
    signal periodRequested(string period)
    function validOffset(text) { return /^[-+]?\d+$/.test(text) && Number(text) >= -180 && Number(text) <= 180 }
    function validCoordinate(text, bound) { return /^[-+]?(\d+(\.\d*)?|\.\d+)$/.test(text.trim()) && isFinite(Number(text)) && Math.abs(Number(text)) <= bound }
    function load(enabled, value) {
        canonical = Object.assign({mode: "auto", style: "neutral", location_mode: "current", location: null, sunrise_offset_minutes: 0, sunset_offset_minutes: 0, prefer_time_of_day: true}, JSON.parse(JSON.stringify(value || {})))
        matchColors = !!enabled; mode = canonical.mode; style = canonical.style; locationMode = canonical.location_mode; preferTimeOfDay = canonical.prefer_time_of_day
        sunrise.text = String(canonical.sunrise_offset_minutes); sunset.text = String(canonical.sunset_offset_minutes)
        label.text = canonical.location?.name || ""; latitude.text = canonical.location ? String(canonical.location.latitude) : ""; longitude.text = canonical.location ? String(canonical.location.longitude) : ""
    }
    function settings() {
        if (!matchColors && !fieldsValid) return JSON.parse(JSON.stringify(canonical))
        return {mode: mode, style: style, location_mode: locationMode, location: locationMode === "fixed" ? {name: label.text.trim() || "Fixed location", latitude: Number(latitude.text), longitude: Number(longitude.text)} : null,
            sunrise_offset_minutes: Number(sunrise.text), sunset_offset_minutes: Number(sunset.text), prefer_time_of_day: preferTimeOfDay}
    }
    function timeLabel(value) { return value ? Qt.formatDateTime(new Date(value), "ddd hh:mm") : "—" }
    function locationLabel() {
        var value = status.location
        if (!value) return ""
        var source = value.provider === "fixed" ? "Fixed coordinates" : value.provider === "omarchy-weather-ip-estimate" ? "Estimated area" : String(value.provider || "").indexOf("omarchy-weather") === 0 ? "Weather location" : "Current area"
        return " · " + source + ": " + value.name + (value.stale ? " (last known)" : "") + (value.resolved_at ? " · updated " + timeLabel(value.resolved_at) : "")
    }
    function operationLabel() {
        if (status.state === "suspended") return "Your manual theme is kept. Turn matching off and on to resume."
        if (status.error === "theme_generator_unavailable") return "Matugen 4.x is required to prepare new matching themes."
        if (status.error === "theme_ownership_lost") return "Choose an Omarchy base theme, then enable matching again."
        if (status.error) return "Theme update failed: " + status.error.replace(/_/g, " ")
        if (status.state === "preparing") return "Preparing matching colours…"
        if (status.state === "applying") return "Applying matching colours…"
        if (status.state === "waiting_for_photo") return "Choose photo sources to start matching colours."
        if (status.state === "active") return "Matching theme active · " + (status.mode === "light" ? "Light" : "Dark") + (status.style ? " · " + status.style.charAt(0).toUpperCase() + status.style.slice(1) : "")
        return "Theme matching is off."
    }
    Text { text: "SYSTEM THEME"; color: Commons.Color.accent; font.family: Style.font.family; font.pixelSize: Style.font.caption }
    RowLayout {
        Layout.fillWidth: true
        Ui.Button { text: "Match wallpaper colours"; focusable: true; selected: root.matchColors; onClicked: { root.matchColors = !root.matchColors; root.edited() } }
        Repeater {
            model: [{key: "auto", name: "Auto"}, {key: "light", name: "Light"}, {key: "dark", name: "Dark"}]
            Ui.Button { required property var modelData; text: modelData.name; focusable: true; enabled: root.matchColors; selected: root.mode === modelData.key; onClicked: { root.mode = modelData.key; root.edited() } }
        }
    }
    RowLayout {
        enabled: root.matchColors
        Text { text: "Style"; color: Commons.Color.foreground; font.pixelSize: Style.font.caption }
        Repeater {
            model: [{key: "neutral", name: "Neutral"}, {key: "subtle", name: "Subtle"}, {key: "tinted", name: "Tinted"}, {key: "rich", name: "Rich"}]
            Ui.Button { required property var modelData; objectName: "style-" + modelData.key; text: modelData.name; focusable: true; selected: root.style === modelData.key; onClicked: { root.style = modelData.key; root.edited() } }
        }
    }
    RowLayout {
        enabled: root.matchColors
        Text { text: "Sunrise / sunset location"; color: Commons.Color.foreground; font.pixelSize: Style.font.caption }
        Ui.Button { text: "Current area"; focusable: true; selected: root.locationMode === "current"; onClicked: { root.locationMode = "current"; root.edited() } }
        Ui.Button { text: "Fixed location"; focusable: true; selected: root.locationMode === "fixed"; onClicked: { root.locationMode = "fixed"; root.edited() } }
    }
    RowLayout {
        visible: root.locationMode === "fixed"
        enabled: root.matchColors; Layout.fillWidth: true
        Ui.TextField { id: label; objectName: "themeLocationLabel"; Layout.fillWidth: true; placeholderText: "Location label"; onTextEdited: root.edited() }
        Ui.TextField { id: latitude; objectName: "themeLatitude"; Layout.preferredWidth: 110; placeholderText: "Latitude"; validator: DoubleValidator { bottom: -90; top: 90; decimals: 6; locale: "C" } onTextEdited: root.edited() }
        Ui.TextField { id: longitude; objectName: "themeLongitude"; Layout.preferredWidth: 110; placeholderText: "Longitude"; validator: DoubleValidator { bottom: -180; top: 180; decimals: 6; locale: "C" } onTextEdited: root.edited() }
    }
    RowLayout {
        enabled: root.matchColors
        Text { text: "Offsets (minutes)"; color: Commons.Color.foreground; font.pixelSize: Style.font.caption }
        Text { text: "Sunrise"; color: Commons.Color.foreground; font.pixelSize: Style.font.caption }
        Ui.TextField { id: sunrise; objectName: "sunriseOffset"; text: "0"; Layout.preferredWidth: 70; validator: IntValidator { bottom: -180; top: 180 } onTextEdited: root.edited() }
        Text { text: "Sunset"; color: Commons.Color.foreground; font.pixelSize: Style.font.caption }
        Ui.TextField { id: sunset; objectName: "sunsetOffset"; text: "0"; Layout.preferredWidth: 70; validator: IntValidator { bottom: -180; top: 180 } onTextEdited: root.edited() }
    }
    Text {
        Layout.fillWidth: true; textFormat: Text.PlainText; wrapMode: Text.WordWrap
        text: !root.valid ? "Enter valid decimal coordinates and offsets from −180 to 180 minutes." : root.locationMode === "current" ? "Current area uses Omarchy’s configured weather location or its estimated area, with an offline cache." : "Fixed coordinates remain available offline."
        color: root.valid ? Commons.Color.foreground : Commons.Color.urgent; font.pixelSize: Style.font.caption
    }
    Text {
        objectName: "themeOperationStatus"
        Layout.fillWidth: true; textFormat: Text.PlainText; wrapMode: Text.WordWrap
        text: root.operationLabel() + root.locationLabel()
        color: root.status.error ? Commons.Color.urgent : Commons.Color.foreground; font.pixelSize: Style.font.caption
    }
    Text {
        Layout.fillWidth: true; visible: !!root.status.solar; wrapMode: Text.WordWrap
        text: "Sunrise " + root.timeLabel(root.status.solar?.next_sunrise) + " · Sunset " + root.timeLabel(root.status.solar?.next_sunset) + (root.status.solar?.polar ? " · Polar " + root.status.solar.polar : "")
        color: Commons.Color.foreground; font.pixelSize: Style.font.caption
    }
    Text { Layout.fillWidth: true; visible: !!root.status.location_error; text: "Current location unavailable; using the last known location or theme mode. Choose Fixed location to set it yourself."; wrapMode: Text.WordWrap; color: Commons.Color.urgent; font.pixelSize: Style.font.caption }
    Ui.Button {
        text: "Prefer photos for day/night mode"; selected: root.preferTimeOfDay; enabled: root.matchColors; focusable: true
        onClicked: { root.preferTimeOfDay = !root.preferTimeOfDay; root.edited() }
    }
    RowLayout {
        visible: root.hasCurrentPhoto
        Text { text: "This photo"; color: Commons.Color.foreground; font.pixelSize: Style.font.caption }
        Repeater {
            model: [{key: "auto", name: "Automatic"}, {key: "day", name: "Day"}, {key: "night", name: "Night"}, {key: "either", name: "Either"}]
            Ui.Button { required property var modelData; objectName: "period-" + modelData.key; text: modelData.name; focusable: true; selected: root.currentPeriod === modelData.key; onClicked: root.periodRequested(modelData.key) }
        }
    }
    Text { Layout.fillWidth: true; visible: root.matchColors; text: "Auto follows the sun even while the photo is paused or pinned. Photo preferences are flexible; matching can be turned off to restore your prior theme."; wrapMode: Text.WordWrap; color: Commons.Color.foreground; font.pixelSize: Style.font.caption }
}
