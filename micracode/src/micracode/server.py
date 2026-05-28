"""FastAPI application factory for the Micracode web server.

Serves the API and optionally pre-built static frontend assets.
Sets Cross-Origin-Opener-Policy and Cross-Origin-Embedder-Policy headers
required for WebContainer to run in the browser.
"""

from __future__ import annotations

import logging
from collections.abc import AsyncIterator
from contextlib import asynccontextmanager
from pathlib import Path

from fastapi import FastAPI, Request
from fastapi.middleware.cors import CORSMiddleware
from fastapi.responses import FileResponse, JSONResponse
from starlette.middleware.base import BaseHTTPMiddleware

from .config import get_settings, settings
from .deps import get_engine
from .routers import generate, health, models, projects

logging.basicConfig(
    level=settings.log_level.upper(),
    format="%(asctime)s %(levelname)s %(name)s: %(message)s",
)
logger = logging.getLogger(settings.app_name)

_STATIC_DIR = Path(__file__).parent / "static"


class _CoopCoepMiddleware(BaseHTTPMiddleware):
    """Add COOP/COEP headers to every response (required by WebContainer)."""

    async def dispatch(self, request: Request, call_next):  # type: ignore[override]
        response = await call_next(request)
        response.headers["Cross-Origin-Opener-Policy"] = "same-origin"
        response.headers["Cross-Origin-Embedder-Policy"] = "require-corp"
        return response


@asynccontextmanager
async def _lifespan(_: FastAPI) -> AsyncIterator[None]:
    get_engine().storage.ensure_root()
    yield


def create_app() -> FastAPI:
    cfg = get_settings()

    app = FastAPI(
        title="Micracode",
        version="0.1.0",
        description="AI-powered web app builder.",
        lifespan=_lifespan,
    )

    app.add_middleware(
        CORSMiddleware,
        allow_origins=cfg.cors_allow_origins,
        allow_credentials=True,
        allow_methods=["GET", "POST", "OPTIONS"],
        allow_headers=["Authorization", "Content-Type", "Accept"],
        expose_headers=["X-Request-ID"],
        max_age=3600,
    )
    app.add_middleware(_CoopCoepMiddleware)

    app.include_router(health.router, prefix="/v1", tags=["health"])
    app.include_router(models.router, prefix="/v1", tags=["models"])
    app.include_router(projects.router, prefix="/v1", tags=["projects"])
    app.include_router(generate.router, prefix="/v1", tags=["generate"])

    # Serve pre-built frontend when static assets are present.
    if _STATIC_DIR.is_dir() and any(_STATIC_DIR.iterdir()):
        _static_root = _STATIC_DIR.resolve()

        @app.get("/{full_path:path}")
        async def _spa(full_path: str) -> FileResponse:
            target = (_static_root / full_path).resolve()
            # Prevent path traversal outside the static directory.
            if not str(target).startswith(str(_static_root)):
                return FileResponse(_static_root / "index.html")
            if target.is_file():
                return FileResponse(target)
            # Support trailingSlash pages (e.g. /projects → projects/index.html).
            index = target / "index.html"
            if index.is_file():
                return FileResponse(index)
            # SPA fallback for any unknown path.
            return FileResponse(_static_root / "index.html")

    @app.exception_handler(Exception)
    async def _unhandled(_: Request, exc: Exception) -> JSONResponse:
        logger.exception("unhandled exception: %s", exc)
        return JSONResponse(status_code=500, content={"detail": "Internal Server Error"})

    logger.info(
        "micracode ready env=%s provider=%s model=%s",
        cfg.environment,
        cfg.llm_provider,
        cfg.active_model,
    )
    return app


app = create_app()
