"""Short-lived OSXPhotos worker. Private paths travel over stdin, never argv."""

import hashlib
import json
import math
import os
import re
import subprocess
import sys
import tempfile
import threading
import time
from contextlib import contextmanager
from datetime import datetime
from pathlib import Path

from ..io import atomic_json, open_private, private_dir, read_json

MAX_BYTES = 64 * 1024**2
POLICY = "metadata-only-v1"


def digest_file(path: Path):
    with open_private(path) as handle:
        info = os.fstat(handle.fileno())
        if not 0 < info.st_size <= MAX_BYTES:
            raise ValueError("invalid_enrichment_file")
        digest = hashlib.sha256()
        total = 0
        while block := handle.read(256 * 1024):
            total += len(block)
            if total > MAX_BYTES:
                raise ValueError("invalid_enrichment_file")
            digest.update(block)
        if total != info.st_size:
            raise ValueError("enrichment_changed_during_read")
        return info.st_size, digest.hexdigest()


def read_small_json(path: Path):
    value = read_json(path, limit=65536)
    if not isinstance(value, dict):
        raise ValueError("invalid_metadata_state")  # noqa: TRY004 - malformed persisted data
    return value


def validate_summary(result, namespace, size, digest):
    if not isinstance(result, dict):
        raise ValueError("invalid_metadata_result")  # noqa: TRY004 - malformed persisted output
    if (
        result.get("provider_namespace") != namespace
        or result.get("schema_version") != 1
        or result.get("bytes") != size
        or result.get("sha256") != digest
    ):
        raise ValueError("invalid_metadata_result")
    datetime.fromisoformat(result["created_at"])
    for key in ["assets", "people", "collections"]:
        if type(result.get(key)) is not int or not 0 <= result[key] <= 1_000_000:
            raise ValueError("invalid_metadata_result")
    for key in ["snapshot_seconds", "extract_seconds"]:
        if (
            not isinstance(result.get(key), (int, float))
            or not math.isfinite(result[key])
            or result[key] < 0
        ):
            raise ValueError("invalid_metadata_result")
    return result


def validate_document(value, namespace):
    if (
        not isinstance(value, dict)
        or value.get("schema_version") != 1
        or value.get("provider_namespace") != namespace
        or value.get("cloud_id_type") != "CPLMaster.recordName"
    ):
        raise ValueError("invalid_enrichment_document")
    datetime.fromisoformat(value["created_at"])
    for key, limit in [
        ("assets", 1_000_000),
        ("people", 100_000),
        ("collections", 100_000),
    ]:
        if not isinstance(value.get(key), list) or len(value[key]) > limit:
            raise ValueError("invalid_enrichment_document")
    people = {p["id"] for p in value["people"]}
    collections = {c["id"] for c in value["collections"]}
    if len(people) != len(value["people"]) or len(collections) != len(
        value["collections"]
    ):
        raise ValueError("duplicate_enrichment_identity")
    seen = set()
    for asset in value["assets"]:
        local = asset["local_id"]
        if (
            not local
            or local in seen
            or not set(asset["person_ids"]) <= people
            or not set(asset["collection_ids"]) <= collections
        ):
            raise ValueError("invalid_enrichment_membership")
        seen.add(local)


