import json
from pathlib import Path

ROOT = Path(__file__).parents[2] / "plugin"


def test_manifest_and_safe_engine_invocation():
    manifest = json.loads((ROOT.parent / "manifest.json").read_text())
    assert manifest["id"] == "domderen.photo-wallpapers"
    assert set(manifest["kinds"]) == {"service", "bar-widget", "panel"}
    for path in manifest["entryPoints"].values():
        assert (ROOT.parent / path).is_file()
    bridge = (ROOT / "components/EngineBridge.qml").read_text()
    assert "Socket {" in bridge and "write(" in bridge and "flush()" in bridge
    assert "Process {" not in bridge
    assert '"sh"' not in bridge and '"bash"' not in bridge
    assert "JSON.stringify" in bridge and "subscribe" in bridge
    assert "SplitParser" in bridge
    assert "JSON.parse" in bridge and "catch" in bridge


def test_native_settings_contract():
    text = (ROOT / "Settings.qml").read_text()
    assert "expected" in text.lower() and "revision" in text
    assert "Save" in text and "Cancel" in text
    assert "Commons.Color" in text
    source = (ROOT / "components/SourcePicker.qml").read_text()
    assert ".id" in source and ".kind" in source and "Text.PlainText" in source
    interval = (ROOT / "components/IntervalEditor.qml").read_text()
    assert "3600" in interval and "60" in interval
    connection = (ROOT / "components/ConnectionEditor.qml").read_text()
    assert "password: true" in connection and 'token.text = ""' in connection


def test_controls_contract():
    text = (ROOT / "components/Controls.qml").read_text()
    for action in (
        "next",
        "previous",
        "pause",
        "resume",
        "pin",
        "unpin",
        "refresh",
        "never",
    ):
        assert '"' + action + '"' in text
    assert "preview_path" in (ROOT / "components/PhotoPreview.qml").read_text()


def test_privacy_controls_keep_one_socket_and_startup_frame_confirmation():
    editor = (ROOT / "components/PrivacyEditor.qml").read_text()
    assert "Swimwear is allowed" in editor and "sleepwear" in editor.lower()
    assert "Personal only" in editor and "Allow" in editor and "Auto" in editor
    assert "filterPersonal" in editor and "meetingRequested" in editor
    service = (ROOT / "Service.qml").read_text()
    assert "PrivacyOverlay" in service
    overlay = (ROOT / "components/PrivacyOverlay.qml").read_text()
    assert "FileView" in overlay and "watchChanges: true" in overlay
    assert "onFrameSwapped" in overlay and "preloaded" in overlay
    assert "WlrLayer.Bottom" in overlay and "mask: Region" in overlay
    assert "_backingWindow" not in overlay and "Process {" not in overlay
    bridge = (ROOT / "components/EngineBridge.qml").read_text()
    assert "privacyAck" in bridge and '"models"' in bridge and '"meeting"' in bridge
    settings = (ROOT / "Settings.qml").read_text()
    assert "privacy: {filter_personal:" in settings and "PrivacyEditor" in settings
    assert "meeting" in (ROOT / "BarWidget.qml").read_text()
