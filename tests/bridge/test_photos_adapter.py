from types import SimpleNamespace as N
from photo_wallpapers.bridge.photos import PhotosAdapter


def photo(id, **changes):
    value = dict(
        uuid=id,
        original_filename="test.heic",
        hidden=False,
        intrash=False,
        isphoto=True,
        width=4000,
        height=3000,
        orientation=1,
        date=None,
        date_modified=None,
        fingerprint="fingerprint",
        favorite=False,
        keywords=[],
        hasadjustments=False,
        person_info=[N(uuid="p1", name="First")],
        album_info=[N(uuid="album", title="旅行", cloud_owner=False, cloud=False)],
    )
    return N(**(value | changes))


def extract(tmp_path, photos):
    db = N(
        photos=lambda **kw: photos,
        person_info=[N(uuid="p1", name="First"), N(uuid="p2", name="First")],
        album_info=[N(uuid="album", title="旅行", cloud=False)],
    )
    return PhotosAdapter(tmp_path, database_factory=lambda **kw: db).extract(
        tmp_path / "Photos.sqlite"
    )


def test_hidden_and_trashed_are_excluded(tmp_path):
    s = extract(
        tmp_path,
        [photo("a"), photo("hidden", hidden=True), photo("deleted", intrash=True)],
    )
    assert {a.id for a in s.assets} == {"a"}


def test_unsupported_shared_sources_are_explicit(tmp_path):
    s = extract(tmp_path, [photo("a")])
    assert s.capabilities["shared_photo_library"] is False
    assert s.capabilities["face_bounds"] is False
    assert s.capabilities["shared_albums"] is True


def test_person_rename_preserves_membership(tmp_path):
    before = extract(tmp_path, [photo("a")])
    after = extract(tmp_path, [photo("a", person_info=[N(uuid="p1", name="Renamed")])])
    assert before.photo_people == after.photo_people == [("a", "p1")]
    assert len({p.id for p in before.people}) == 2


def test_unnamed_people_are_not_offered_as_named_sources(tmp_path):
    db = N(
        photos=lambda **kw: [
            photo("a", person_info=[N(uuid="anon", name="_UNKNOWN_")])
        ],
        person_info=[N(uuid="anon", name="_UNKNOWN_")],
        album_info=[],
    )
    result = PhotosAdapter(tmp_path, database_factory=lambda **kw: db).extract(
        tmp_path / "Photos.sqlite"
    )
    assert result.people == [] and result.photo_people == []
