import os
import stat

import pytest

from photo_wallpapers.bridge.extract import digest_file, read_small_json
from photo_wallpapers.bridge.snapshot import capture_database
from photo_wallpapers.io import atomic_json, locked, private_dir, read_json


def test_private_directory_never_chmods_symlink_target(tmp_path):
    target = tmp_path / "other"
    target.mkdir(mode=0o755)
    link = tmp_path / "state"
    link.symlink_to(target)
    with pytest.raises((OSError, ValueError)):
        private_dir(link)
    assert stat.S_IMODE(target.stat().st_mode) == 0o755


def test_private_storage_rejects_linked_ancestor(tmp_path):
    outside = tmp_path / "outside"
    outside.mkdir(mode=0o755)
    (outside / "data.json").write_text("{}")
    link = tmp_path / "linked"
    link.symlink_to(outside)
    with pytest.raises((OSError, ValueError)):
        private_dir(link / "new-state")
    assert not (outside / "new-state").exists()
    with pytest.raises((OSError, ValueError)):
        read_json(link / "data.json")


def test_private_storage_rejects_replaceable_ancestor(tmp_path):
    public = tmp_path / "public"
    public.mkdir()
    public.chmod(0o777)
    with pytest.raises((OSError, ValueError)):
        private_dir(public / "state")
    assert not (public / "state").exists()


@pytest.mark.parametrize("kind", ["symlink", "fifo"])
@pytest.mark.parametrize("operation", ["read", "lock", "write", "digest", "small"])
def test_private_files_reject_links_and_nonregular(tmp_path, kind, operation):
    target = tmp_path / "other.json"
    target.write_text('{"keep": true}')
    path = tmp_path / "unsafe.json"
    path.symlink_to(target) if kind == "symlink" else os.mkfifo(path)
    with pytest.raises((OSError, ValueError)):
        if operation == "lock":
            with locked(path):
                pass
        else:
            {
                "read": read_json,
                "write": lambda p: atomic_json(p, {}),
                "digest": digest_file,
                "small": read_small_json,
            }[operation](path)
    assert target.read_text() == '{"keep": true}'


def test_small_state_is_bounded(tmp_path):
    path = tmp_path / "state.json"
    path.write_bytes(b" " * 65537)
    with pytest.raises(ValueError):
        read_small_json(path)


def test_foreign_owner_fails_closed(tmp_path, monkeypatch):
    path = tmp_path / "state.json"
    path.write_text("{}")
    monkeypatch.setattr(os, "getuid", lambda: path.stat().st_uid + 1)
    with pytest.raises((OSError, ValueError)):
        read_json(path)


def test_snapshot_rejects_source_link_and_preserves_target(tmp_path):
    source = tmp_path / "source"
    source.write_text("private original")
    link = tmp_path / "link"
    link.symlink_to(source)
    with pytest.raises((OSError, ValueError)):
        capture_database(link, tmp_path / "copy")
    assert source.read_text() == "private original"


def test_auth_rejects_corrupt_or_unsafe_credentials(tmp_path):
    from photo_wallpapers.bridge.auth import valid_token

    path = tmp_path / "credentials.json"
    for value in ["[]", "{", '{"token": 123}']:
        path.write_text(value)
        assert valid_token(tmp_path, "supplied") is False
    path.unlink()
    path.symlink_to(tmp_path / "other")
    assert valid_token(tmp_path, "supplied") is False
