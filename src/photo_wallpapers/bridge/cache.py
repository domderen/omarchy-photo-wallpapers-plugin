import hashlib
import os
import threading
from contextlib import contextmanager
from pathlib import Path

from ..io import (
    atomic_bytes,
    atomic_json,
    open_private,
    owned_regular,
    private_dir,
    read_json,
)

MAX_CACHED_BYTES = 150 * 1024**2


class MediaCache:
    def __init__(self, root: Path, max_bytes: int):
        self.root = private_dir(root)
        self.max_bytes = max_bytes
        self.lock = threading.RLock()
        self.active = {}

    def lookup(self, key):
        with self.lock:
            path = self.root / (key + ".jpg")
            meta_path = self.root / (key + ".json")
            try:
                meta = read_json(meta_path, limit=65536)
                if not isinstance(meta, dict) or type(meta.get("bytes")) is not int:
                    return None
                with open_private(path) as handle:
                    size = os.fstat(handle.fileno()).st_size
                    if not 0 < size <= MAX_CACHED_BYTES or size != meta["bytes"]:
                        return None
                    data = handle.read(MAX_CACHED_BYTES + 1)
                    if (
                        len(data) != size
                        or hashlib.sha256(data).hexdigest() != meta["sha256"]
                    ):
                        return None
                    # Touch the verified descriptor, never a substituted path.
                    os.utime(handle.fileno(), None)
                return meta | {"path": str(path)}
            except (OSError, ValueError, KeyError, TypeError):
                return None

    def put(self, key, data, metadata):
        with self.lock:
            atomic_bytes(self.root / (key + ".jpg"), data)
            atomic_json(
                self.root / (key + ".json"),
                metadata
                | {"sha256": hashlib.sha256(data).hexdigest(), "bytes": len(data)},
            )

    @contextmanager
    def open(self, key):
        with self.lock:
            if not self.lookup(key):
                raise FileNotFoundError("cache missing or corrupt")
            handle = open_private(self.root / (key + ".jpg"))
            self.active[key] = self.active.get(key, 0) + 1
        try:
            yield handle
        finally:
            handle.close()
            with self.lock:
                self.active[key] -= 1

    def evict(self):
        with self.lock:
            files = []
            for path in self.root.glob("*.jpg"):
                try:
                    files.append((path, owned_regular(path)))
                except (OSError, ValueError):
                    continue
            files.sort(key=lambda entry: entry[1].st_mtime_ns)
            total = sum(info.st_size for _, info in files)
            for path, info in files:
                if total <= self.max_bytes:
                    break
                if self.active.get(path.stem, 0):
                    continue
                total -= info.st_size
                path.unlink()
                path.with_suffix(".json").unlink(missing_ok=True)
