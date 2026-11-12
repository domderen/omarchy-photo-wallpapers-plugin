"""Explicit legacy media protocol; never loaded by the default metadata helper."""

import re

from fastapi import Query, Request
from fastapi.responses import JSONResponse, Response, StreamingResponse
from starlette.background import BackgroundTask

from ..io import locked, open_private


def register_routes(app, service):
    @app.get("/v1/manifest")
    def manifest(request: Request):
        value = service.manifest()
        if not value:
            return JSONResponse({"error": "catalog_unavailable"}, status_code=503)
        etag = '"' + value["generation"] + '"'
        if request.headers.get("if-none-match") == etag:
            return Response(status_code=304, headers={"ETag": etag})
        return JSONResponse(value, headers={"ETag": etag})

    @app.get("/v1/catalogs/{generation}.sqlite")
    def catalog(generation: str, request: Request):
        if not re.fullmatch("[a-f0-9]{64}", generation):
            return JSONResponse({"error": "not_found"}, status_code=404)
        etag = '"' + generation + '"'
        with locked(service.catalogs / ".publish.lock"):
            try:
                handle = open_private(service.catalogs / (generation + ".sqlite"))
            except (OSError, ValueError):
                return JSONResponse({"error": "not_found"}, status_code=404)
        if request.headers.get("if-none-match") == etag:
            handle.close()
            return Response(status_code=304, headers={"ETag": etag})
        import os

        length = os.fstat(handle.fileno()).st_size

        def chunks():
            try:
                while block := handle.read(256 * 1024):
                    yield block
            finally:
                handle.close()

        return StreamingResponse(
            chunks(),
            media_type="application/vnd.sqlite3",
            headers={"ETag": etag, "Content-Length": str(length)},
        )

    def media(asset_id, revision, max_edge):
        try:
            result = service.worker.request(asset_id, revision, max_edge)
        except KeyError:
            return JSONResponse({"error": "not_found"}, status_code=404)
        except ValueError:
            return JSONResponse({"error": "stale_revision"}, status_code=409)
        if result["state"] == "pending":
            return JSONResponse(
                {"state": "pending"},
                status_code=202,
                headers={"Retry-After": str(result.get("retry_after", 2))},
            )
        if result["state"] == "failed":
            return JSONResponse(
                {"state": "failed", "error": result["error"]},
                status_code=503,
                headers={"Retry-After": "30"},
            )
        context = service.cache.open(result["key"])
        try:
            handle = context.__enter__()
        except FileNotFoundError:
            return JSONResponse(
                {"state": "pending"}, status_code=202, headers={"Retry-After": "2"}
            )
        released = False

        def release():
            nonlocal released
            if not released:
                released = True
                context.__exit__(None, None, None)

        def chunks():
            try:
                while block := handle.read(256 * 1024):
                    yield block
            finally:
                release()

        return StreamingResponse(
            chunks(),
            media_type="image/jpeg",
            headers={
                "ETag": '"' + result["sha256"] + '"',
                "Content-Length": str(result["bytes"]),
                "X-Photo-Rendition": result["rendition"],
                "X-Content-SHA256": result["sha256"],
            },
            background=BackgroundTask(release),
        )

    @app.get("/v1/assets/{asset_id}/image")
    def image(
        asset_id: str, revision: str, max_edge: int = Query(4096, ge=128, le=8192)
    ):
        return media(asset_id, revision, max_edge)

    @app.get("/v1/assets/{asset_id}/thumbnail")
    def thumbnail(asset_id: str):
        value = getattr(service.provider, "_media", {}).get(asset_id)
        if not value:
            return JSONResponse({"error": "not_found"}, status_code=404)
        return media(asset_id, value[0], 512)
