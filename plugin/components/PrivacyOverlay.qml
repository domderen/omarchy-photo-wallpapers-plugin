import QtQuick
import QtQuick.Window
import Quickshell
import Quickshell.Io
import Quickshell.Wayland

Item {
    id: root
    property var engine: null
    property string statePath: Quickshell.env("PHOTO_WALLPAPERS_PRIVACY_STATE") || (Quickshell.env("PHOTO_WALLPAPERS_PROFILE") ? Quickshell.env("PHOTO_WALLPAPERS_PROFILE") + "/data/privacy-mode.json" : (Quickshell.env("XDG_DATA_HOME") || Quickshell.env("HOME") + "/.local/share") + "/omarchy-photo-wallpapers/privacy-mode.json")
    property bool retainedProtection: false
    property bool shadowMissing: false
    property var shadowValue: readShadow()
    readonly property var privacy: engine?.status?.privacy || ({})
    readonly property bool online: !!engine?.status?.daemon_running && !!engine?.connected
    readonly property bool protectedMode: !!privacy.protected || retainedProtection || !!shadowValue.protected
    readonly property string phase: online ? (privacy.phase || "preload") : "disconnected"
    readonly property string nonce: String(privacy.generation || 0) + ":" + (privacy.transition_id || "")
    readonly property string imagePath: !online || !privacy.protected ? "" : phase === "active" ? (engine.status.current?.render_path || "") : (privacy.pending_path || "")
    readonly property bool showTarget: online && (phase === "frame" || phase === "active")
    readonly property color neutralColor: engine?.status?.theme?.mode === "light" ? "#e3e9ef" : "#1e252d"
    function readShadow() {
        var text = shadow.text()
        if (shadowMissing) return {protected: false}
        try { var value = JSON.parse(text); if (value.schema_version === 1 && typeof value.protected === "boolean") return value } catch(error) {}
        return {protected: true}
    }
    function acceptStatus() {
        if (online && typeof privacy.protected === "boolean") retainedProtection = privacy.protected
    }
    function publishShadowProtection() { if (engine) engine.protectionRequestedFromShadow = root.protectedMode }
    onProtectedModeChanged: publishShadowProtection()
    onEngineChanged: publishShadowProtection()
    onPrivacyChanged: acceptStatus()
    onOnlineChanged: acceptStatus()
    FileView {
        id: shadow
        path: root.statePath
        blockLoading: true
        printErrors: false
        watchChanges: true
        onFileChanged: reload()
        onLoaded: { root.shadowMissing = false; root.shadowValue = root.readShadow(); root.retainedProtection = !!root.shadowValue.protected }
        onLoadFailed: function(error) { root.shadowMissing = error === FileViewError.FileNotFound; root.shadowValue = root.readShadow() }
    }
    Variants {
        model: Quickshell.screens
        PanelWindow {
            id: surface
            required property var modelData
            screen: modelData
            visible: root.protectedMode
            color: root.neutralColor
            anchors { top: true; bottom: true; left: true; right: true }
            exclusionMode: ExclusionMode.Ignore
            mask: Region {}
            WlrLayershell.namespace: "photo-wallpapers-privacy"
            WlrLayershell.layer: WlrLayer.Bottom
            WlrLayershell.keyboardFocus: WlrKeyboardFocus.None
            property string loadedNonce: ""
            property string framedNonce: ""
            property int frameCount: 0
            function acknowledge(operation) {
                if (root.online && root.engine && root.privacy.protected && root.privacy.transition_id && (root.privacy.outputs || []).indexOf(modelData.name) >= 0)
                    root.engine.privacyAck(operation, {generation: root.privacy.generation, transition_id: root.privacy.transition_id, output: modelData.name})
            }
            function ready() {
                if (picture.status === Image.Ready && picture.source.toString() && root.online && loadedNonce !== root.nonce) {
                    loadedNonce = root.nonce
                    acknowledge("preloaded")
                }
                if (root.showTarget && picture.status === Image.Ready && picture.Window.window) picture.Window.window.update()
            }
            Connections { target: root; function onNonceChanged() { surface.frameCount = 0; surface.loadedNonce = ""; surface.framedNonce = ""; Qt.callLater(surface.ready) } function onPhaseChanged() { surface.frameCount = 0; Qt.callLater(surface.ready) } }
            Image {
                id: picture
                anchors.fill: parent
                visible: root.showTarget && status === Image.Ready
                source: root.imagePath ? "file://" + root.imagePath : ""
                fillMode: Image.PreserveAspectCrop
                asynchronous: true
                cache: false
                onStatusChanged: {
                    if (status === Image.Error) surface.acknowledge("render_error")
                    else surface.ready()
                }
            }
            Connections {
                target: picture.Window.window
                function onFrameSwapped() {
                    if (!surface.visible || !root.showTarget || picture.status !== Image.Ready || root.phase !== "frame" || surface.framedNonce === root.nonce) return
                    surface.frameCount++
                    if (surface.frameCount >= 2) { surface.framedNonce = root.nonce; surface.acknowledge("frame") }
                    else picture.Window.window.update()
                }
            }
            Component.onCompleted: Qt.callLater(ready)
        }
    }
}
