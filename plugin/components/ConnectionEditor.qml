import QtQuick
import QtQuick.Layouts
import qs.Ui as Ui
ColumnLayout {
    id: root
    property string helperUrl: ""
    signal pairRequested(var connection)
    function clearToken() { token.text = "" }
    Ui.TextField { id: helper; Layout.fillWidth: true; text: root.helperUrl; placeholderText: "https://your-mac.tailnet.ts.net:8443" }
    RowLayout {
        Layout.fillWidth: true
        Ui.TextField { id: token; Layout.fillWidth: true; password: true; placeholderText: "Pairing token (stored privately)" }
        Ui.Button { text: "Pair helper"; focusable: true; enabled: helper.text.length > 0 && token.text.length >= 32; onClicked: { root.pairRequested({helper_url: helper.text, token: token.text}); token.text = "" } }
    }
}
