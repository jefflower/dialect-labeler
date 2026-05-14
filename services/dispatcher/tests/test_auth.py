from __future__ import annotations

from fastapi.testclient import TestClient

from .conftest import auth_headers, login, register


def test_first_registrant_becomes_admin(client: TestClient) -> None:
    body = register(client, "founder@example.com")
    assert body["user"]["role"] == "admin"
    assert body["access_token"]


def test_second_registrant_is_plain_user_under_open_registration(client: TestClient) -> None:
    register(client, "founder@example.com")
    body = register(client, "second@example.com")
    assert body["user"]["role"] == "user"


def test_registration_disabled_when_users_exist_and_flag_off(
    monkeypatch, tmp_state, client: TestClient
) -> None:
    register(client, "founder@example.com")
    monkeypatch.setenv("ALLOW_OPEN_REGISTRATION", "false")
    # Force-reload settings so the new env wins.
    from app import config

    config.reset_settings_for_tests()
    resp = client.post(
        "/api/auth/register",
        json={"email": "blocked@example.com", "password": "secret-pass"},
    )
    assert resp.status_code == 403


def test_login_with_bad_password_returns_401(client: TestClient) -> None:
    register(client, "founder@example.com")
    resp = client.post(
        "/api/auth/login",
        json={"email": "founder@example.com", "password": "wrong-password"},
    )
    assert resp.status_code == 401


def test_login_with_unknown_email_returns_401(client: TestClient) -> None:
    resp = client.post(
        "/api/auth/login",
        json={"email": "ghost@example.com", "password": "secret-pass"},
    )
    assert resp.status_code == 401


def test_me_requires_token(client: TestClient) -> None:
    resp = client.get("/api/auth/me")
    assert resp.status_code == 401


def test_me_returns_current_user(client: TestClient) -> None:
    register(client, "founder@example.com")
    token = login(client, "founder@example.com")
    resp = client.get("/api/auth/me", headers=auth_headers(token))
    assert resp.status_code == 200
    body = resp.json()
    assert body["email"] == "founder@example.com"
    assert body["role"] == "admin"


def test_duplicate_email_rejected(client: TestClient) -> None:
    register(client, "founder@example.com")
    resp = client.post(
        "/api/auth/register",
        json={"email": "founder@example.com", "password": "another-pass"},
    )
    assert resp.status_code == 409


def test_login_rate_limit_kicks_in_after_too_many_attempts(client: TestClient) -> None:
    register(client, "victim@example.com")
    # 10 wrong passwords back-to-back exhausts the bucket.
    for _ in range(10):
        resp = client.post(
            "/api/auth/login",
            json={"email": "victim@example.com", "password": "wrong"},
        )
        assert resp.status_code == 401
    # 11th attempt — even with the right password — must be throttled.
    resp = client.post(
        "/api/auth/login",
        json={"email": "victim@example.com", "password": "secret-pass"},
    )
    assert resp.status_code == 429
    assert "Retry-After" in resp.headers


def test_successful_login_clears_rate_limit_bucket(client: TestClient) -> None:
    register(client, "ok@example.com")
    # Fewer than the limit, then a success — bucket should reset so
    # subsequent typos don't immediately trip the limit.
    for _ in range(3):
        client.post(
            "/api/auth/login",
            json={"email": "ok@example.com", "password": "wrong"},
        )
    resp = client.post(
        "/api/auth/login",
        json={"email": "ok@example.com", "password": "secret-pass"},
    )
    assert resp.status_code == 200
    # 8 more wrong attempts (3+8 = 11 total) should still be allowed
    # because the success reset the bucket.
    for _ in range(8):
        resp = client.post(
            "/api/auth/login",
            json={"email": "ok@example.com", "password": "wrong"},
        )
        assert resp.status_code == 401, f"got {resp.status_code} {resp.text}"


def test_authenticated_response_includes_request_id(client: TestClient) -> None:
    register(client, "u@example.com")
    token = login(client, "u@example.com")
    resp = client.get("/api/tasks", headers=auth_headers(token))
    assert resp.status_code == 200
    assert resp.headers.get("X-Request-ID")


