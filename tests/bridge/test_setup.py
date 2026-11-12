import plistlib

import pytest

from photo_wallpapers.bridge.config import BridgeConfig
from photo_wallpapers.bridge.setup import (
    check_listener,
    launch_agent_bytes,
    remove_owned_listener,
)


def test_existing_listener_is_preserved():
    current = {
        "TCP": {"443": {"HTTPS": True}, "8443": {"HTTPS": True}},
        "Web": {"example:443": {"Handlers": {"/": {"Proxy": "http://other"}}}},
    }
    with pytest.raises(ValueError):
        check_listener(current, 8443, None)
    assert current["TCP"]["443"] == {"HTTPS": True}


def test_install_is_idempotent():
    ours = {"HTTPS": True}
    check_listener({"TCP": {"8443": ours}}, 8443, {"TCP": {"8443": ours}})


def test_remove_only_owned_resources():
    calls = []
    before = {
        "TCP": {"443": {"HTTPS": True}, "8443": {"HTTPS": True}},
        "Web": {"mac:8443": {"Handlers": {"/": {"Proxy": "http://127.0.0.1:8741"}}}},
    }
    assert remove_owned_listener(
        before,
        8443,
        {
            "TCP": {"8443": {"HTTPS": True}},
            "Web": {"mac:8443": before["Web"]["mac:8443"]},
        },
        lambda args: calls.append(args),
    )
    assert calls == [["tailscale", "serve", "--https=8443", "off"]]
    calls.clear()
    assert not remove_owned_listener(
        before,
        8443,
        {"TCP": {"8443": {"TCPForward": "elsewhere"}}},
        lambda args: calls.append(args),
    )
    assert not calls


def test_permission_denial_is_actionable(tmp_path):
    config = BridgeConfig(tmp_path, tmp_path / "state")
    value = plistlib.loads(launch_agent_bytes(config, tmp_path / "config.json"))
    assert value["Label"] == "net.domderen.photo-bridge"
    assert value["ProgramArguments"][-2:] == ["--config", str(tmp_path / "config.json")]
    assert value["KeepAlive"] is True


def test_failed_prerequisite_does_not_register_startup(tmp_path, monkeypatch):
    from types import SimpleNamespace

    from photo_wallpapers.bridge import setup
    from photo_wallpapers.io import atomic_json

    config = tmp_path / "state/config.json"
    atomic_json(
        config,
        {"library_path": str(tmp_path / "library"), "state_path": str(config.parent)},
    )
    calls = []

    def runner(args, check=True):
        calls.append(args)
        return SimpleNamespace(stdout="{}", returncode=0)

    class FailedService:
        def __init__(self, c):
            pass

        def refresh(self):
            return False

        def close(self):
            pass

    monkeypatch.setattr(setup, "run", runner)
    monkeypatch.setattr(setup, "BridgeService", FailedService)
    with pytest.raises(PermissionError):
        setup.install_helper(config)
    assert not any("launchctl" in command for command in calls)
    assert not (config.parent / "ownership.json").exists()


def test_purge_preserves_unrelated_files_inside_state_directory(tmp_path, monkeypatch):
    from types import SimpleNamespace

    from photo_wallpapers.bridge import setup
    from photo_wallpapers.io import atomic_json

    state = tmp_path / "state"
    config = state / "config.json"
    atomic_json(
        config, {"library_path": str(tmp_path / "library"), "state_path": str(state)}
    )
    (state / "unrelated.txt").write_text("keep")
    monkeypatch.setattr(
        setup, "run", lambda *a, **k: SimpleNamespace(stdout="{}", returncode=0)
    )
    setup.remove_helper(config, purge=True)
    assert (state / "unrelated.txt").read_text() == "keep"


def test_initialization_refuses_nonempty_state_directory(tmp_path):
    from photo_wallpapers.bridge.cli import main

    library = tmp_path / "library"
    (library / "database").mkdir(parents=True)
    (library / "database/Photos.sqlite").touch()
    state = tmp_path / "state"
    state.mkdir()
    (state / "personal.txt").write_text("keep")
    assert (
        main(
            ["init", "--library", str(library), "--config", str(state / "config.json")]
        )
        == 1
    )
    assert not (state / "config.json").exists()
    assert (state / "personal.txt").read_text() == "keep"


def test_listener_port_change_requires_explicit_reinstallation(tmp_path, monkeypatch):
    from types import SimpleNamespace

    from photo_wallpapers.bridge import setup
    from photo_wallpapers.io import atomic_json

    config = tmp_path / "state/config.json"
    atomic_json(
        config,
        {
            "library_path": str(tmp_path / "library"),
            "state_path": str(config.parent),
            "https_port": 9443,
        },
    )
    listener = {
        "TCP": {"8443": {"HTTPS": True}},
        "Web": {"host:8443": {"Handlers": {"/": {"Proxy": "http://127.0.0.1:8741"}}}},
    }
    atomic_json(config.parent / "ownership.json", {"listener": listener})
    monkeypatch.setattr(
        setup,
        "run",
        lambda *a, **k: SimpleNamespace(
            stdout=__import__("json").dumps(listener), returncode=0
        ),
    )
    with pytest.raises(ValueError, match="listener settings"):
        setup.install_helper(config)


def test_metadata_install_requires_no_media_provider(tmp_path, monkeypatch):
    import json
    from types import SimpleNamespace

    from photo_wallpapers.bridge import setup
    from photo_wallpapers.io import atomic_json

    config = tmp_path / "state/config.json"
    atomic_json(
        config,
        {"library_path": str(tmp_path / "library"), "state_path": str(config.parent)},
    )
    monkeypatch.setattr(setup.Path, "home", lambda: tmp_path / "home")
    calls = []

    def runner(args, check=True):
        calls.append(args)
        return SimpleNamespace(
            stdout=json.dumps({"TCP": {"8443": {"HTTPS": True}}})
            if len(calls) > 1
            else "{}",
            returncode=0,
        )

    class MetadataService:
        def __init__(self, config):
            assert config.legacy_media is False

        def refresh(self):
            return True

        def close(self):
            pass

    monkeypatch.setattr(setup, "run", runner)
    monkeypatch.setattr(setup, "BridgeService", MetadataService)
    assert setup.install_helper(config)["installed"]
    assert (
        tmp_path / "home/Library/LaunchAgents/net.domderen.photo-bridge.plist"
    ).exists()
