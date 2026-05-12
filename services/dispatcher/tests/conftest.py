"""Shared test fixtures.

Each test gets a fresh SQLite DB in `tmp_path` and a fresh STORAGE_DIR
sibling. Settings + DB + Storage singletons are reset between tests so
nothing leaks across modules.
"""

from __future__ import annotations

import os
import secrets
from pathlib import Path
from typing import Iterator

import pytest
from fastapi.testclient import TestClient

# Set env BEFORE importing anything that reads it.
os.environ.setdefault("JWT_SECRET", secrets.token_urlsafe(32))


@pytest.fixture
def tmp_state(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Iterator[Path]:
    """Point DB_URL + STORAGE_DIR at a fresh tmp_path."""
    db_file = tmp_path / "dispatcher.db"
    storage_dir = tmp_path / "storage"
    monkeypatch.setenv("DB_URL", f"sqlite:///{db_file}")
    monkeypatch.setenv("STORAGE_DIR", str(storage_dir))
    # Short retention so cleanup tests don't have to sleep.
    monkeypatch.setenv("OUTPUT_TTL_HOURS", "168")
    monkeypatch.setenv("FAILED_INPUT_TTL_HOURS", "72")
    monkeypatch.setenv("CLEAN_INTERVAL_SECONDS", "999999")  # never fires under tests
    monkeypatch.setenv("ALLOW_OPEN_REGISTRATION", "true")

    # Reset cached singletons so the new env wins.
    from app import config, db, storage

    config.reset_settings_for_tests()
    db.reset_db_for_tests()
    storage.reset_storage_for_tests()

    db.init_db()
    yield tmp_path

    # Tear down singletons so the next test starts clean even if it
    # forgets to depend on this fixture.
    config.reset_settings_for_tests()
    db.reset_db_for_tests()
    storage.reset_storage_for_tests()


@pytest.fixture
def client(tmp_state: Path) -> Iterator[TestClient]:
    """A TestClient against the production app, but with the lifespan
    short-circuited so we don't spin up the cleaner scheduler in tests
    (we drive the cleaner directly when we want to test it)."""
    from app.main import create_app

    app = create_app()
    # Skip lifespan in tests (no scheduler, no admin seeding).
    with TestClient(app) as tc:
        yield tc


def register(client: TestClient, email: str, password: str = "secret-pass") -> dict:
    resp = client.post(
        "/api/auth/register", json={"email": email, "password": password}
    )
    assert resp.status_code == 201, resp.text
    return resp.json()


def login(client: TestClient, email: str, password: str = "secret-pass") -> str:
    resp = client.post(
        "/api/auth/login", json={"email": email, "password": password}
    )
    assert resp.status_code == 200, resp.text
    return resp.json()["access_token"]


def auth_headers(token: str) -> dict:
    return {"Authorization": f"Bearer {token}"}
