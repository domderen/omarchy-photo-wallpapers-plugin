from types import SimpleNamespace as N

from photo_wallpapers.bridge.photos import PhotosAdapter
from tests.bridge.test_photos_adapter import photo


def test_enrichment_exports_master_mapping_and_named_membership_only(tmp_path):
    db = N(
        photos=lambda **kw: [
            photo("a", cloud_guid="cloud-master", cloud_owner_hashed_id=None),
            photo("hidden", hidden=True, cloud_guid="hidden-cloud"),
        ],
        person_info=[N(uuid="p1", name="First")],
        album_info=[],
    )
    snapshot = PhotosAdapter(tmp_path, database_factory=lambda **kw: db).extract(
        tmp_path / "snapshot.sqlite"
    )
    value = snapshot.enrichment
    assert value["schema_version"] == 1
    assert value["cloud_id_type"] == "CPLMaster.recordName"
    assert value["provider_namespace"] == snapshot.namespace
    assert value["assets"] == [
        {
            "local_id": "a",
            "cloud_master_guid": "cloud-master",
            "owner_hash": None,
            "person_ids": ["p1"],
            "keywords": [],
            "collection_ids": [],
        }
    ]
    assert value["people"] == [{"id": "p1", "name": "First"}]
    assert value["capabilities"]["face_bounds"] is False
    assert str(tmp_path) not in str(value)


def test_missing_mapping_stays_unknown(tmp_path):
    db = N(photos=lambda **kw: [photo("a")], person_info=[], album_info=[])
    value = (
        PhotosAdapter(tmp_path, database_factory=lambda **kw: db)
        .extract(tmp_path / "snapshot.sqlite")
        .enrichment
    )
    assert value["assets"][0]["cloud_master_guid"] is None


def test_enrichment_is_separate_from_legacy_catalog_identity(tmp_path):
    from photo_wallpapers.catalog.models import CatalogSnapshot
    from photo_wallpapers.catalog.publish import (
        generation,
        publish_catalog,
        read_catalog,
    )

    snapshot = CatalogSnapshot("synthetic")
    baseline = generation(snapshot)
    snapshot.enrichment = {"schema_version": 1, "created_at": "2026-10-22T12:00:00Z"}
    assert generation(snapshot) == baseline
    manifest = publish_catalog(snapshot, tmp_path)
    assert (
        read_catalog(tmp_path / (manifest["generation"] + ".sqlite")).namespace
        == "synthetic"
    )


def test_local_album_membership_is_exported_as_metadata_without_guessing_cloud_album_ids(
    tmp_path,
):
    album = N(uuid="album", title="Local collection")
    db = N(
        photos=lambda **kw: [photo("a", cloud_guid="cloud-master")],
        person_info=[],
        album_info=[album],
    )
    value = (
        PhotosAdapter(tmp_path, database_factory=lambda **kw: db)
        .extract(tmp_path / "snapshot.sqlite")
        .enrichment
    )
    assert value["collections"] == [
        {"id": "album", "kind": "album", "name": "Local collection", "available": True}
    ]
    assert value["assets"][0]["collection_ids"] == ["album"]


def test_metadata_only_matches_legacy_membership_without_retaining_media(tmp_path):
    shared_photo = photo("a", cloud_guid="master-a", cloud_owner_hashed_id=None)
    shared = N(uuid="shared", title="Shared", photos=[shared_photo])
    db = N(
        photos=lambda **kw: [
            shared_photo,
            photo("hidden", hidden=True),
            photo("deleted", intrash=True),
        ],
        person_info=[N(uuid="p1", name="First"), N(uuid="p2", name="First")],
        album_info=[N(uuid="album", title="Local")],
        album_info_shared=[shared],
    )
    legacy = PhotosAdapter(tmp_path, database_factory=lambda **kw: db).extract(
        tmp_path / "snapshot"
    )
    adapter = PhotosAdapter(tmp_path, database_factory=lambda **kw: db)
    metadata = adapter.extract(tmp_path / "snapshot", metadata_only=True)
    assert metadata.enrichment | {"created_at": "ignored"} == legacy.enrichment | {
        "created_at": "ignored"
    }
    assert metadata.assets == []
    assert adapter._media == {}
    assert adapter.database is None


def test_metadata_only_does_not_read_rendition_or_fingerprint_fields(tmp_path):
    item = photo("a", cloud_guid="master")
    for key in [
        "fingerprint",
        "original_filename",
        "date_modified",
        "date",
        "orientation",
        "favorite",
        "hasadjustments",
    ]:
        delattr(item, key)
    db = N(photos=lambda **kw: [item], person_info=[], album_info=[])
    result = PhotosAdapter(tmp_path, database_factory=lambda **kw: db).extract(
        tmp_path / "snapshot", metadata_only=True
    )
    assert result.enrichment["assets"][0]["cloud_master_guid"] == "master"