class MetadataExtractor:
    def __init__(self, *, command=None, timeout=600):
        self.command = command or [
            sys.executable,
            "-m",
            "photo_wallpapers.bridge.extract",
        ]
        self.timeout = timeout
        self.lock = threading.Lock()
        self.closed = False
        self.process = None

    @staticmethod
    def terminate(process):
        if process.poll() is None:
            try:
                process.terminate()
            except ProcessLookupError:
                pass
            try:
                process.wait(timeout=3)
            except subprocess.TimeoutExpired:
                try:
                    process.kill()
                except ProcessLookupError:
                    pass
                process.wait(timeout=3)

    @contextmanager
    def child(self, request, timeout):
        with self.lock:
            if self.closed:
                raise RuntimeError("metadata_extractor_closed")
            if self.process is not None:
                raise RuntimeError("metadata_extractor_busy")
            process = subprocess.Popen(
                self.command,
                stdin=subprocess.PIPE,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
            )
            self.process = process
        try:
            try:
                process.communicate(json.dumps(request).encode(), timeout=timeout)
            except subprocess.TimeoutExpired:
                self.terminate(process)
                raise TimeoutError("metadata_extraction_timeout") from None
            if process.returncode != 0 or self.closed:
                raise RuntimeError("metadata_extraction_failed")
            yield
        finally:
            self.terminate(process)
            with self.lock:
                self.process = None

    def signature(self, library_path: Path, state: Path):
        # macOS can suspend even directory stat/open for a normal Photos prompt.
        # Keep that wait cancellable, away from the long-lived HTTP owner.
        private_dir(state)
        with tempfile.TemporaryDirectory(prefix=".probe-", dir=state) as working:
            output = Path(working) / "signature.json"
            request = {
                "mode": "signature",
                "library_path": str(library_path.resolve()),
                "output_path": str(output),
            }
            with self.child(request, min(self.timeout, 60)):
                value = read_small_json(output).get("signature")
                if (
                    not isinstance(value, str)
                    or re.fullmatch("[a-f0-9]{64}", value) is None
                ):
                    raise ValueError("invalid_source_signature")
                return value

    def extract(self, library_path: Path, snapshot_path: Path, output_path: Path):
        private_dir(output_path.parent)
        summary_path = output_path.with_name(output_path.name + ".summary")
        summary_path.unlink(missing_ok=True)
        request = {
            "library_path": str(library_path.resolve()),
            "snapshot_path": str(snapshot_path.resolve()),
            "output_path": str(output_path.resolve()),
            "summary_path": str(summary_path.resolve()),
        }
        try:
            with self.child(request, self.timeout):
                try:
                    result = read_small_json(summary_path)
                    size, digest = digest_file(output_path)
                    expected = hashlib.sha256(
                        str(library_path.resolve()).encode()
                    ).hexdigest()
                    validate_summary(result, expected, size, digest)
                    return result
                except (OSError, ValueError, KeyError, TypeError):
                    raise RuntimeError("metadata_extraction_invalid_output") from None
        finally:
            summary_path.unlink(missing_ok=True)

    def close(self):
        with self.lock:
            self.closed = True
            process = self.process
        if process is not None:
            self.terminate(process)


def main():
    try:
        request = json.loads(sys.stdin.buffer.read(65537))
        library = Path(request["library_path"])
        if request.get("mode") == "signature":
            from .metadata import source_signature

            atomic_json(
                Path(request["output_path"]), {"signature": source_signature(library)}
            )
            return 0
        snapshot = Path(request["snapshot_path"])
        output = Path(request["output_path"])
        os.nice(10)
        from .photos import PhotosAdapter
        from .snapshot import capture_database

        start = time.monotonic()
        capture_database(library / "database/Photos.sqlite", snapshot)
        captured = time.monotonic()
        document = (
            PhotosAdapter(library).extract(snapshot, metadata_only=True).enrichment
        )
        namespace = hashlib.sha256(str(library.resolve()).encode()).hexdigest()
        validate_document(document, namespace)
        atomic_json(output, document)
        size, digest = digest_file(output)
        summary = {
            k: document[k]
            for k in ["schema_version", "provider_namespace", "created_at"]
        }
        summary.update(
            {k: len(document[k]) for k in ["assets", "people", "collections"]}
        )
        summary.update(
            {
                "sha256": digest,
                "bytes": size,
                "snapshot_seconds": captured - start,
                "extract_seconds": time.monotonic() - captured,
            }
        )
        atomic_json(Path(request["summary_path"]), summary)
        return 0
    except Exception:  # noqa: BLE001 - never expose third-party metadata exceptions
        # Third-party exceptions can contain private paths and metadata.
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
