import json
import os
import sys
import threading
import time

import pytest

SUCCESS = r"""
import sys,json,pathlib,hashlib
v=json.load(sys.stdin); p=pathlib.Path(v['output_path'])
doc={'schema_version':1,'provider_namespace':hashlib.sha256(str(pathlib.Path(v['library_path']).resolve()).encode()).hexdigest(),'cloud_id_type':'CPLMaster.recordName','created_at':'2026-10-31T12:00:00+00:00','people':[],'collections':[],'assets':[],'capabilities':{'people':True,'face_bounds':False}}
data=json.dumps(doc).encode(); p.write_bytes(data)
summary={k:doc[k] for k in ['schema_version','provider_namespace','created_at']}
summary.update({'assets':0,'people':0,'collections':0,'sha256':hashlib.sha256(data).hexdigest(),'bytes':len(data),'snapshot_seconds':0.0,'extract_seconds':0.01})
pathlib.Path(v['summary_path']).write_text(json.dumps(summary))
"""


def extractor(command=SUCCESS, timeout=5):
    from photo_wallpapers.bridge.extract import MetadataExtractor

    return MetadataExtractor(command=[sys.executable, "-c", command], timeout=timeout)


def test_child_output_is_verified_and_no_large_document_is_returned(tmp_path):
    worker = extractor()
    try:
        result = worker.extract(
            tmp_path / "library", tmp_path / "snapshot", tmp_path / "metadata.json"
        )
        assert result["assets"] == 0 and result["bytes"] > 100
        assert "cloud_id_type" not in result and "capabilities" not in result
        assert (
            json.loads((tmp_path / "metadata.json").read_text())["schema_version"] == 1
        )
        assert not (tmp_path / "metadata.json.summary").exists()
    finally:
        worker.close()


def test_failed_or_corrupt_child_output_is_not_accepted(tmp_path):
    for command in [
        "raise RuntimeError('private path and token')",
        SUCCESS + "\np.write_text('corrupt')",
    ]:
        worker = extractor(command)
        with pytest.raises(RuntimeError, match="metadata_extraction") as error:
            worker.extract(
                tmp_path / "library", tmp_path / "snapshot", tmp_path / "out.json"
            )
        assert "private path and token" not in str(error.value)
        worker.close()


SLEEP = r"""
import sys,json,pathlib,os,time
v=json.load(sys.stdin)
(pathlib.Path(v['output_path']).parent/'child.pid').write_text(str(os.getpid()))
time.sleep(60)
"""


def test_timed_out_child_is_reaped(tmp_path):
    worker = extractor(SLEEP, timeout=0.2)
    with pytest.raises(TimeoutError, match="metadata_extraction_timeout"):
        worker.extract(tmp_path, tmp_path / "snapshot", tmp_path / "out.json")
    pid = int((tmp_path / "child.pid").read_text())
    with pytest.raises(ProcessLookupError):
        os.kill(pid, 0)
    worker.close()


def test_shutdown_cancels_child_and_refuses_new_work(tmp_path):
    worker = extractor(SLEEP)
    errors = []

    def run():
        try:
            worker.extract(tmp_path, tmp_path / "snapshot", tmp_path / "out.json")
        except RuntimeError as error:
            errors.append(str(error))

    thread = threading.Thread(target=run)
    thread.start()
    deadline = time.monotonic() + 3
    while not (tmp_path / "child.pid").exists() and time.monotonic() < deadline:
        time.sleep(0.01)
    assert (tmp_path / "child.pid").exists()
    worker.close()
    thread.join(timeout=3)
    assert not thread.is_alive() and errors
    with pytest.raises(ProcessLookupError):
        os.kill(int((tmp_path / "child.pid").read_text()), 0)
    with pytest.raises(RuntimeError, match="metadata_extractor_closed"):
        worker.extract(tmp_path, tmp_path / "snapshot", tmp_path / "out.json")


def test_signature_probe_is_isolated_and_does_not_import_photos(tmp_path):
    from photo_wallpapers.bridge.extract import MetadataExtractor
    from photo_wallpapers.bridge.metadata import source_signature

    library = tmp_path / "library"
    (library / "database").mkdir(parents=True)
    (library / "database/Photos.sqlite").write_bytes(b"synthetic stat-only probe")
    worker = MetadataExtractor()
    assert worker.signature(library, tmp_path / "state") == source_signature(library)
    assert not list((tmp_path / "state").glob(".probe-*"))
    worker.close()


def test_permission_stalled_probe_times_out_and_reaps_child(tmp_path):
    worker = extractor(SLEEP, timeout=0.1)
    with pytest.raises(TimeoutError):
        worker.signature(tmp_path, tmp_path / "state")
    assert worker.process is None
    assert not list((tmp_path / "state").glob(".probe-*"))
    worker.close()
