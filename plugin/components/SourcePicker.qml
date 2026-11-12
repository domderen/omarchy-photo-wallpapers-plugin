import QtQuick
import QtQuick.Layouts
import qs.Commons
import qs.Commons as Commons
import qs.Ui as Ui
ColumnLayout {
    id: root
    property var sources: []
    property var selected: []
    readonly property var filtered: sources.filter(function(row) { return (String(row.name) + " " + row.kind + (row.kind === "person" ? " people" : "")).toLowerCase().indexOf(search.text.toLowerCase()) >= 0 })
    signal selectionChangedByUser(var value)
    function isSelected(row) { return selected.some(function(s) { return s.id === row.id && s.kind === row.kind }) }
    function toggle(row) {
        var next = selected.filter(function(s) { return !(s.id === row.id && s.kind === row.kind) })
        if (!isSelected(row)) next.push({id: row.id, kind: row.kind})
        selectionChangedByUser(next)
    }
    Ui.TextField { id: search; Layout.fillWidth: true; placeholderText: "Search library, albums and People" }
    ListView {
        Layout.fillWidth: true
        Layout.preferredHeight: root.filtered.length ? 170 : 60
        clip: true
        model: root.filtered
        spacing: 2
        section.property: "kind"
        section.delegate: Text {
            required property string section
            text: section === "library" ? "LIBRARY" : section === "person" ? "PEOPLE" : section === "shared_album" ? "SHARED ALBUMS" : "ALBUMS"
            color: Commons.Color.accent
            font.family: Style.font.family
            font.pixelSize: Style.font.caption
            height: 24
            verticalAlignment: Text.AlignVCenter
        }
        delegate: Ui.Button {
            required property var modelData
            width: ListView.view.width
            height: 32
            focusable: true
            selected: root.isSelected(modelData)
            leftAlign: true
            text: (selected ? "☑  " : "☐  ") + modelData.name + "  ·  " + modelData.count + (modelData.available ? "" : "  · unavailable") + (String(modelData.provider || "").indexOf("mac:") === 0 ? "  · Mac metadata" : "") + (modelData.stale ? "  · last imported" : "")
            onClicked: root.toggle(modelData)
            // Shared Ui.Button renders its label as Text.PlainText.
        }
        Text {
            anchors.centerIn: parent
            visible: root.filtered.length === 0
            text: root.sources.length ? "No matching sources." : "Sources appear after iCloud sync. Optional Mac metadata adds People."
            textFormat: Text.PlainText
            color: Commons.Color.foreground
        }
    }
    Text { text: root.selected.length + " sources selected · overlapping photos appear once"; color: Commons.Color.foreground; font.pixelSize: Style.font.caption }
}
