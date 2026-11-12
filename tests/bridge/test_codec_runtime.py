import sys
from types import SimpleNamespace

import pytest

from photo_wallpapers.bridge.media import register_safe_heif


@pytest.mark.parametrize("version", ["1.23.6", "1.23.5", "1.21.0", "", "unknown"])
def test_legacy_heif_rejects_unsafe_native_runtime(monkeypatch, version):
    called = []
    monkeypatch.setitem(
        sys.modules,
        "pillow_heif",
        SimpleNamespace(
            libheif_info=lambda: {"libheif": version},
            register_heif_opener=lambda: called.append(True),
        ),
    )
    with pytest.raises(RuntimeError, match="unsafe_heif_runtime"):
        register_safe_heif()
    assert not called


def test_legacy_heif_accepts_security_release(monkeypatch):
    called = []
    monkeypatch.setitem(
        sys.modules,
        "pillow_heif",
        SimpleNamespace(
            libheif_info=lambda: {"libheif": "1.23.7"},
            register_heif_opener=lambda: called.append(True),
        ),
    )
    register_safe_heif()
    assert called == [True]