def test_old_token_refreshed_via_response_header(
    client: TestClient, monkeypatch
) -> None:
    """An authenticated request whose token is older than
    `jwt_refresh_after_hours` should come back with a fresh `X-Refreshed-Token`
    header that decodes to the same user but a later expiry."""
    from datetime import datetime, timezone

    import jwt as pyjwt

    from app import config
    from app.config import get_settings

    monkeypatch.setenv("JWT_REFRESH_AFTER_HOURS", "0")  # always refresh
    config.reset_settings_for_tests()

    register(client, "u@example.com")
    token = login(client, "u@example.com")
    settings = get_settings()
    payload_before = pyjwt.decode(
        token, settings.jwt_secret, algorithms=[settings.jwt_algorithm]
    )

    resp = client.get("/api/tasks", headers=auth_headers(token))
    assert resp.status_code == 200
    fresh = resp.headers.get("X-Refreshed-Token")
    assert fresh, "expected X-Refreshed-Token on an aged token"
    payload_after = pyjwt.decode(
        fresh, settings.jwt_secret, algorithms=[settings.jwt_algorithm]
    )
    assert payload_after["sub"] == payload_before["sub"]
    assert payload_after["exp"] >= payload_before["exp"]
    assert payload_after["iat"] >= payload_before["iat"]


def test_recent_token_not_refreshed(client: TestClient) -> None:
    """A freshly-minted token does NOT get refreshed on the next request —
    avoids pointless crypto work on every API call."""
    register(client, "u@example.com")
    token = login(client, "u@example.com")
    # Default jwt_refresh_after_hours = 24, default jwt_ttl_hours = 720;
    # the just-issued token is well under the refresh threshold.
    resp = client.get("/api/tasks", headers=auth_headers(token))
    assert resp.status_code == 200
    assert resp.headers.get("X-Refreshed-Token") is None


# ---------------------------------------------------------------------
# Admin-approval gate. Self-registered users after the bootstrap admin
# land in `is_approved=False` and must be flipped by an admin before
# they can log in. These tests use the raw HTTP API rather than the
# legacy `register()` helper because the helper auto-approves.
# ---------------------------------------------------------------------


def test_bootstrap_admin_is_auto_approved(client: TestClient) -> None:
    """First-ever registrant becomes admin AND skips the pending state.
    Otherwise no one could ever log in to do the approving."""
    resp = client.post(
        "/api/auth/register",
        json={"email": "founder@example.com", "password": "secret-pass"},
    )
    assert resp.status_code == 201
    body = resp.json()
    assert body["user"]["role"] == "admin"
    assert body["user"]["is_approved"] is True
    assert "access_token" in body, "bootstrap admin should get a token immediately"


def test_second_registrant_returns_pending_payload(client: TestClient) -> None:
    """Self-registration after bootstrap returns RegisterPendingOut, NOT
    a token. The user must wait for admin approval before logging in."""
    register(client, "founder@example.com")  # bootstrap
    resp = client.post(
        "/api/auth/register",
        json={"email": "newbie@example.com", "password": "secret-pass"},
    )
    assert resp.status_code == 201
    body = resp.json()
    assert body["status"] == "pending_approval"
    assert "access_token" not in body
    assert body["user"]["is_approved"] is False


def test_pending_user_cannot_login(client: TestClient) -> None:
    """Login endpoint must distinguish "wrong password" (401) from
    "pending approval" (403) so the SPA can show a useful message."""
    register(client, "founder@example.com")
    client.post(
        "/api/auth/register",
        json={"email": "pending@example.com", "password": "secret-pass"},
    )
    resp = client.post(
        "/api/auth/login",
        json={"email": "pending@example.com", "password": "secret-pass"},
    )
    assert resp.status_code == 403
    assert "审核" in resp.json()["detail"]


