from __future__ import annotations

import io

from fastapi.testclient import TestClient

from .conftest import auth_headers, login, register


def _admin_token(client: TestClient) -> str:
    register(client, "admin@example.com")
    return login(client, "admin@example.com")


def _upload(
    client: TestClient,
    token: str,
    version: str,
    *,
    target: str = "windows-x86_64",
    channel: str = "stable",
    signature: str | None = None,
    notes: str | None = None,
) -> dict:
    data: dict = {"version": version, "target": target, "channel": channel}
    if signature is not None:
        data["signature"] = signature
    if notes is not None:
        data["notes"] = notes
    payload = b"MSI" + b"\x00" * 60
    resp = client.post(
        "/api/releases",
        data=data,
        files={
            "file": (
                f"dialect-labeler_{version}_x64.msi",
                io.BytesIO(payload),
                "application/octet-stream",
            )
        },
        headers=auth_headers(token),
    )
    assert resp.status_code == 201, resp.text
    return resp.json()


def test_upload_lists_and_deletes(client: TestClient) -> None:
    admin = _admin_token(client)
    body = _upload(client, admin, "0.1.0", notes="initial")
    assert body["version"] == "0.1.0"
    assert body["target"] == "windows-x86_64"
    assert body["installer_size"] is not None

    resp = client.get("/api/releases", headers=auth_headers(admin))
    assert resp.status_code == 200
    assert any(r["version"] == "0.1.0" for r in resp.json())

    resp = client.delete(f"/api/releases/{body['id']}", headers=auth_headers(admin))
    assert resp.status_code == 204
    resp = client.get("/api/releases", headers=auth_headers(admin))
    assert all(r["version"] != "0.1.0" for r in resp.json())


def test_upload_requires_admin(client: TestClient) -> None:
    _admin_token(client)
    register(client, "u@example.com")
    plain = login(client, "u@example.com")
    resp = client.post(
        "/api/releases",
        data={"version": "0.1.0", "target": "windows-x86_64"},
        files={"file": ("a.msi", io.BytesIO(b"MSI\x00"), "application/octet-stream")},
        headers=auth_headers(plain),
    )
    assert resp.status_code == 403


def test_invalid_version_rejected(client: TestClient) -> None:
    admin = _admin_token(client)
    resp = client.post(
        "/api/releases",
        data={"version": "not-a-version", "target": "windows-x86_64"},
        files={"file": ("a.msi", io.BytesIO(b"x"), "application/octet-stream")},
        headers=auth_headers(admin),
    )
    assert resp.status_code == 400


def test_unsafe_filename_rejected(client: TestClient) -> None:
    admin = _admin_token(client)
    resp = client.post(
        "/api/releases",
        data={"version": "0.1.0", "target": "windows-x86_64"},
        files={
            "file": (
                "../escape.msi",
                io.BytesIO(b"x"),
                "application/octet-stream",
            )
        },
        headers=auth_headers(admin),
    )
    assert resp.status_code == 400


def test_latest_picks_highest_version(client: TestClient) -> None:
    admin = _admin_token(client)
    _upload(client, admin, "0.1.0")
    _upload(client, admin, "0.1.2")
    _upload(client, admin, "0.1.10")  # ten, not "1.0"

    resp = client.get("/api/releases/latest?target=windows-x86_64")
    assert resp.status_code == 200
    assert resp.json()["version"] == "0.1.10"


def test_public_download_works_without_auth(client: TestClient) -> None:
    admin = _admin_token(client)
    body = _upload(client, admin, "0.1.0")
    resp = client.get(body["id"] and f"/api/releases/{body['id']}/download")
    assert resp.status_code == 200
    assert resp.content.startswith(b"MSI")


def test_updater_returns_204_when_current(client: TestClient) -> None:
    admin = _admin_token(client)
    _upload(client, admin, "0.1.0", signature="sig-blob-0")
    resp = client.get("/api/updater/windows-x86_64/0.1.0")
    assert resp.status_code == 204


def test_updater_returns_204_when_no_signature(client: TestClient) -> None:
    """Even if a newer release exists, refuse to advertise it without a
    signature — Tauri won't install it anyway and we don't want clients
    looping."""
    admin = _admin_token(client)
    _upload(client, admin, "0.1.1")  # no signature
    resp = client.get("/api/updater/windows-x86_64/0.1.0")
    assert resp.status_code == 204


def test_updater_returns_manifest_for_signed_newer(client: TestClient) -> None:
    admin = _admin_token(client)
    _upload(client, admin, "0.1.1", signature="sig-blob-1", notes="bug fix")
    resp = client.get("/api/updater/windows-x86_64/0.1.0")
    assert resp.status_code == 200
    body = resp.json()
    assert body["version"] == "0.1.1"
    assert body["signature"] == "sig-blob-1"
    assert body["notes"] == "bug fix"
    assert body["url"].endswith("/download")


def test_updater_unknown_target_returns_204(client: TestClient) -> None:
    resp = client.get("/api/updater/atari-st/0.0.1")
    assert resp.status_code == 204


def test_duplicate_release_returns_409(client: TestClient) -> None:
    admin = _admin_token(client)
    _upload(client, admin, "0.1.0")
    payload = b"MSI" + b"\x00" * 60
    resp = client.post(
        "/api/releases",
        data={"version": "0.1.0", "target": "windows-x86_64", "channel": "stable"},
        files={
            "file": (
                "a.msi",
                io.BytesIO(payload),
                "application/octet-stream",
            )
        },
        headers=auth_headers(admin),
    )
    assert resp.status_code == 409
