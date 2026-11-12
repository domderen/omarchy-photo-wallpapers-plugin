import time
from concurrent.futures import ThreadPoolExecutor

import pytest
from PIL import Image

from photo_wallpapers.bridge.cache import MediaCache
from photo_wallpapers.bridge.media import MediaWorker


class Provider:
    def __init__(self, image):
        self.image = image
        self.exports = 0
        self.inflight = 0
        self.peak = 0
        self.fail = False

    def resolve(self, id, revision):
        if id not in ("a", "b"):
            raise KeyError(id)
        if revision != "r":
            raise ValueError("stale revision")

    def export(self, id, revision, directory):
        self.exports += 1
        self.inflight += 1
        self.peak = max(self.peak, self.inflight)
        time.sleep(0.03)
        self.inflight -= 1
        if self.fail:
            raise FileNotFoundError("missing original")
        return self.image, "original"


def ready(worker, id="a"):
    for _ in range(100):
        result = worker.request(id, "r", 1024)
        if result["state"] != "pending":
            return result
        time.sleep(0.01)
    raise AssertionError("worker stalled")


def test_duplicate_requests_share_job(tmp_path):
    path = tmp_path / "source.jpg"
    Image.new("RGB", (1800, 1200), "red").save(path)
    provider = Provider(path)
    with MediaWorker(provider, MediaCache(tmp_path / "cache", 1000000)) as worker:
        with ThreadPoolExecutor(8) as pool:
            list(pool.map(lambda _: worker.request("a", "r", 1024), range(16)))
        assert ready(worker)["state"] == "ready"
        assert provider.exports == 1


def test_preparation_concurrency_is_one(tmp_path):
    path = tmp_path / "source.jpg"
    Image.new("RGB", (600, 400)).save(path)
    provider = Provider(path)
    with MediaWorker(provider, MediaCache(tmp_path / "cache", 1000000)) as worker:
        worker.request("a", "r", 1024)
        worker.request("b", "r", 1024)
        assert (
            ready(worker, "a")["state"] == "ready"
            and ready(worker, "b")["state"] == "ready"
        )
        assert provider.peak == 1


def test_missing_original_is_recoverable(tmp_path):
    path = tmp_path / "source.jpg"
    Image.new("RGB", (600, 400)).save(path)
    provider = Provider(path)
    provider.fail = True
    with MediaWorker(
        provider, MediaCache(tmp_path / "cache", 1000000), retry_seconds=0
    ) as worker:
        worker.request("a", "r", 1024)
        # Observe the first failure before a retry_seconds=0 request can enqueue
        # its replacement. A fixed sleep races a delayed CI worker thread.
        worker.executor.submit(lambda: None).result(timeout=5)
        assert worker.jobs[next(iter(worker.jobs))]["state"] == "failed"
        provider.fail = False
        assert ready(worker)["state"] == "ready"


def test_exported_symlink_is_rejected_before_image_decode(tmp_path):
    target = tmp_path / "original.png"
    Image.new("RGB", (600, 400), "red").save(target)
    link = tmp_path / "export.png"
    link.symlink_to(target)
    before = target.read_bytes()
    with MediaWorker(Provider(link), MediaCache(tmp_path / "cache", 1000000)) as worker:
        assert ready(worker)["state"] == "failed"
    assert target.read_bytes() == before


def test_oversized_source_is_rejected_even_when_its_small_jpeg_payload_is_valid(
    tmp_path,
):
    path = tmp_path / "large.jpg"
    Image.new("RGB", (8, 8), "red").save(path)
    with path.open("r+b") as handle:
        handle.truncate(150 * 1024**2 + 1)  # sparse tail; no large allocation
    with MediaWorker(Provider(path), MediaCache(tmp_path / "cache", 1000000)) as worker:
        assert ready(worker)["state"] == "failed"


def test_unexpected_eps_content_never_reaches_an_external_renderer(
    tmp_path, monkeypatch
):
    from PIL import EpsImagePlugin

    path = tmp_path / "source.jpg"  # Check contents, not the claimed extension.
    path.write_bytes(
        b"%!PS-Adobe-3.0 EPSF-3.0\n%%BoundingBox: 0 0 8 8\n"
        b"%%Pages: 1\n%%EndComments\n0 0 moveto\nshowpage\n%%EOF\n"
    )
    calls = []

    def renderer(*args, **kwargs):
        calls.append(True)
        raise RuntimeError("synthetic renderer sentinel; no subprocess runs")

    monkeypatch.setattr(EpsImagePlugin, "Ghostscript", renderer)
    with MediaWorker(Provider(path), MediaCache(tmp_path / "cache", 1000000)) as worker:
        assert ready(worker)["state"] == "failed"
    assert calls == []


def test_revision_change_rejects_stale_bounds(tmp_path):
    with MediaWorker(Provider(None), MediaCache(tmp_path / "cache", 1000000)) as worker:
        with pytest.raises(ValueError):
            worker.request("a", "old", 1024)
        with pytest.raises(KeyError):
            worker.request("../../etc/passwd", "r", 1024)
        with pytest.raises(ValueError):
            worker.request("a", "r", 99999)


def test_rotated_copy_has_correct_dimensions(tmp_path):
    path = tmp_path / "source.jpg"
    im = Image.new("RGB", (300, 500))
    exif = Image.Exif()
    exif[274] = 6
    im.save(path, exif=exif)
    with MediaWorker(Provider(path), MediaCache(tmp_path / "cache", 1000000)) as worker:
        result = ready(worker)
        with Image.open(result["path"]) as image:
            assert image.size == (500, 300) and not image.getexif()


def test_shutdown_cancels_queued_jobs(tmp_path):
    path = tmp_path / "source.jpg"
    Image.new("RGB", (600, 400)).save(path)
    provider = Provider(path)
    worker = MediaWorker(provider, MediaCache(tmp_path / "cache", 1000000))
    worker.request("a", "r", 1024)
    worker.request("b", "r", 1024)
    worker.close()
    assert provider.exports <= 1
    with pytest.raises(RuntimeError):
        worker.request("a", "r", 1024)
