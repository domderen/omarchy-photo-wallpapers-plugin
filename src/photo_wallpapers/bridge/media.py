import hashlib
import io
import json
import os
import tempfile
import threading
import time
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

from ..imaging.color import oriented_srgb
from ..io import open_private

MAX_SOURCE_BYTES = 150 * 1024**2


def register_safe_heif():
    try:
        import pillow_heif
    except ImportError:
        return
    try:
        version = tuple(
            int(part) for part in pillow_heif.libheif_info()["libheif"].split(".")
        )
        if len(version) != 3 or version < (1, 23, 7):
            raise ValueError("old_heif")
    except (ValueError, KeyError, TypeError):
        raise RuntimeError("unsafe_heif_runtime") from None
    pillow_heif.register_heif_opener()


class MediaWorker:
    def __init__(self, provider, cache, retry_seconds=30, max_jobs=32):
        self.provider = provider
        self.cache = cache
        self.retry_seconds = retry_seconds
        self.max_jobs = max_jobs
        self.executor = ThreadPoolExecutor(
            max_workers=1, thread_name_prefix="photo-media"
        )
        self.lock = threading.RLock()
        self.jobs = {}
        self.closed = False

    def __enter__(self):
        return self

    def __exit__(self, *args):
        self.close()

    def close(self):
        with self.lock:
            self.closed = True
        self.executor.shutdown(wait=True, cancel_futures=True)

    def request(self, asset_id, revision, max_edge):
        if not isinstance(max_edge, int) or not 128 <= max_edge <= 8192:
            raise ValueError("max_edge must be 128..8192")
        self.provider.resolve(asset_id, revision)
        key = hashlib.sha256(
            json.dumps([asset_id, revision, max_edge, "srgb-jpeg-v1"]).encode()
        ).hexdigest()
        with self.lock:
            if self.closed:
                raise RuntimeError("worker stopped")
            cached = self.cache.lookup(key)
            if cached:
                return cached | {"state": "ready", "key": key}
            job = self.jobs.get(key)
            if job and (
                job["state"] == "pending"
                or (
                    job["state"] == "failed"
                    and time.monotonic() - job["updated"] < self.retry_seconds
                )
            ):
                return job.copy()
            if (
                sum(j["state"] == "pending" for j in self.jobs.values())
                >= self.max_jobs
            ):
                return {"state": "pending", "retry_after": 5, "reason": "queue_full"}
            self.jobs[key] = {
                "state": "pending",
                "key": key,
                "retry_after": 2,
                "updated": time.monotonic(),
            }
            self.executor.submit(self._prepare, key, asset_id, revision, max_edge)
            return self.jobs[key].copy()

    def _prepare(self, key, asset_id, revision, max_edge):
        try:
            with tempfile.TemporaryDirectory(dir=self.cache.root) as working:
                source, rendition = self.provider.export(
                    asset_id, revision, Path(working)
                )
                # HEIF/AVIF ISO-BMFF identifies itself before any native decoder runs.
                # Ordinary JPEG/PNG exports do not load the optional codec.
                with open_private(source) as handle:
                    if not 0 < os.fstat(handle.fileno()).st_size <= MAX_SOURCE_BYTES:
                        raise ValueError("image_size_limit")
                    header = handle.read(12)
                    is_heif = header[4:8] == b"ftyp"
                    if is_heif:
                        register_safe_heif()
                    elif not header.startswith((b"\xff\xd8\xff", b"\x89PNG\r\n\x1a\n")):
                        # Never dispatch unexpected formats to Pillow plugins
                        # that can invoke external programs (for example EPS).
                        raise ValueError("unsupported_image_type")
                    handle.seek(0)
                    image = oriented_srgb(
                        handle, formats=["HEIF"] if is_heif else ["JPEG", "PNG"]
                    )
                image.thumbnail((max_edge, max_edge))
                data = io.BytesIO()
                image.save(data, format="JPEG", quality=92)
                self.provider.resolve(asset_id, revision)
                self.cache.put(
                    key,
                    data.getvalue(),
                    {
                        "asset_id": asset_id,
                        "revision": revision,
                        "max_edge": max_edge,
                        "rendition": rendition,
                        "width": image.width,
                        "height": image.height,
                        "face_bounds": False,
                    },
                )
            with self.lock:
                self.jobs[key] = {
                    "state": "ready",
                    "key": key,
                    "updated": time.monotonic(),
                }
                # Keep diagnostic memory bounded as well as queue and disk use.
                for old in list(self.jobs):
                    if len(self.jobs) <= 256:
                        break
                    if self.jobs[old]["state"] != "pending":
                        del self.jobs[old]
            self.cache.evict()
        except Exception as error:  # noqa: BLE001 - sanitize external decoder/provider diagnostics
            # Exception messages can contain local paths or provider metadata.
            with self.lock:
                self.jobs[key] = {
                    "state": "failed",
                    "key": key,
                    "error": type(error).__name__,
                    "retry_after": self.retry_seconds,
                    "updated": time.monotonic(),
                }
