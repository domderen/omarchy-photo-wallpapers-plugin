import stat
import time

from photo_wallpapers.io import private_dir


def test_secure_directory_does_not_emit_attribute_changes(tmp_path):
    """Repeated storage access must not wake parent-directory desktop watchers."""
    directory = private_dir(tmp_path / "data")
    before = directory.stat().st_ctime_ns
    time.sleep(0.002)
    private_dir(directory)
    assert directory.stat().st_ctime_ns == before


def test_private_directory_repairs_loose_permissions(tmp_path):
    directory = tmp_path / "data"
    directory.mkdir(mode=0o755)
    private_dir(directory)
    assert stat.S_IMODE(directory.stat().st_mode) == 0o700
