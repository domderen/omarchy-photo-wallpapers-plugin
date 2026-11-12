import QtQuick
import qs.Commons
import qs.Commons as Commons
Text {
    property var status: ({})
    textFormat: Text.PlainText
    color: Commons.Color.foreground
    font.family: Style.font.family
    font.pixelSize: Style.font.caption
    wrapMode: Text.WordWrap
    text: {
        if (!status.daemon_running) return "Wallpaper service unavailable. Install the backend using the plugin README, or start omarchy-photo-wallpapers.service."
        if (status.protocol_version !== 1) return "Update the wallpaper backend and plugin together; see the plugin README." + (status.backend_version ? " Backend: " + status.backend_version : "")
        var connection = status.connection || "not_signed_in"
        var sync = status.last_sync ? Qt.formatTime(new Date(status.last_sync), "HH:mm") : "never"
        var due = status.paused ? "Paused" : status.pinned ? "Pinned" : status.next_due ? "Next at " + Qt.formatTime(new Date(status.next_due), "HH:mm") : "Ready"
        var cloud = connection === "online" ? "iCloud connected" : connection === "offline" || connection === "session_expired" ? "iCloud unavailable · using cached photos" : connection === "mfa_required" ? "Apple verification needed" : connection === "device_approval_required" ? "Apple device approval needed" : connection === "not_signed_in" ? "Sign in to iCloud" : connection.replace(/_/g, " ")
        var helper = status.helper || {}
        var metadata = helper.url ? helper.stale ? " · Mac offline, imported People retained" : " · Mac metadata ready" : " · Mac metadata optional"
        var progress = status.sync_progress || {}
        var syncing = progress.phase === "photos" ? " · Reading " + progress.library + " photos: " + progress.completed + " / " + progress.total : progress.phase === "albums" ? " · Reading albums: " + progress.completed + " / " + progress.total : progress.phase === "finishing" ? " · Saving the catalog…" : status.account_sync_pending ? " · Waiting for the account catalog" : status.preparing || status.pending_actions ? " · Preparing photos…" : ""
        return cloud + " · Synced " + sync + metadata + " · " + due + syncing + (status.error ? "\n" + status.error.replace(/_/g, " ") : "")
    }
}
