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


def _phone_counter() -> "Iterator[str]":
    """Yield deterministic 11-digit phone numbers for tests.

    Module-level so it persists across calls within a process — each
    `register(client, "...")` invocation either passes an explicit
    identifier (legacy email-style string accepted as a free-form
    username post-rename) or pulls a fresh phone via `next_phone()`.
    Starting at 13800000001 keeps the numbers obviously fake.
    """
    n = 1
    while True:
        yield f"138{n:08d}"
        n += 1


_phone_iter = _phone_counter()


def next_phone() -> str:
    """Allocate a fresh fake phone number for a test."""
    return next(_phone_iter)


def register(
    client: TestClient,
    identifier: str | None = None,
    password: str = "secret-pass",
) -> dict:
    """Register + auto-approve for legacy test compatibility.

    The production register endpoint now drops 2nd+ registrants into a
    pending state (admin must approve via /api/users/{id}/approve)
    AND only accepts 11-digit Chinese mobile numbers. Most existing
    tests pre-date both changes and don't care — they just want a
    logged-in user. So this helper:

      1. If `identifier` looks like an 11-digit mobile, posts
         /register normally. Otherwise (email-shaped or username
         "admin" etc.), generates a phone number, registers with that,
         then DB-renames the identifier — this lets old tests keep
         their pretty `founder@example.com` strings as the visible
         account label without having to teach every test the new
         mobile-only path.
      2. Auto-approves via direct DB write if the registration landed
         in the pending state.
      3. Returns a token-bearing dict, normalised to the legacy shape
         (with `user.identifier` instead of `user.email`).

    Tests that specifically exercise the registration / approval flow
    should NOT use this helper — call the endpoints directly with
    `client.post(...)`.
    """
    # Resolve which phone number to use for the actual register call.
    actual_phone = (
        identifier if identifier and identifier.isdigit() and len(identifier) == 11
        else next_phone()
    )
    resp = client.post(
        "/api/auth/register",
        json={"phone": actual_phone, "password": password},
    )
    assert resp.status_code == 201, resp.text
    body = resp.json()

    # If caller passed a non-phone identifier, rename the DB row so
    # legacy tests that match against "founder@example.com" still
    # work. Bypass the API — this is fixture-grade, not a public path.
    from app.db import session_scope
    from app.models import User, utcnow

    user_id = body["user"]["id"]
    needs_rename = identifier and identifier != actual_phone
    is_pending = "access_token" not in body

    if needs_rename or is_pending:
        with session_scope() as db:
            u = db.get(User, user_id)
            assert u is not None
            if needs_rename:
                u.identifier = identifier
            if is_pending:
                u.is_approved = True
                u.approved_at = utcnow()
        # Refresh body so callers see the renamed/approved row.
        body["user"]["identifier"] = identifier or actual_phone
        body["user"]["is_approved"] = True

    if is_pending:
        # The auto-approve path didn't get a token. Log in now.
        token = login(client, identifier or actual_phone, password)
        return {
            "access_token": token,
            "token_type": "bearer",
            "user": body["user"],
        }
    return body


def login(
    client: TestClient,
    identifier: str,
    password: str = "secret-pass",
) -> str:
    resp = client.post(
        "/api/auth/login",
        json={"identifier": identifier, "password": password},
    )
    assert resp.status_code == 200, resp.text
    return resp.json()["access_token"]


def auth_headers(token: str) -> dict:
    return {"Authorization": f"Bearer {token}"}
