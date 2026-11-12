"""Private change detection. Uncertain source/cache state requires extraction."""

import hashlib
import json
from pathlib import Path

from ..io import owned_regular
from .extract import POLICY, digest_file, read_small_json, validate_summary


def source_signature(library: Path):
    source = library.resolve() / "database/Photos.sqlite"
    files = []
    for path in [source, Path(str(source) + "-wal")]:
        try:
            value = owned_regular(path)
        except FileNotFoundError:
            if path == source:
                raise
            files.append(None)
            continue
        files.append(
            [
                value.st_dev,
                value.st_ino,
                value.st_size,
                value.st_mtime_ns,
                value.st_ctime_ns,
            ]
        )
    return hashlib.sha256(
        json.dumps([str(source), files], separators=(",", ":")).encode()
    ).hexdigest()


def valid_cache(state: Path, namespace: str, signature=None):
    try:
        marker = read_small_json(state / "metadata-state.json")
        summary = marker["payload"]
        if (
            marker.get("policy") != POLICY
            or summary.get("schema_version") != 1
            or summary.get("provider_namespace") != namespace
        ):
            return None
        if signature is not None and marker.get("source_signature") != signature:
            return None
        size, digest = digest_file(state / "enrichment-v1.json")
        validate_summary(summary, namespace, size, digest)
        return marker
    except (OSError, KeyError, TypeError, ValueError, AttributeError):
        return None
