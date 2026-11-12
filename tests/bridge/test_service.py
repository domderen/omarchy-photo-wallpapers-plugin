from photo_wallpapers.bridge.config import BridgeConfig
from photo_wallpapers.bridge.service import create_service as BridgeService
from photo_wallpapers.catalog.models import CatalogSnapshot


def test_failed_refresh_keeps_good_generation(tmp_path):
    service = BridgeService(
        BridgeConfig(tmp_path, tmp_path / "state", legacy_media=True),
        extractor=lambda: CatalogSnapshot("test"),
        provider=object(),
    )
    service.refresh()
    before = service.manifest()

    def fail():
        raise PermissionError("private path and token must not be logged")

    service.extractor = fail
    assert service.refresh() is False
    assert service.manifest() == before
    assert service.status()["refresh_error"] == "PermissionError"
    service.close()
