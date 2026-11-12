import json
import os
import subprocess
import sys

from photo_wallpapers.bridge.cache import MediaCache


def test_cache_symlink_cannot_read_or_touch_its_target(tmp_path):
    cache = MediaCache(tmp_path / "cache", 1000000)
    cache.put("safe", b"synthetic-cached-image", {})
    path = cache.root / "safe.jpg"
    outside = tmp_path / "unrelated.jpg"
    path.rename(outside)
    os.utime(outside, ns=(1000000000, 1000000000))
    before = outside.stat().st_mtime_ns
    path.symlink_to(outside)
    assert cache.lookup("safe") is None
    assert outside.stat().st_mtime_ns == before


def test_cache_metadata_with_wrong_shape_is_a_recoverable_miss(tmp_path):
    cache = MediaCache(tmp_path / "cache", 1000000)
    cache.put("safe", b"synthetic-cached-image", {})
    (cache.root / "safe.json").write_text(json.dumps(["not-a-metadata-object"]))
    assert cache.lookup("safe") is None


def test_fifo_cache_file_cannot_block_a_lookup(tmp_path):
    cache = MediaCache(tmp_path / "cache", 1000000)
    cache.put("safe", b"synthetic-cached-image", {})
    path = cache.root / "safe.jpg"
    path.unlink()
    os.mkfifo(path)
    # A subprocess timeout kills/reaps a pre-fix blocked FIFO open, preventing
    # an orphan test thread from hanging the test runner itself.
    result = subprocess.run(
        [
            sys.executable,
            "-c",
            "from pathlib import Path; import sys; from photo_wallpapers.bridge.cache import MediaCache; print(MediaCache(Path(sys.argv[1]), 1000000).lookup('safe'))",
            str(cache.root),
        ],
        capture_output=True,
        text=True,
        timeout=2,
        check=False,
    )
    assert result.returncode == 0
    assert result.stdout.strip() == "None"


def test_eviction_ignores_nonregular_paths_and_still_enforces_the_budget(tmp_path):
    cache = MediaCache(tmp_path / "cache", 4)
    cache.put("safe", b"12345678", {})
    broken = cache.root / "broken.jpg"
    broken.symlink_to(tmp_path / "missing-target")
    fifo = cache.root / "fifo.jpg"
    os.mkfifo(fifo)
    cache.evict()
    assert not (cache.root / "safe.jpg").exists()
    assert broken.is_symlink() and fifo.exists()
