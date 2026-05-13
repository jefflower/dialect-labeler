"""SQLAlchemy engine + session factory.

SQLite-specific tweaks:
  - `check_same_thread=False` so FastAPI's threadpool can share connections
  - WAL journal mode for non-blocking concurrent reads while a Worker
    holds the writer lock during heartbeat / completion
  - `foreign_keys=ON` because SQLite defaults to OFF (still!)
"""

from __future__ import annotations

from contextlib import contextmanager
from pathlib import Path
from typing import Iterator

from sqlalchemy import create_engine, event
from sqlalchemy.engine import Engine
from sqlalchemy.orm import DeclarativeBase, Session, sessionmaker

from .config import get_settings


class Base(DeclarativeBase):
    pass


_engine: Engine | None = None
_SessionLocal: sessionmaker[Session] | None = None


def _enable_sqlite_pragmas(engine: Engine) -> None:
    @event.listens_for(engine, "connect")
    def _on_connect(dbapi_conn, _record):  # type: ignore[no-untyped-def]
        cursor = dbapi_conn.cursor()
        cursor.execute("PRAGMA journal_mode=WAL")
        cursor.execute("PRAGMA foreign_keys=ON")
        cursor.execute("PRAGMA busy_timeout=5000")
        cursor.close()


def _build_engine() -> Engine:
    settings = get_settings()
    url = settings.db_url
    connect_args: dict = {}
    if url.startswith("sqlite"):
        connect_args["check_same_thread"] = False
        # Make sure the SQLite parent dir exists; SQLAlchemy won't mkdir.
        path = url.split("sqlite:///", 1)[-1]
        if path and path not in (":memory:",):
            Path(path).parent.mkdir(parents=True, exist_ok=True)
    engine = create_engine(url, connect_args=connect_args, future=True)
    if url.startswith("sqlite"):
        _enable_sqlite_pragmas(engine)
    return engine


def get_engine() -> Engine:
    global _engine
    if _engine is None:
        _engine = _build_engine()
    return _engine


def get_session_factory() -> sessionmaker[Session]:
    global _SessionLocal
    if _SessionLocal is None:
        _SessionLocal = sessionmaker(
            bind=get_engine(), autoflush=False, expire_on_commit=False
        )
    return _SessionLocal


def reset_db_for_tests() -> None:
    """Drop cached engine + session factory so tests can swap DB_URL."""
    global _engine, _SessionLocal
    if _engine is not None:
        _engine.dispose()
    _engine = None
    _SessionLocal = None


def init_db() -> None:
    """Create all tables. Idempotent.

    Also runs lightweight in-place column adds for SQLite — `create_all`
    won't `ALTER TABLE` an existing table to add a new column, so any
    column added after the initial release needs a `PRAGMA table_info`
    check + `ALTER TABLE … ADD COLUMN`. Kept inline (vs. Alembic) because
    schema drift here is rare and the box is single-host.
    """
    from . import models  # noqa: F401 — register mappers

    engine = get_engine()
    Base.metadata.create_all(bind=engine)
    if engine.url.get_backend_name() == "sqlite":
        _apply_sqlite_inline_migrations(engine)


def _apply_sqlite_inline_migrations(engine: Engine) -> None:
    """Idempotent ALTER TABLE migrations for SQLite."""
    from sqlalchemy import text

    with engine.begin() as conn:
        rows = conn.execute(text("PRAGMA table_info(tasks)")).fetchall()
        columns = {row[1] for row in rows}
        if "attempts" not in columns:
            conn.execute(
                text("ALTER TABLE tasks ADD COLUMN attempts INTEGER NOT NULL DEFAULT 0")
            )
        # Mode committed at upload time. Default to `dialect` so existing
        # rows keep their original behaviour.
        if "mode" not in columns:
            conn.execute(
                text(
                    "ALTER TABLE tasks ADD COLUMN mode VARCHAR(16) NOT NULL DEFAULT 'dialect'"
                )
            )
        # Worker-pushed progress snapshot. All four are nullable — a
        # task that hasn't started yet (or one created before this
        # migration) simply has progress=NULL across the board.
        if "progress_percent" not in columns:
            conn.execute(text("ALTER TABLE tasks ADD COLUMN progress_percent INTEGER"))
        if "progress_stage" not in columns:
            conn.execute(text("ALTER TABLE tasks ADD COLUMN progress_stage VARCHAR(64)"))
        if "progress_detail" not in columns:
            conn.execute(
                text("ALTER TABLE tasks ADD COLUMN progress_detail VARCHAR(255)")
            )
        if "progress_updated_at" not in columns:
            conn.execute(
                text("ALTER TABLE tasks ADD COLUMN progress_updated_at DATETIME")
            )


def get_db() -> Iterator[Session]:
    """FastAPI dependency: yields a Session and closes it after the request."""
    session = get_session_factory()()
    try:
        yield session
    finally:
        session.close()


@contextmanager
def session_scope() -> Iterator[Session]:
    """Synchronous context manager for background jobs (scheduler etc.)."""
    session = get_session_factory()()
    try:
        yield session
        session.commit()
    except Exception:
        session.rollback()
        raise
    finally:
        session.close()
