from photo_wallpapers.bridge.cache import MediaCache


def test_eviction_preserves_active_download(tmp_path):
    cache = MediaCache(tmp_path, 5)
    cache.put("a", b"12345", {})
    with cache.open("a") as file:
        cache.put("b", b"67890", {})
        cache.evict()
        assert file.read() == b"12345"
        assert cache.lookup("a") is not None
    cache.evict()
    assert sum(p.stat().st_size for p in tmp_path.glob("*.jpg")) <= 5


def test_corrupt_cache_is_rejected(tmp_path):
    cache = MediaCache(tmp_path, 100)
    cache.put("a", b"good", {})
    (tmp_path / "a.jpg").write_bytes(b"bad")
    assert cache.lookup("a") is None
