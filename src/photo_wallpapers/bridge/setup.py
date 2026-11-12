"""Own only this LaunchAgent and one explicitly reserved Serve listener."""

import json
import os
import plistlib
import subprocess
import sys
from pathlib import Path

from ..io import atomic_bytes, atomic_json, private_dir, read_json
from .config import load_config
from .service import create_service as BridgeService

LABEL = "net.domderen.photo-bridge"


def run(args, check=True):
    return subprocess.run(args, check=check, capture_output=True, text=True, timeout=60)


def listener(config, port):
    suffix = ":" + str(port)
    return {
        "TCP": {str(port): config.get("TCP", {}).get(str(port))},
        "Web": {
            key: value
            for key, value in config.get("Web", {}).items()
            if key.endswith(suffix)
        },
        "AllowFunnel": {
            key: value
            for key, value in config.get("AllowFunnel", {}).items()
            if key.endswith(suffix)
        },
    }


def check_listener(current, port, owned):
    actual = listener(current, port)
    occupied = (
        actual["TCP"].get(str(port)) is not None
        or actual["Web"]
        or any(actual["AllowFunnel"].values())
    )
    if occupied and (not owned or actual != listener(owned, port)):
        raise ValueError(
            f"HTTPS listener {port} is already configured; choose another port"
        )
    if any(actual["AllowFunnel"].values()):
        raise ValueError("public Funnel is incompatible with this helper")


def remove_owned_listener(current, port, owned, runner=run):
    if (
        not owned
        or str(port) not in owned.get("TCP", {})
        or listener(current, port) != listener(owned, port)
    ):
        return False
    runner(["tailscale", "serve", f"--https={port}", "off"])
    return True


def launch_agent_bytes(config, config_path):
    return plistlib.dumps(
        {
            "Label": LABEL,
            "ProgramArguments": [
                sys.executable,
                "-m",
                "photo_wallpapers.bridge.cli",
                "serve",
                "--config",
                str(config_path.resolve()),
            ],
            "RunAtLoad": True,
            "KeepAlive": True,
            "EnvironmentVariables": {
                "PATH": "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin"
            },
            "StandardOutPath": str(config.state_path / "service.log"),
            "StandardErrorPath": str(config.state_path / "service-error.log"),
            "ThrottleInterval": 30,
        }
    )


def install_helper(config_path: Path) -> dict:
    config = load_config(config_path)
    ownership = read_json(config.state_path / "ownership.json", {})
    current = json.loads(run(["tailscale", "serve", "status", "--json"]).stdout or "{}")
    owned = ownership.get("listener")
    if owned:
        proxies = [
            handler.get("Proxy")
            for web in owned.get("Web", {}).values()
            for handler in web.get("Handlers", {}).values()
        ]
        if str(config.https_port) not in owned.get("TCP", {}) or proxies != [
            f"http://127.0.0.1:{config.port}"
        ]:
            raise ValueError(
                "listener settings changed; remove the helper before changing listener ports"
            )
    check_listener(current, config.https_port, ownership.get("listener"))
    # Verify prerequisites and a real exported image before registering resources.
    with_service = BridgeService(config)
    try:
        if not with_service.refresh():
            raise PermissionError(
                "Photos extraction failed; allow file access normally in macOS Privacy & Security, then retry"
            )
        assets = getattr(getattr(with_service, "provider", None), "_media", {})
        if not config.legacy_media:
            assets = {}
        if config.legacy_media and not assets:
            raise ValueError("No visible photos available for export verification")
        import time

        verified = False
        for asset_id, (revision, photo) in assets.items():
            if not photo.path and not photo.path_edited:
                continue
            for _ in range(650):
                result = with_service.worker.request(asset_id, revision, 1024)
                if result["state"] != "pending":
                    break
                time.sleep(0.1)
            if result["state"] == "ready":
                verified = True
                break
        if config.legacy_media and not verified:
            raise PermissionError(
                "Photos image export unavailable; open Photos and download an image, then retry"
            )
    finally:
        with_service.close()
    agent = Path.home() / "Library/LaunchAgents" / f"{LABEL}.plist"
    if agent.exists() and not ownership.get("launch_agent"):
        raise ValueError("Unowned LaunchAgent already exists; refusing to overwrite")
    private_dir(config.state_path)
    atomic_bytes(agent, launch_agent_bytes(config, config_path))
    ownership["launch_agent"] = str(agent)
    atomic_json(config.state_path / "ownership.json", ownership)
    domain = f"gui/{os.getuid()}"
    if run(["launchctl", "print", domain + "/" + LABEL], check=False).returncode:
        run(["launchctl", "bootstrap", domain, str(agent)])
    # Only this dedicated port is touched, never reset the global Serve config.
    if not ownership.get("listener"):
        run(
            [
                "tailscale",
                "serve",
                "--bg",
                f"--https={config.https_port}",
                f"http://127.0.0.1:{config.port}",
            ]
        )
        after = json.loads(run(["tailscale", "serve", "status", "--json"]).stdout)
        ownership["listener"] = listener(after, config.https_port)
        atomic_json(config.state_path / "ownership.json", ownership)
    return {"installed": True, "port": config.port, "https_port": config.https_port}


def remove_helper(config_path: Path, purge=False):
    config = load_config(config_path)
    ownership = read_json(config.state_path / "ownership.json", {})
    if ownership.get("launch_agent"):
        run(["launchctl", "bootout", f"gui/{os.getuid()}/{LABEL}"], check=False)
        Path(ownership["launch_agent"]).unlink(missing_ok=True)
    current = json.loads(run(["tailscale", "serve", "status", "--json"]).stdout or "{}")
    owned = ownership.get("listener")
    owned_ports = list((owned or {}).get("TCP", {}))
    removed = len(owned_ports) == 1 and remove_owned_listener(
        current, int(owned_ports[0]), owned
    )
    if purge:
        import shutil

        # A configured state directory can contain unrelated files. Purge only
        # this application's known data, never the directory that contains it.
        for name in ("catalogs", "media", "snapshot"):
            shutil.rmtree(config.state_path / name, ignore_errors=True)
        for name in (
            "credentials.json",
            ".credentials.lock",
            "refresh.json",
            "enrichment-v1.json",
            "metadata-state.json",
            ".enrichment.lock",
            ".refresh.lock",
            "ownership.json",
            "service.log",
            "service-error.log",
        ):
            (config.state_path / name).unlink(missing_ok=True)
        config_path.unlink(missing_ok=True)
    else:
        atomic_json(config.state_path / "ownership.json", {})
    return {"removed": True, "listener_removed": removed, "data_preserved": not purge}
