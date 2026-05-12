from __future__ import annotations

from fastapi.testclient import TestClient

from .conftest import auth_headers, login, register


def _bootstrap_admin(client: TestClient) -> str:
    """First registrant becomes admin. Return the admin's token."""
    register(client, "admin@example.com")
    return login(client, "admin@example.com")


def test_list_users_requires_admin(client: TestClient) -> None:
    admin = _bootstrap_admin(client)
    register(client, "u@example.com")
    plain = login(client, "u@example.com")

    resp = client.get("/api/users", headers=auth_headers(plain))
    assert resp.status_code == 403

    resp = client.get("/api/users", headers=auth_headers(admin))
    assert resp.status_code == 200
    emails = sorted(u["email"] for u in resp.json())
    assert emails == ["admin@example.com", "u@example.com"]


def test_admin_can_create_user(client: TestClient) -> None:
    admin = _bootstrap_admin(client)
    resp = client.post(
        "/api/users",
        json={"email": "new@example.com", "password": "secretpw1", "role": "user"},
        headers=auth_headers(admin),
    )
    assert resp.status_code == 201, resp.text
    body = resp.json()
    assert body["email"] == "new@example.com"
    assert body["role"] == "user"

    # The new user can actually log in.
    token = login(client, "new@example.com", password="secretpw1")
    me = client.get("/api/auth/me", headers=auth_headers(token))
    assert me.json()["email"] == "new@example.com"


def test_admin_cannot_demote_self(client: TestClient) -> None:
    admin = _bootstrap_admin(client)
    me = client.get("/api/auth/me", headers=auth_headers(admin)).json()
    resp = client.patch(
        f"/api/users/{me['id']}",
        json={"role": "user"},
        headers=auth_headers(admin),
    )
    assert resp.status_code == 400


def test_admin_can_promote_demote_others(client: TestClient) -> None:
    admin = _bootstrap_admin(client)
    register(client, "u@example.com")
    # Find the new user's id
    listing = client.get("/api/users", headers=auth_headers(admin)).json()
    target = next(u for u in listing if u["email"] == "u@example.com")

    promo = client.patch(
        f"/api/users/{target['id']}",
        json={"role": "admin"},
        headers=auth_headers(admin),
    )
    assert promo.status_code == 200
    assert promo.json()["role"] == "admin"

    demote = client.patch(
        f"/api/users/{target['id']}",
        json={"role": "user"},
        headers=auth_headers(admin),
    )
    assert demote.status_code == 200
    assert demote.json()["role"] == "user"


def test_admin_cannot_delete_self(client: TestClient) -> None:
    admin = _bootstrap_admin(client)
    me = client.get("/api/auth/me", headers=auth_headers(admin)).json()
    resp = client.delete(f"/api/users/{me['id']}", headers=auth_headers(admin))
    assert resp.status_code == 400


def test_admin_can_delete_others(client: TestClient) -> None:
    admin = _bootstrap_admin(client)
    register(client, "doomed@example.com")
    listing = client.get("/api/users", headers=auth_headers(admin)).json()
    target = next(u for u in listing if u["email"] == "doomed@example.com")
    resp = client.delete(f"/api/users/{target['id']}", headers=auth_headers(admin))
    assert resp.status_code == 204

    listing = client.get("/api/users", headers=auth_headers(admin)).json()
    assert "doomed@example.com" not in [u["email"] for u in listing]


def test_create_user_validates_role(client: TestClient) -> None:
    admin = _bootstrap_admin(client)
    resp = client.post(
        "/api/users",
        json={"email": "x@example.com", "password": "secretpw1", "role": "superduper"},
        headers=auth_headers(admin),
    )
    assert resp.status_code == 400
