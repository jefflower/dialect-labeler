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
