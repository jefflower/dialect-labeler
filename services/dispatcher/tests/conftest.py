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
    """Register + auto-approve for legacy test compatibility.

    The production register endpoint now drops 2nd+ registrants into a
    pending state (admin must approve via /api/users/{id}/approve).
    Most existing tests pre-date this gate and don't care about the
    workflow — they just want a logged-in user. So this helper:
      1. Posts /register (gets TokenOut for bootstrap admin, or
         RegisterPendingOut for everyone else).
      2. If the response is the pending shape, flips `is_approved` on
         the DB row directly so subsequent `login()` calls succeed.
      3. Returns a token-bearing dict, normalised to the legacy shape.

    Tests that specifically exercise the approval flow should NOT use
    this helper — call the endpoints directly with `client.post(...)`.
    """
    resp = client.post(
        "/api/auth/register", json={"email": email, "password": password}
    )
    assert resp.status_code == 201, resp.text
    body = resp.json()
    if "access_token" in body:
        return body
    # Pending shape — auto-approve through the DB so legacy tests
    # can immediately log in. We bypass the API on purpose: this
    # helper is fixture-grade, not a public surface.
    from app.db import session_scope
    from app.models import User, utcnow

    user_id = body["user"]["id"]
    with session_scope() as db:
        u = db.get(User, user_id)
        assert u is not None
        u.is_approved = True
        u.approved_at = utcnow()
    # Now login to get a token the rest of the test can use.
    token = login(client, email, password)
    return {
        "access_token": token,
        "token_type": "bearer",
        "user": body["user"] | {"is_approved": True},
    }


def login(client: TestClient, email: str, password: str = "secret-pass") -> str:
    resp = client.post(
        "/api/auth/login", json={"email": email, "password": password}
    )
    assert resp.status_code == 200, resp.text
    return resp.json()["access_token"]


def auth_headers(token: str) -> dict:
    return {"Authorization": f"Bearer {token}"}
