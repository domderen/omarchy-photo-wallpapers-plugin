import asyncio
import threading
from datetime import UTC, datetime

from ..catalog.publish import publish_catalog
from ..io import atomic_json, locked, private_dir, read_json
from .auth import ensure_token
from .cache import MediaCache
from .media import MediaWorker
from .photos import PhotosAdapter
from .snapshot import capture_database


class LegacyBridgeService:
    def __init__(self, config, extractor=None, provider=None):
        self.config = config
        private_dir(config.state_path)
        self.catalogs = private_dir(config.state_path / "catalogs")
        self.provider = provider or PhotosAdapter(config.library_path)
        self.extractor = extractor or self._extract
        self.cache = MediaCache(config.state_path / "media", config.cache_bytes)
        self.worker = MediaWorker(self.provider, self.cache)
        self.refresh_lock = threading.Lock()
        self.closed = False
        ensure_token(config.state_path)

    def _extract(self):
        path = capture_database(
            self.config.library_path / "database/Photos.sqlite",
            self.config.state_path / "snapshot/Photos.sqlite",
        )
        provider = PhotosAdapter(self.config.library_path)
        snapshot = provider.extract(path)
        self.pending_provider = provider
        return snapshot

    def refresh(self, force=False):
        if not self.refresh_lock.acquire(blocking=False):
            return False
        try:
            with locked(self.config.state_path / ".refresh.lock"):
                snapshot = self.extractor()
                publish_catalog(snapshot, self.catalogs)
                if snapshot.enrichment:
                    atomic_json(
                        self.config.state_path / "enrichment-v1.json",
                        snapshot.enrichment,
                    )
                if getattr(self, "pending_provider", None) is not None:
                    self.provider = self.pending_provider
                    self.worker.provider = self.provider
                    self.pending_provider = None
                atomic_json(
                    self.config.state_path / "refresh.json",
                    {
                        "last_success": datetime.now(UTC).isoformat(),
                        "refresh_error": None,
                        "capabilities": snapshot.capabilities,
                    },
                )
            return True
        except Exception as error:  # noqa: BLE001 - persist only sanitized provider error type
            previous = read_json(self.config.state_path / "refresh.json", {})
            atomic_json(
                self.config.state_path / "refresh.json",
                previous | {"refresh_error": type(error).__name__},
            )
            return False
        finally:
            self.refresh_lock.release()

    def manifest(self):
        return read_json(self.catalogs / "manifest.json")

    def status(self):
        return read_json(self.config.state_path / "refresh.json", {}) | {
            "generation": (self.manifest() or {}).get("generation"),
            "refreshing": self.refresh_lock.locked(),
            "pending": sum(
                j["state"] == "pending" for j in self.worker.jobs.copy().values()
            ),
        }

    async def run(self):
        while not self.closed:
            await asyncio.to_thread(self.refresh)
            await asyncio.sleep(self.config.refresh_seconds)

    def close(self):
        self.closed = True
        self.worker.close()
