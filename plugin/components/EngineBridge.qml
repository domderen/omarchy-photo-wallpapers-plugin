import QtQuick
import Quickshell
import Quickshell.Io

Item {
    id: root
    property var status: ({})
    property bool protectionRequestedFromShadow: false
    readonly property bool privacyProtected: protectionRequestedFromShadow || !!status.privacy?.protected
    property string error: ""
    // Retained as an enable switch for isolated QML checks; status is event driven.
    property bool polling: true
    readonly property bool connected: socket.connected
    readonly property bool busy: Object.keys(pending).some(function(id) { return pending[id] !== "privacy-ack" })
    property var pending: ({})
    property int sequence: 0
    property string socketPath: Quickshell.env("PHOTO_WALLPAPERS_SOCKET") || (Quickshell.env("XDG_RUNTIME_DIR") ? Quickshell.env("XDG_RUNTIME_DIR") + "/omarchy-photo-wallpapers/engine.sock" : "")
    signal completed(string action, var result)

    function send(action, request) {
        if (!socket.connected) {
            root.error = "The wallpaper service is unavailable."
            completed(action, {error: "engine_unavailable"})
            return
        }
        var id = "qml-" + Date.now() + "-" + (++sequence)
        request.id = id
        var next = Object.assign({}, pending)
        if (action !== "subscribe" && action !== "status") next[id] = action
        pending = next
        socket.write(JSON.stringify(request) + "\n")
        socket.flush()
    }
    function privacyAck(operation, input) {
        if (socket.connected) send("privacy-ack", {command: "privacy", operation: operation, input: input})
    }
    function refreshStatus() { if (socket.connected) send("status", {command: "status"}) }
    function runAction(action, arguments, inputJson) {
        var allowed = ["config", "sources", "auth", "pair", "next", "previous", "pause", "resume", "pin", "unpin", "refresh", "feedback", "meeting", "models"]
        if (allowed.indexOf(action) < 0) return
        var args = arguments || [], request = {command: action, input: inputJson || null}
        if (action === "config") {
            request.operation = args[0] || "get"
            var revision = args.indexOf("--expected-revision")
            if (revision >= 0) request.expected_revision = Number(args[revision + 1])
        } else if (action === "auth" || action === "meeting" || action === "models") request.operation = args[0] || "resume"
        else if (action === "feedback") {
            request.input = Object.assign({}, inputJson || {})
            var feedback = args.indexOf("--action")
            if (feedback >= 0) request.input.action = args[feedback + 1]
        }
        root.error = ""
        send(action, request)
    }
    function receive(text) {
        try {
            var value = JSON.parse(text)
            if (value.event === "status") { status = value.status; return }
            var action = pending[value.id]
            var next = Object.assign({}, pending)
            delete next[value.id]
            pending = next
            if (!action) { if (value.result && value.result.daemon_running !== undefined) status = value.result; return }
            if (action === "privacy-ack") return
            if (value.result && value.result.error) error = value.result.error.replace(/_/g, " ")
            completed(action, value.result || {})
        } catch (e) { error = "The wallpaper service returned an unreadable response." }
    }
    function disconnect() {
        var previous = pending
        pending = ({})
        for (var id in previous) if (previous[id] !== "privacy-ack") completed(previous[id], {error: "engine_unavailable"})
        status = Object.assign({}, status, {daemon_running: false})
        if (polling) reconnect.restart()
    }
    Socket {
        id: socket
        path: root.socketPath
        parser: SplitParser { onRead: function(data) { root.receive(data) } }
        onConnectedChanged: {
            if (connected) { reconnect.stop(); root.error = ""; root.send("subscribe", {command: "subscribe"}) }
            else root.disconnect()
        }
        onError: function(socketError) { root.error = "Start the wallpaper service to continue."; root.disconnect() }
    }
    Timer { id: reconnect; interval: 2000; repeat: false; onTriggered: { if (root.polling) socket.connected = true } }
    onPollingChanged: { if (polling) socket.connected = true; else { reconnect.stop(); socket.connected = false } }
    Component.onCompleted: { if (polling) socket.connected = true }
}
