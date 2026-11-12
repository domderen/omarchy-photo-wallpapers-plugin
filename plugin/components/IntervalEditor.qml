import QtQuick
import QtQuick.Layouts
import qs.Commons
import qs.Commons as Commons
import qs.Ui as Ui
RowLayout {
    id: root
    readonly property int seconds: valid ? Number(input.text) : 0
    readonly property bool valid: /^\d+$/.test(input.text) && Number(input.text) >= 60 && Number(input.text) <= 31536000
    signal edited()
    function setSeconds(value) { input.text = String(value) }
    Text { text: "Rotate every"; color: Commons.Color.foreground; font.family: Style.font.family }
    Repeater {
        model: [{name: "15 min", seconds: 900}, {name: "1 hour", seconds: 3600}, {name: "4 hours", seconds: 14400}]
        Ui.Button { required property var modelData; text: modelData.name; focusable: true; selected: Number(input.text) === modelData.seconds; onClicked: { root.setSeconds(modelData.seconds); root.edited() } }
    }
    Ui.TextField { id: input; Layout.preferredWidth: 95; objectName: "intervalInput"; text: "3600"; onTextEdited: root.edited(); validator: IntValidator { bottom: 60; top: 31536000 } }
    Text { text: root.valid ? "seconds" : "Minimum 60 s"; color: root.valid ? Commons.Color.foreground : Commons.Color.urgent }
}
