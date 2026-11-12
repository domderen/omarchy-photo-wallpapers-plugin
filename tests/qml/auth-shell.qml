import QtQuick
import Quickshell
import "../../plugin/components"
ShellRoot {
    AuthEditor {
        id: editor; width: 700; visible: false; daemonOnline: true
        onRequested: function(operation, input) {
            if (operation === "login" && input.username === "synthetic@example.invalid" && input.password === "synthetic-password") loginSent = true
            if (operation === "mfa" && input.code === "123456") mfaSent = true
        }
    }
    property bool loginSent: false
    property bool mfaSent: false
    function find(item, name) {
        if (item.objectName === name) return item
        for (var child of item.children || []) { var result = find(child, name); if (result) return result }
        return null
    }
    Component.onCompleted: Qt.callLater(function() {
        try {
            if (!editor.needsLogin) throw new Error("Fresh setup must expose Apple sign-in")
            var account = find(editor, "appleAccount"), password = find(editor, "applePassword"), code = find(editor, "appleCode")
            account.text = "synthetic@example.invalid"; password.text = "synthetic-password"
            editor.signIn()
            if (!loginSent || password.text !== "") throw new Error("Sign-in must send once and clear password immediately")
            editor.auth = {state: "mfa_required"}
            code.text = "123456"; editor.verify()
            if (!mfaSent || code.text !== "") throw new Error("MFA must clear the secret code immediately")
            password.text = "unsent"; code.text = "654321"; editor.clearSecrets()
            if (password.text || code.text) throw new Error("Closing settings must clear unsent secrets")
            editor.auth = {state: "device_approval_required"}
            if (editor.needsLogin) throw new Error("ADP approval must use its own state")
            console.log("PHOTO_WALLPAPERS_QML_CHECKS_PASS")
        } catch (error) { console.error("PHOTO_WALLPAPERS_QML_CHECKS_FAIL", error) }
        Qt.quit()
    })
}