def test_admin_can_approve_pending_user(client: TestClient) -> None:
    """Happy path: admin flips is_approved, user can then log in."""
    founder = register(client, "founder@example.com")
    reg_resp = client.post(
        "/api/auth/register",
        json={"email": "candidate@example.com", "password": "secret-pass"},
    )
    user_id = reg_resp.json()["user"]["id"]

    # Pending login → 403
    pre = client.post(
        "/api/auth/login",
        json={"email": "candidate@example.com", "password": "secret-pass"},
    )
    assert pre.status_code == 403

    # Admin approves.
    approve = client.post(
        f"/api/users/{user_id}/approve",
        headers=auth_headers(founder["access_token"]),
    )
    assert approve.status_code == 200
    approved_body = approve.json()
    assert approved_body["is_approved"] is True
    assert approved_body["approved_by_id"] == founder["user"]["id"]
    assert approved_body["approved_at"]

    # Login now succeeds.
    post = client.post(
        "/api/auth/login",
        json={"email": "candidate@example.com", "password": "secret-pass"},
    )
    assert post.status_code == 200
    assert post.json()["access_token"]


def test_approve_is_idempotent(client: TestClient) -> None:
    """Double-approving doesn't error or move the approved_at timestamp."""
    founder = register(client, "founder@example.com")
    reg = client.post(
        "/api/auth/register",
        json={"email": "u@example.com", "password": "secret-pass"},
    )
    user_id = reg.json()["user"]["id"]
    first = client.post(
        f"/api/users/{user_id}/approve",
        headers=auth_headers(founder["access_token"]),
    )
    second = client.post(
        f"/api/users/{user_id}/approve",
        headers=auth_headers(founder["access_token"]),
    )
    assert first.status_code == 200
    assert second.status_code == 200
    assert first.json()["approved_at"] == second.json()["approved_at"]


def test_non_admin_cannot_approve(client: TestClient) -> None:
    """Approval is an admin-only operation."""
    register(client, "founder@example.com")  # bootstrap admin
    # Create a regular user via the legacy helper (auto-approved).
    regular = register(client, "regular@example.com")
    # And a pending user we'll try to approve.
    pending = client.post(
        "/api/auth/register",
        json={"email": "pending@example.com", "password": "secret-pass"},
    ).json()
    resp = client.post(
        f"/api/users/{pending['user']['id']}/approve",
        headers=auth_headers(regular["access_token"]),
    )
    assert resp.status_code == 403


def test_admin_created_user_is_pre_approved(client: TestClient) -> None:
    """When an admin creates an account via /api/users, the approval
    step is implicit — they can log in immediately."""
    founder = register(client, "founder@example.com")
    create = client.post(
        "/api/users",
        json={"email": "by-admin@example.com", "password": "secret-pass", "role": "user"},
        headers=auth_headers(founder["access_token"]),
    )
    assert create.status_code == 201
    body = create.json()
    assert body["is_approved"] is True
    assert body["approved_by_id"] == founder["user"]["id"]
    # And login works without any extra step.
    login_resp = client.post(
        "/api/auth/login",
        json={"email": "by-admin@example.com", "password": "secret-pass"},
    )
    assert login_resp.status_code == 200


def test_list_users_orders_pending_first(client: TestClient) -> None:
    """Admin's user list surfaces pending accounts ahead of approved ones
    so the review queue is at the top of the page."""
    founder = register(client, "founder@example.com")
    register(client, "approved@example.com")  # legacy auto-approve helper
    client.post(
        "/api/auth/register",
        json={"email": "pending@example.com", "password": "secret-pass"},
    )
    rows = client.get(
        "/api/users", headers=auth_headers(founder["access_token"])
    ).json()
    # Order: pending first (is_approved=False), then approved ones.
    statuses = [r["is_approved"] for r in rows]
    assert statuses[0] is False, f"expected pending first, got {statuses}"
    # Total of 3 users (founder + approved + pending).
    assert len(rows) == 3


def test_admin_stats_counts_pending_users(client: TestClient) -> None:
    """Admin dashboard surfaces pending_user_count as a separate metric."""
    founder = register(client, "founder@example.com")
    client.post(
        "/api/auth/register",
        json={"email": "p1@example.com", "password": "secret-pass"},
    )
    client.post(
        "/api/auth/register",
        json={"email": "p2@example.com", "password": "secret-pass"},
    )
    stats = client.get(
        "/api/admin/stats", headers=auth_headers(founder["access_token"])
    ).json()
    assert stats["pending_user_count"] == 2
