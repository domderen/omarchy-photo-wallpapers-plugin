import hashlib

import pytest
from fastapi.testclient import TestClient

from photo_wallpapers.bridge.api import create_app
from photo_wallpapers.bridge.auth import ensure_token
from photo_wallpapers.bridge.config import BridgeConfig
from photo_wallpapers.bridge.service import create_service

from .test_metadata_service import FakeExtractor
from .test_metadata_service import library as library  # noqa: PLC0414 - imported pytest fixture


@pytest.fixture
def api(tmp_path, library):
    config = BridgeConfig(library[0], tmp_path / "state")
    service = create_service(config, extractor=FakeExtractor())
    assert service.refresh()
    client = TestClient(create_app(service=service))
    client.headers["Authorization"] = "Bearer " + ensure_token(config.state_path)
    yield client, service
    service.close()


def test_metadata_stream_etag_and_default_retirement(api):
    client, service = api
    response = client.get("/v2/enrichment")
    assert response.status_code == 200
    assert (
        response.content
        == (service.config.state_path / "enrichment-v1.json").read_bytes()
    )
    assert (
        response.headers["etag"]
        == '"' + hashlib.sha256(response.content).hexdigest() + '"'
    )
    assert (
        client.get(
            "/v2/enrichment", headers={"If-None-Match": response.headers["etag"]}
        ).status_code
        == 304
    )
    for path in [
        "/v1/manifest",
        "/v1/catalogs/abc.sqlite",
        "/v1/assets/abc/image",
        "/v1/assets/abc/thumbnail",
    ]:
        assert client.get(path).status_code == 410
        assert (
            client.get(path, headers={"Authorization": "Bearer invalid"}).status_code
            == 401
        )


@pytest.mark.parametrize("failure", ["corrupt", "missing_marker", "stale"])
def test_unavailable_metadata_fails_closed_preserving_good_overlay(api, failure):
    client, service = api
    state = service.config.state_path
    if failure == "corrupt":
        (state / "enrichment-v1.json").write_text("{}")
    elif failure == "missing_marker":
        (state / "metadata-state.json").unlink()
    else:
        from photo_wallpapers.io import atomic_json

        atomic_json(state / "refresh.json", {"refresh_error": "PermissionError"})
    assert client.get("/v2/enrichment").status_code == 503


def test_inflight_stream_keeps_its_snapshot_during_refresh(api):
    import asyncio

    from starlette.requests import Request

    client, service = api
    endpoint = next(
        route.endpoint for route in client.app.routes if route.path == "/v2/enrichment"
    )
    previous = (service.config.state_path / "enrichment-v1.json").read_bytes()
    response = endpoint(Request({"type": "http", "headers": []}))
    assert service.refresh(force=True)

    async def consume():
        return b"".join([chunk async for chunk in response.body_iterator])

    assert asyncio.run(consume()) == previous
