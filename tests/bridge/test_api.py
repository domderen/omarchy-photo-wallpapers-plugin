import time

import pytest
from fastapi.testclient import TestClient
from PIL import Image

from photo_wallpapers.bridge.api import create_app
from photo_wallpapers.bridge.auth import ensure_token
from photo_wallpapers.bridge.config import BridgeConfig
from photo_wallpapers.bridge.service import create_service as BridgeService
from photo_wallpapers.catalog.models import Asset, CatalogSnapshot


class RedactedHeaders(dict):
    def __repr__(self):
        return "<authorization headers redacted>"


class Provider:
    def __init__(self, path):
        self.path = path

    def resolve(self, id, rev):
        if id != "a":
            raise KeyError(id)
        if rev != "r":
            raise ValueError("stale")

    def export(self, id, rev, directory):
        time.sleep(0.03)
        return self.path, "original"


@pytest.fixture
def running(tmp_path):
    image = tmp_path / "original.jpg"
    Image.new("RGB", (1800, 1200), "blue").save(image)
    config = BridgeConfig(tmp_path, tmp_path / "state", legacy_media=True)
    service = BridgeService(
        config,
        extractor=lambda: CatalogSnapshot(
            "test", [Asset("a", "a.jpg", "r", 1800, 1200)]
        ),
        provider=Provider(image),
    )
    service.refresh()
    token = ensure_token(config.state_path)
    with TestClient(create_app(service=service)) as client:
        yield client, RedactedHeaders(Authorization="Bearer " + token), service


@pytest.mark.parametrize(
    "url",
    [
        "/v1/status",
        "/v2/enrichment",
        "/v1/manifest",
        "/v1/catalogs/abc.sqlite",
        "/v1/assets/a/thumbnail",
        "/v1/assets/a/image?revision=r&max_edge=1024",
        "/docs",
        "/other",
    ],
)
def test_all_routes_require_auth(running, url):
    client, _headers, _service = running
    assert client.get(url).status_code == 401
    assert client.get(url, headers={"Authorization": "Bearer wrong"}).status_code == 401


def test_token_is_redacted(running, caplog):
    client, headers, _ = running
    assert client.get("/v1/status", headers=headers).status_code == 200
    assert headers["Authorization"][7:] not in caplog.text


def test_catalog_etag(running):
    client, headers, _ = running
    manifest = client.get("/v1/manifest", headers=headers).json()
    result = client.get(manifest["catalog_url"], headers=headers)
    assert len(result.content) == manifest["bytes"]
    assert (
        client.get(
            manifest["catalog_url"],
            headers=headers | {"If-None-Match": result.headers["etag"]},
        ).status_code
        == 304
    )
    etag = client.get("/v1/manifest", headers=headers).headers["etag"]
    assert (
        client.get(
            "/v1/manifest", headers=headers | {"If-None-Match": etag}
        ).status_code
        == 304
    )


def test_legacy_catalog_route_rejects_symlinks_without_streaming_target(
    running, tmp_path
):
    client, headers, service = running
    # Keep the fixture's background refresh from replacing the injected path
    # while the HTTP request exercises the read boundary.
    with service.refresh_lock:
        manifest = client.get("/v1/manifest", headers=headers).json()
        path = service.catalogs / (manifest["generation"] + ".sqlite")
        path.unlink()
        unrelated = tmp_path / "unrelated-secret"
        unrelated.write_bytes(b"synthetic-private-file")
        path.symlink_to(unrelated)
        response = client.get(manifest["catalog_url"], headers=headers)
    assert b"synthetic-private-file" not in response.content
    assert response.status_code == 404


def test_pending_image_returns_202(running):
    client, headers, _ = running
    url = "/v1/assets/a/image?revision=r&max_edge=1024"
    assert client.get(url, headers=headers).status_code == 202
    deadline = time.monotonic() + 5
    while True:
        result = client.get(url, headers=headers)
        if result.status_code != 202 or time.monotonic() >= deadline:
            break
        time.sleep(0.01)
    assert result.status_code == 200 and result.headers["content-type"] == "image/jpeg"


@pytest.mark.parametrize(
    "url,status",
    [
        ("/v1/assets/unknown/image?revision=r", 404),
        ("/v1/assets/a/image?revision=old", 409),
        ("/v1/assets/a/image?revision=r&max_edge=99999", 422),
        ("/v1/catalogs/../../etc/passwd", 404),
    ],
)
def test_unknown_asset_returns_404_and_stale_revision_returns_409(running, url, status):
    assert running[0].get(url, headers=running[1]).status_code == status


def test_ready_response_protects_media_before_stream_starts(running):
    import asyncio

    client, _headers, service = running
    result = service.worker.request("a", "r", 1024)
    time.sleep(0.2)
    result = service.worker.request("a", "r", 1024)
    endpoint = next(
        r.endpoint for r in client.app.routes if r.path == "/v1/assets/{asset_id}/image"
    )
    response = endpoint("a", "r", 1024)
    assert service.cache.active.get(result["key"], 0) > 0
    service.cache.max_bytes = 0
    service.cache.evict()

    async def consume():
        data = b""
        async for part in response.body_iterator:
            data += part
        return data

    assert len(asyncio.run(consume())) == result["bytes"]
    assert service.cache.active.get(result["key"], 0) == 0


def test_non_ascii_invalid_token_is_unauthorized(running):
    client, _, _ = running
    assert (
        client.get("/v1/status", headers={b"Authorization": b"Bearer \xff"}).status_code
        == 401
    )


def test_enrichment_endpoint_is_private_and_separate(running):
    from photo_wallpapers.io import atomic_json

    client, headers, service = running
    assert client.get("/v2/enrichment", headers=headers).status_code == 503
    value = {"schema_version": 1, "provider_namespace": "synthetic", "assets": []}
    atomic_json(service.config.state_path / "enrichment-v1.json", value)
    assert client.get("/v2/enrichment", headers=headers).json() == value
