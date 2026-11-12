import QtQuick
import qs.Commons as Commons
Rectangle {
    id: root
    property var current: null
    property bool privacyProtected: false
    property bool modeConfirmed: false
    readonly property bool neutral: current?.provenance === "meeting-neutral"
    readonly property bool withheld: privacyProtected && !modeConfirmed
    implicitHeight: 160
    radius: 8
    color: Commons.Color.background
    Image {
        objectName: "photoPreviewImage"
        anchors.fill: parent
        source: !root.neutral && !root.withheld && root.current && root.current.preview_path ? "file://" + root.current.preview_path : ""
        fillMode: Image.PreserveAspectFit
        asynchronous: true
        sourceSize: Qt.size(640, 400)
        cache: true
    }
    Text {
        anchors.centerIn: parent
        visible: root.neutral || root.withheld || !root.current
        text: root.withheld ? "Privacy protection · preview withheld until the desktop is confirmed" : root.neutral ? "Neutral background" : "Choose sources, then press Next"
        width: parent.width - 24
        horizontalAlignment: Text.AlignHCenter
        wrapMode: Text.WordWrap
        color: Commons.Color.foreground
    }
}
