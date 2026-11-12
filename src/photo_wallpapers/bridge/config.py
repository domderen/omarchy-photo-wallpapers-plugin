from dataclasses import dataclass
from pathlib import Path

from ..io import read_json


def default_state():
    return Path.home() / "Library/Application Support/Omarchy Photo Bridge"


@dataclass
class BridgeConfig:
    library_path: Path
    state_path: Path
    refresh_seconds: int = 900
    cache_bytes: int = 5 * 1024**3
    port: int = 8741
    https_port: int = 8443
    legacy_media: bool = False


def load_config(path: Path) -> BridgeConfig:
    data = read_json(path)
    data["library_path"] = Path(data["library_path"]).expanduser().resolve()
    data["state_path"] = Path(data["state_path"]).expanduser().absolute()
    config = BridgeConfig(**data)
    if (
        type(config.legacy_media) is not bool
        or not 1024 <= config.port <= 65535
        or not 1024 <= config.https_port <= 65535
        or config.refresh_seconds < 60
        or config.cache_bytes < 1024**2
    ):
        raise ValueError("invalid bridge configuration")
    return config
