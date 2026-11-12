import uvicorn

from photo_wallpapers.bridge import api, cli
from photo_wallpapers.bridge.config import BridgeConfig


def test_loopback_server_bounds_concurrency_and_header_buffers(monkeypatch, tmp_path):
    captured = {}
    monkeypatch.setattr(
        cli, "load_config", lambda _: BridgeConfig(tmp_path, tmp_path / "state")
    )
    monkeypatch.setattr(api, "create_app", lambda _: object())
    monkeypatch.setattr(uvicorn, "run", lambda app, **kwargs: captured.update(kwargs))
    assert cli.main(["serve", "--config", str(tmp_path / "config.json")]) == 0
    assert captured["host"] == "127.0.0.1"
    assert captured["access_log"] is False
    assert captured.get("limit_concurrency") == 32
    assert captured.get("backlog") == 64
    assert captured.get("h11_max_incomplete_event_size") == 16384
