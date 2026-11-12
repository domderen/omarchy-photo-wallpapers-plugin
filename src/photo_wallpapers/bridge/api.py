import asyncio
from contextlib import asynccontextmanager
from pathlib import Path

from fastapi import FastAPI, Request
from fastapi.responses import JSONResponse, Response, StreamingResponse
from starlette.background import BackgroundTask

from ..io import locked, open_private
from .auth import valid_token
from .config import load_config
from .extract import digest_file
from .metadata import valid_cache
from .service import create_service


def create_app(config_path: Path | None = None, *, service=None):
    service = service or create_service(load_config(config_path))

    @asynccontextmanager
    async def lifespan(app):
        task = asyncio.create_task(service.run())
        yield
        task.cancel()
        try:
            await task
        except asyncio.CancelledError:
            pass
        await asyncio.to_thread(service.close)

    app = FastAPI(lifespan=lifespan, docs_url=None, redoc_url=None, openapi_url=None)
    app.state.service = service

    @app.middleware("http")
    async def authentication(request, call_next):
        auth = request.headers.get("authorization", "")
        if not auth.startswith("Bearer ") or not valid_token(
            service.config.state_path, auth[7:]
        ):
            return JSONResponse({"error": "unauthorized"}, status_code=401)
        return await call_next(request)

    @app.get("/v1/status")
    def status():
        return service.status()

    @app.get("/v2/enrichment")
    def enrichment(request: Request):
        handle = None
        try:
            with locked(service.config.state_path / ".enrichment.lock"):
                if not service.config.legacy_media:
                    if service.status().get("refresh_error"):
                        raise ValueError("stale_enrichment")
                    marker = valid_cache(service.config.state_path, service.namespace)
                    if marker is None:
                        raise ValueError("unverified_enrichment")
                    size, digest = (
                        marker["payload"]["bytes"],
                        marker["payload"]["sha256"],
                    )
                else:
                    size, digest = digest_file(
                        service.config.state_path / "enrichment-v1.json"
                    )
                handle = open_private(service.config.state_path / "enrichment-v1.json")
            etag = '"' + digest + '"'
            if request.headers.get("if-none-match") == etag:
                handle.close()
                return Response(status_code=304, headers={"ETag": etag})
        except (OSError, ValueError, KeyError, TypeError):
            if handle:
                handle.close()
            return JSONResponse({"error": "enrichment_unavailable"}, status_code=503)

        def chunks():
            try:
                while block := handle.read(256 * 1024):
                    yield block
            finally:
                handle.close()

        return StreamingResponse(
            chunks(),
            media_type="application/json",
            headers={"ETag": etag, "Content-Length": str(size)},
            background=BackgroundTask(handle.close),
        )

    if service.config.legacy_media:
        from .legacy_api import register_routes

        register_routes(app, service)
    else:

        @app.get("/v1/manifest")
        @app.get("/v1/catalogs/{generation}.sqlite")
        @app.get("/v1/assets/{asset_id}/image")
        @app.get("/v1/assets/{asset_id}/thumbnail")
        def retired():
            return JSONResponse(
                {
                    "error": "legacy_media_disabled",
                    "action": "Use direct iCloud downloads; legacy clients require explicit legacy_media configuration",
                },
                status_code=410,
            )

    return app
