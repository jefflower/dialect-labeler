"""FastAPI entrypoint.

Wires up routes, builds DB tables on startup, optionally seeds an admin
from env, kicks off the file-cleanup scheduler, and (in production) mounts
the built React SPA at `/` so the dispatcher serves the whole stack from
a single port.
"""

from __future__ import annotations

import logging
from contextlib import asynccontextmanager
from pathlib import Path

from fastapi import FastAPI
from sqlalchemy import select
from sqlalchemy.orm import Session

from .auth import hash_password
from .cleaner import schedule_cleaner
from .config import get_settings
from .db import get_session_factory, init_db
from .models import ROLE_ADMIN, User
from .routes import auth as auth_routes
from .routes import tasks as tasks_routes
from .routes import worker as worker_routes

logging.basicConfig(
    level=logging.INFO,
    format="%(asctime)s %(levelname)s %(name)s %(message)s",
)
log = logging.getLogger("dispatcher")


def _seed_admin(db: Session) -> None:
    """Create the bootstrap admin if env vars are set and users table is empty."""
    settings = get_settings()
    if not settings.admin_email or not settings.admin_password:
        return
    if db.execute(select(User.id).limit(1)).scalar_one_or_none() is not None:
        return
    admin = User(
        email=settings.admin_email,
        password_hash=hash_password(settings.admin_password),
        role=ROLE_ADMIN,
    )
    db.add(admin)
    db.commit()
    log.info("seeded bootstrap admin: %s", settings.admin_email)


@asynccontextmanager
async def lifespan(app: FastAPI):
    init_db()
    with get_session_factory()() as db:
        _seed_admin(db)
    scheduler = schedule_cleaner()
    try:
        yield
    finally:
        scheduler.shutdown(wait=False)


def create_app() -> FastAPI:
    app = FastAPI(
        title="dialect-labeler dispatcher",
        version="0.1.0",
        lifespan=lifespan,
    )
    app.include_router(auth_routes.router)
    app.include_router(tasks_routes.router)
    app.include_router(worker_routes.router)

    @app.get("/healthz")
    def healthz() -> dict:
        return {"ok": True}

    # Serve the built React SPA if it's there. The web/dist dir is
    # populated by `npm run build` in services/dispatcher/web — absent
    # in the test container, present in production images. We use a
    # catch-all instead of StaticFiles(html=True) so deep links like
    # /tasks/<id> fall back to index.html (the SPA handles them client-side)
    # while real assets under /assets/* are served as-is.
    web_root = (Path(__file__).parent.parent / "web" / "dist").resolve()
    if web_root.is_dir():
        from fastapi.responses import FileResponse

        index_html = web_root / "index.html"

        @app.get("/{full_path:path}", include_in_schema=False)
        def serve_spa(full_path: str) -> FileResponse:
            candidate = (web_root / full_path).resolve()
            try:
                candidate.relative_to(web_root)
            except ValueError:
                return FileResponse(index_html)
            if full_path and candidate.is_file():
                return FileResponse(candidate)
            return FileResponse(index_html)

    return app


app = create_app()
