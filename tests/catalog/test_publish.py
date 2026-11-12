from dataclasses import replace
import hashlib
import sqlite3
import pytest
from photo_wallpapers.catalog.models import Asset, Person, Collection, CatalogSnapshot
from photo_wallpapers.catalog.publish import publish_catalog, read_catalog


def snapshot():
    return CatalogSnapshot(
        namespace="library-test",
        assets=[
            Asset("a", "one.jpg", "r1", 2400, 1600),
            Asset("b", "two.jpg", "r2", 1600, 2400),
        ],
        people=[Person("p1", "Zoë ' ; $(false)"), Person("p2", "Zoë ' ; $(false)")],
        collections=[Collection("c1", "album", "旅行")],
        photo_people=[("a", "p1"), ("b", "p2")],
        photo_albums=[("a", "c1")],
    )


def test_same_content_retains_generation(tmp_path):
    s = snapshot()
    first = publish_catalog(s, tmp_path)
    data = (tmp_path / (first["generation"] + ".sqlite")).read_bytes()
    second = publish_catalog(replace(s, assets=list(reversed(s.assets))), tmp_path)
    assert first == second
    assert hashlib.sha256(data).hexdigest() == first["sha256"]
    assert read_catalog(tmp_path / (first["generation"] + ".sqlite")) == s


def test_duplicate_names_keep_distinct_ids(tmp_path):
    m = publish_catalog(snapshot(), tmp_path)
    s = read_catalog(tmp_path / (m["generation"] + ".sqlite"))
    ids = [p.id for p in s.people]
    assert len(ids) == len(set(ids)) == 2


def test_failed_publish_preserves_previous(tmp_path, monkeypatch):
    import photo_wallpapers.catalog.publish as mod

    first = publish_catalog(snapshot(), tmp_path)
    previous = (tmp_path / "manifest.json").read_bytes()

    def fail(*args):
        raise OSError("injected failure")

    monkeypatch.setattr(mod, "write_catalog", fail)
    with pytest.raises(OSError):
        publish_catalog(replace(snapshot(), namespace="different"), tmp_path)
    assert (tmp_path / "manifest.json").read_bytes() == previous
    assert read_catalog(tmp_path / (first["generation"] + ".sqlite")) == snapshot()


@pytest.mark.parametrize(
    "change",
    [
        {"photo_people": [("missing", "p1")]},
        {"assets": [Asset("a", "x", "r", 0, 1)]},
        {"people": [Person("p", "a"), Person("p", "b")]},
    ],
)
def test_invalid_relationships_rejected(tmp_path, change):
    with pytest.raises((ValueError, sqlite3.IntegrityError)):
        publish_catalog(replace(snapshot(), **change), tmp_path)
    assert not (tmp_path / "manifest.json").exists()


def test_corruption_rejected(tmp_path):
    path = tmp_path / "bad.sqlite"
    path.write_bytes(b"not a database")
    with pytest.raises((ValueError, sqlite3.DatabaseError)):
        read_catalog(path)


def test_publish_repairs_corrupt_stored_generation(tmp_path):
    first = publish_catalog(snapshot(), tmp_path)
    path = tmp_path / (first["generation"] + ".sqlite")
    path.write_bytes(b"corrupt storage")
    repaired = publish_catalog(snapshot(), tmp_path)
    assert repaired["generation"] == first["generation"]
    assert read_catalog(path) == snapshot()
