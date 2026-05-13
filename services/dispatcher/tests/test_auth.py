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
