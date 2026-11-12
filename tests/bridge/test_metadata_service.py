import hashlib
import json
import sqlite3
import threading
from datetime import UTC, datetime

import pytest

from photo_wallpapers.bridge.config import BridgeConfig
from photo_wallpapers.io import atomic_json, locked


@pytest.fixture
def library(tmp_path):
    path = tmp_path / "library"
    (path / "database").mkdir(parents=True)
    db = sqlite3.connect(path / "database/Photos.sqlite", check_same_thread=False)
    db.execute("PRAGMA journal_mode=WAL")
    db.execute("CREATE TABLE example(value)")
    db.execute("INSERT INTO example VALUES (1)")
    db.commit()
    yield path, db
    db.close()


class FakeExtractor:
    def __init__(self, callback=None):
        self.calls = 0
        self.callback = callback
        self.closed = False

    def extract(self, library_path, snapshot_path, output_path):
        self.calls += 1
        if self.callback:
            self.callback()
        namespace = hashlib.sha256(str(library_path.resolve()).encode()).hexdigest()
        doc = {
            "schema_version": 1,
            "provider_namespace": namespace,
            "cloud_id_type": "CPLMaster.recordName",
            "created_at": datetime.now(UTC).isoformat(),
            "people": [{"id": "p", "name": "Synthetic"}],
            "collections": [],
            "assets": [
                {
                    "local_id": "a",
                    "cloud_master_guid": "master",
                    "owner_hash": None,
                    "person_ids": ["p"],
                    "collection_ids": [],
                    "keywords": [],
                }
            ],
            "capabilities": {"people": True, "face_bounds": False},
        }
        atomic_json(output_path, doc)
        data = output_path.read_bytes()
        return {
            "schema_version": 1,
            "provider_namespace": namespace,
            "created_at": doc["created_at"],
            "people": 1,
            "assets": 1,
            "collections": 0,
            "bytes": len(data),
            "sha256": hashlib.sha256(data).hexdigest(),
            "snapshot_seconds": 0.01,
            "extract_seconds": 0.02,
        }

    def close(self):
        self.closed = True


def make_service(tmp_path, library, worker=None):
    from photo_wallpapers.bridge.service import BridgeService

    return BridgeService(
        BridgeConfig(library[0], tmp_path / "state"),
        extractor=worker or FakeExtractor(),
    )


def test_default_is_metadata_only_without_legacy_allocations(tmp_path, library):
    from photo_wallpapers.bridge.service import create_service

    config = BridgeConfig(library[0], tmp_path / "state")
    assert config.legacy_media is False
    service = create_service(config, extractor=FakeExtractor())
    assert service.refresh()
    assert not hasattr(service, "worker") and not hasattr(service, "provider")
    assert not (config.state_path / "media").exists()
    assert not (config.state_path / "catalogs").exists()
    assert service.status()["mode"] == "metadata_only"
    service.close()


def test_unchanged_source_and_restart_reuse_verified_metadata(tmp_path, library):
    first = FakeExtractor()
    service = make_service(tmp_path, library, first)
    assert service.refresh()
    output = tmp_path / "state/enrichment-v1.json"
    original = output.read_bytes()
    modified = output.stat().st_mtime_ns
    assert service.refresh() and first.calls == 1
    assert service.status()["last_refresh_kind"] == "unchanged"
    assert output.read_bytes() == original and output.stat().st_mtime_ns == modified
    service.close()
    next_worker = FakeExtractor()
    restarted = make_service(tmp_path, library, next_worker)
    assert restarted.refresh() and next_worker.calls == 0
    assert restarted.status()["extraction_jobs"] == 1
    assert restarted.refresh(force=True) and next_worker.calls == 1
    restarted.close()


def test_wal_commit_and_checkpoint_invalidate_reuse(tmp_path, library):
    worker = FakeExtractor()
    service = make_service(tmp_path, library, worker)
    assert service.refresh()
    database = library[0] / "database/Photos.sqlite"
    before = database.stat().st_mtime_ns
    library[1].execute("INSERT INTO example VALUES (2)")
    library[1].commit()
    assert database.stat().st_mtime_ns == before
    assert service.refresh() and worker.calls == 2
    library[1].execute("PRAGMA wal_checkpoint(TRUNCATE)")
    assert service.refresh() and worker.calls == 3
    service.close()


