import QtQuick
import QtQuick.Layouts
import qs.Commons
import qs.Commons as Commons
import qs.Ui as Ui

ColumnLayout {
    id: root
    property var auth: ({state: "not_signed_in"})
    property bool busy: false
    property bool daemonOnline: false
    readonly property string authState: auth.state || "not_signed_in"
    readonly property bool needsLogin: ["not_signed_in", "error", "offline", "session_expired"].indexOf(authState) >= 0
    signal requested(string operation, var input)
    function clearSecrets() { password.text = ""; code.text = "" }
    function signIn() {
        var input = {username: account.text.trim(), password: password.text}
        clearSecrets()
        requested("login", input)
    }
    function verify() {
        var input = {code: code.text}
        clearSecrets()
        requested("mfa", input)
    }
    onAuthChanged: {
        if (!account.text && auth.account) account.text = auth.account
        if (authState === "authenticated") clearSecrets()
    }
    Text {
        Layout.fillWidth: true
        text: root.authState === "authenticated" ? "Connected to iCloud · " + (root.auth.account || "") : root.authState === "mfa_required" ? "Enter the six-digit verification code from your trusted Apple device." : root.authState === "device_approval_required" ? "Approve Photos access on a trusted Apple device, then check again." : root.authState === "account_action_required" ? "Apple requires an account action. Complete it at iCloud.com, then retry." : root.authState === "session_saved" ? "Checking your saved Apple session…" : "Sign in to iCloud to download photos directly. A Mac helper is optional."
        textFormat: Text.PlainText; wrapMode: Text.WordWrap
        color: Commons.Color.foreground; font.family: Style.font.family
    }
    RowLayout {
        visible: root.needsLogin
        Layout.fillWidth: true
        Ui.TextField { id: account; objectName: "appleAccount"; Layout.fillWidth: true; placeholderText: "Apple Account"; enabled: !root.busy }
        Ui.TextField { id: password; objectName: "applePassword"; Layout.fillWidth: true; placeholderText: "Password"; password: true; enabled: !root.busy }
        Ui.Button { text: root.busy ? "Signing in…" : "Sign in"; focusable: true; enabled: root.daemonOnline && !root.busy && account.text.trim().length > 0 && password.text.length > 0; onClicked: root.signIn() }
    }
    RowLayout {
        visible: root.authState === "mfa_required"
        Layout.fillWidth: true
        Ui.TextField { id: code; objectName: "appleCode"; Layout.preferredWidth: 180; placeholderText: "Verification code"; password: true; validator: RegularExpressionValidator { regularExpression: /[0-9]{0,6}/ } enabled: !root.busy }
        Ui.Button { text: "Verify"; focusable: true; enabled: !root.busy && /^[0-9]{6}$/.test(code.text); onClicked: root.verify() }
    }
    RowLayout {
        Ui.Button { text: "Check approval"; focusable: true; visible: root.authState === "device_approval_required"; enabled: !root.busy; onClicked: root.requested("authorize", {}) }
        Ui.Button { text: "Open iCloud.com"; focusable: true; visible: root.authState === "account_action_required"; onClicked: Qt.openUrlExternally("https://www.icloud.com/photos/") }
        Ui.Button { text: "Retry saved session"; focusable: true; visible: root.authState !== "not_signed_in" && root.authState !== "mfa_required"; enabled: root.daemonOnline && !root.busy; onClicked: { root.clearSecrets(); root.requested("resume", {}) } }
        Ui.Button { text: root.authState === "error" ? "Reset sign-in" : "Sign out"; focusable: true; visible: root.authState !== "not_signed_in"; enabled: root.daemonOnline && !root.busy; onClicked: { root.clearSecrets(); root.requested("logout", {}) } }
        Text { text: root.auth.error ? root.auth.error.replace(/_/g, " ") : ""; textFormat: Text.PlainText; wrapMode: Text.WordWrap; Layout.fillWidth: true; color: Commons.Color.urgent; font.pixelSize: Style.font.caption }
    }
}
