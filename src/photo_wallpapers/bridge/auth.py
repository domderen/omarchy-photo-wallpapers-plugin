import os
import secrets
import stat
from pathlib import Path

from ..io import atomic_json, locked, open_private, private_dir, read_json


def ensure_token(state: Path):
    private_dir(state)
    with locked(state / ".credentials.lock"):
        path = state / "credentials.json"
        data = read_json(path)
        if not data:
            data = {"token": secrets.token_urlsafe(32)}
            atomic_json(path, data)
        if (
            not isinstance(data, dict)
            or not isinstance(data.get("token"), str)
            or not 32 <= len(data["token"]) <= 1024
        ):
            raise ValueError("invalid_credentials")
        with open_private(path) as handle:
            if stat.S_IMODE(os.fstat(handle.fileno()).st_mode) != 0o600:
                os.fchmod(handle.fileno(), 0o600)
        return data["token"]


def valid_token(state, supplied):
    try:
        data = read_json(state / "credentials.json", {}, limit=65536)
        token = data.get("token") if isinstance(data, dict) else None
    except (OSError, ValueError):
        return False
    return bool(
        isinstance(token, str)
        and token
        and supplied
        and secrets.compare_digest(token.encode("utf-8"), supplied.encode("utf-8"))
    )
