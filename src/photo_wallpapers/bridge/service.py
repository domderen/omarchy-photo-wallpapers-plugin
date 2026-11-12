"""Small metadata HTTP owner; PhotosDB lives only in a bounded child process."""

import asyncio
import hashlib
import os
import tempfile
import threading
import time
from datetime import UTC, datetime
from pathlib import Path

from ..io import atomic_json, locked, owned_regular, private_dir
from .auth import ensure_token
from .extract import (
    POLICY,
    MetadataExtractor,
    digest_file,
    read_small_json,
    validate_summary,
)
from .metadata import source_signature, valid_cache


def create_service(config, *, extractor=None, provider=None):
    if config.legacy_media:
        from .legacy import LegacyBridgeService

        return LegacyBridgeService(config, extractor=extractor, provider=provider)
    if provider is not None:
        raise ValueError("metadata_only_has_no_media_provider")
    return BridgeService(config, extractor=extractor)


class BridgeService:
    def __init__(self, config, extractor=None):
        self.config = config
        private_dir(config.state_path)
        self.extractor = extractor or MetadataExtractor()
        self.refresh_lock = threading.Lock()
        self.idle = threading.Event()
        self.idle.set()
        self.closed = False
        self.namespace = hashlib.sha256(
            str(config.library_path.resolve()).encode()
        ).hexdigest()
        ensure_token(config.state_path)

    def _signature(self):
        if hasattr(self.extractor, "signature"):
            return self.extractor.signature(
                self.config.library_path, self.config.state_path
            )
        return source_signature(self.config.library_path)

    def _status(self):
        try:
            return read_small_json(self.config.state_path / "refresh.json")
        except (OSError, ValueError):
            return {}

    def status(self):
        return self._status() | {
            "mode": "metadata_only",
            "refreshing": self.refresh_lock.locked(),
            "pending": 0,
        }

    def _publish(self, candidate, marker):
        state = self.config.state_path
        target = state / "enrichment-v1.json"
        previous = candidate.parent / "previous.json"
        with locked(state / ".enrichment.lock"):
            had_previous = owned_regular(target, missing_ok=True) is not None
            if had_previous:
                os.link(target, previous)
            os.replace(candidate, target)
            try:
                atomic_json(state / "metadata-state.json", marker)
            except Exception:
                if had_previous:
                    os.replace(previous, target)
                else:
                    target.unlink(missing_ok=True)
                raise

    def refresh(self, force=False):
        if self.closed or not self.refresh_lock.acquire(blocking=False):
            return False
        self.idle.clear()
        before_status = self._status()
        counters = {
            key: before_status.get(key, 0)
            for key in ["snapshot_jobs", "extraction_jobs"]
        }
        started = time.monotonic()
        try:
            with locked(self.config.state_path / ".refresh.lock", blocking=False):
                before = self._signature()
                marker = (
                    None
                    if force
                    else valid_cache(self.config.state_path, self.namespace, before)
                )
                if marker is None:
                    counters = {key: value + 1 for key, value in counters.items()}
                    with tempfile.TemporaryDirectory(
                        prefix=".metadata-", dir=self.config.state_path
                    ) as working:
                        candidate = Path(working) / "enrichment.json"
                        summary = self.extractor.extract(
                            self.config.library_path,
                            Path(working) / "Photos.sqlite",
                            candidate,
                        )
                        size, digest = digest_file(candidate)
                        validate_summary(summary, self.namespace, size, digest)
                        after = self._signature()
                        if self.closed:
                            raise RuntimeError("metadata_extractor_closed")
                        marker = {
                            "policy": POLICY,
                            "source_signature": before if before == after else None,
                            "payload": summary,
                        }
                        self._publish(candidate, marker)
                    kind = "extracted"
                else:
                    kind = "unchanged"
                now = datetime.now(UTC).isoformat()
                summary = marker["payload"]
                value = (
                    before_status
                    | counters
                    | {
                        "mode": "metadata_only",
                        "last_success": now,
                        "last_checked": now,
                        "last_extracted": summary["created_at"],
                        "refresh_error": None,
                        "last_refresh_kind": kind,
                        "last_check_seconds": time.monotonic() - started,
                        "asset_count": summary["assets"],
                        "people_count": summary["people"],
                        "collection_count": summary["collections"],
                        "data_bytes": summary["bytes"],
                        "last_snapshot_seconds": summary["snapshot_seconds"],
                        "last_extract_seconds": summary["extract_seconds"],
                        "source_changed_during_extract": marker["source_signature"]
                        is None,
                    }
                )
                atomic_json(self.config.state_path / "refresh.json", value)
                return True
        except BlockingIOError:
            return False
        except Exception as error:  # noqa: BLE001 - retain last-good metadata on provider failure
            atomic_json(
                self.config.state_path / "refresh.json",
                before_status | counters | {"refresh_error": type(error).__name__},
            )
            return False
        finally:
            self.refresh_lock.release()
            self.idle.set()

    async def run(self):
        while not self.closed:
            await asyncio.to_thread(self.refresh)
            await asyncio.sleep(self.config.refresh_seconds)

    def close(self):
        self.closed = True
        self.extractor.close()
        self.idle.wait(5)