@pytest.mark.parametrize(
    "broken", ["payload", "marker", "namespace", "summary", "shape"]
)
def test_corrupt_cached_state_forces_rebuild(tmp_path, library, broken):
    worker = FakeExtractor()
    service = make_service(tmp_path, library, worker)
    assert service.refresh()
    if broken == "payload":
        (tmp_path / "state/enrichment-v1.json").write_text("corrupt")
    elif broken == "marker":
        (tmp_path / "state/metadata-state.json").write_text("broken json")
    else:
        p = tmp_path / "state/metadata-state.json"
        v = json.loads(p.read_text())
        if broken == "namespace":
            v["payload"]["provider_namespace"] = "other-library"
        elif broken == "summary":
            del v["payload"]["created_at"]
        else:
            v["payload"] = []
        atomic_json(p, v)
    assert service.refresh() and worker.calls == 2
    assert (
        json.loads((tmp_path / "state/enrichment-v1.json").read_text())[
            "schema_version"
        ]
        == 1
    )
    service.close()


def test_source_changing_during_capture_is_not_trusted_for_next_skip(tmp_path, library):
    def change():
        library[1].execute("INSERT INTO example VALUES (2)")
        library[1].commit()

    worker = FakeExtractor(change)
    service = make_service(tmp_path, library, worker)
    assert service.refresh()
    assert (
        json.loads((tmp_path / "state/metadata-state.json").read_text())[
            "source_signature"
        ]
        is None
    )
    worker.callback = None
    assert service.refresh() and worker.calls == 2
    assert service.refresh() and worker.calls == 2
    service.close()


def test_failed_refresh_keeps_data_but_does_not_report_freshness(tmp_path, library):
    worker = FakeExtractor()
    service = make_service(tmp_path, library, worker)
    assert service.refresh()
    before = (tmp_path / "state/enrichment-v1.json").read_bytes()
    success = service.status()["last_success"]

    def fail():
        raise PermissionError("private person, path or token")

    worker.callback = fail
    assert service.refresh(force=True) is False
    assert (tmp_path / "state/enrichment-v1.json").read_bytes() == before
    status = service.status()
    assert (
        status["last_success"] == success
        and status["refresh_error"] == "PermissionError"
    )
    assert "private person" not in str(status)
    assert not list((tmp_path / "state").glob(".metadata-*"))
    service.close()


def test_manual_force_cli_reaches_extractor(tmp_path, library, monkeypatch, capsys):
    from photo_wallpapers.bridge import service as module
    from photo_wallpapers.bridge.cli import main

    worker = FakeExtractor()
    service = make_service(tmp_path, library, worker)
    config = tmp_path / "config.json"
    atomic_json(
        config,
        {"library_path": str(library[0]), "state_path": str(service.config.state_path)},
    )
    assert service.refresh()
    monkeypatch.setattr(module, "create_service", lambda config: service)
    assert main(["refresh", "--force", "--config", str(config)]) == 0
    assert worker.calls == 2
    assert json.loads(capsys.readouterr().out)["last_refresh_kind"] == "extracted"


def test_marker_publication_failure_restores_previous_payload(
    tmp_path, library, monkeypatch
):
    import photo_wallpapers.bridge.service as module

    service = make_service(tmp_path, library)
    assert service.refresh()
    before = (tmp_path / "state/enrichment-v1.json").read_bytes()
    real = module.atomic_json

    def fail(path, value):
        if path.name == "metadata-state.json":
            raise OSError("private detail")
        return real(path, value)

    monkeypatch.setattr(module, "atomic_json", fail)
    assert service.refresh(force=True) is False
    assert (tmp_path / "state/enrichment-v1.json").read_bytes() == before
    service.close()


def test_duplicate_and_cross_process_refresh_do_not_start_more_work(tmp_path, library):
    entered = threading.Event()
    release = threading.Event()

    def block():
        entered.set()
        assert release.wait(3)

    worker = FakeExtractor(block)
    service = make_service(tmp_path, library, worker)
    result = []
    thread = threading.Thread(target=lambda: result.append(service.refresh()))
    thread.start()
    assert entered.wait(3)
    assert service.refresh() is False and worker.calls == 1
    release.set()
    thread.join(3)
    assert result == [True]
    with locked(tmp_path / "state/.refresh.lock"):
        assert service.refresh(force=True) is False
    assert worker.calls == 1
    service.close()
