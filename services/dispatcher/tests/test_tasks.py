from __future__ import annotations

import io

from fastapi.testclient import TestClient

from .conftest import auth_headers, login, register


def _make_zip_bytes(size: int = 64) -> bytes:
    return b"PK\x03\x04" + b"\x00" * (size - 4)


def test_create_task_stores_input_zip(client: TestClient, tmp_state) -> None:
    register(client, "u@example.com")
    token = login(client, "u@example.com")

    payload = _make_zip_bytes()
    resp = client.post(
        "/api/tasks",
        data={"name": "demo project"},
        files={"file": ("demo.zip", io.BytesIO(payload), "application/zip")},
        headers=auth_headers(token),
    )
    assert resp.status_code == 201, resp.text
    body = resp.json()
    assert body["status"] == "pending"
    assert body["input_size"] == len(payload)
    assert body["name"] == "demo project"
    # File landed on disk under the configured storage dir.
    storage_dir = tmp_state / "storage"
    files = list((storage_dir / "inputs").iterdir())
    assert len(files) == 1
    assert files[0].read_bytes() == payload


def test_empty_upload_rejected(client: TestClient) -> None:
    register(client, "u@example.com")
    token = login(client, "u@example.com")
    resp = client.post(
        "/api/tasks",
        data={"name": "empty"},
        files={"file": ("empty.zip", io.BytesIO(b""), "application/zip")},
        headers=auth_headers(token),
    )
    assert resp.status_code == 400


def test_list_returns_own_tasks_only(client: TestClient) -> None:
    register(client, "a@example.com")  # becomes admin
    register(client, "b@example.com")
    register(client, "c@example.com")
    token_b = login(client, "b@example.com")
    token_c = login(client, "c@example.com")

    for token in (token_b, token_c):
        client.post(
            "/api/tasks",
            data={"name": "task"},
            files={"file": ("t.zip", io.BytesIO(_make_zip_bytes()), "application/zip")},
            headers=auth_headers(token),
        )

    # B sees only their own.
    resp = client.get("/api/tasks", headers=auth_headers(token_b))
    assert resp.status_code == 200
    assert len(resp.json()) == 1

    # Admin sees both.
    token_admin = login(client, "a@example.com")
    resp = client.get("/api/tasks", headers=auth_headers(token_admin))
    assert resp.status_code == 200
    assert len(resp.json()) == 2


def test_cannot_view_another_users_task(client: TestClient) -> None:
    register(client, "a@example.com")  # admin
    register(client, "b@example.com")
    register(client, "c@example.com")
    token_b = login(client, "b@example.com")
    token_c = login(client, "c@example.com")

    resp = client.post(
        "/api/tasks",
        data={"name": "secret"},
        files={"file": ("t.zip", io.BytesIO(_make_zip_bytes()), "application/zip")},
        headers=auth_headers(token_b),
    )
    task_id = resp.json()["id"]

    resp = client.get(f"/api/tasks/{task_id}", headers=auth_headers(token_c))
    assert resp.status_code == 403


def test_download_pending_task_output_is_409(client: TestClient) -> None:
    register(client, "u@example.com")
    token = login(client, "u@example.com")
    resp = client.post(
        "/api/tasks",
        data={"name": "pending"},
        files={"file": ("t.zip", io.BytesIO(_make_zip_bytes()), "application/zip")},
        headers=auth_headers(token),
    )
    task_id = resp.json()["id"]
    resp = client.get(f"/api/tasks/{task_id}/output", headers=auth_headers(token))
    assert resp.status_code == 409


def test_delete_removes_files_and_row(client: TestClient, tmp_state) -> None:
    register(client, "u@example.com")
    token = login(client, "u@example.com")
    resp = client.post(
        "/api/tasks",
        data={"name": "to-delete"},
        files={"file": ("t.zip", io.BytesIO(_make_zip_bytes()), "application/zip")},
        headers=auth_headers(token),
    )
    task_id = resp.json()["id"]
    storage_dir = tmp_state / "storage"
    assert any((storage_dir / "inputs").iterdir())

    resp = client.delete(f"/api/tasks/{task_id}", headers=auth_headers(token))
    assert resp.status_code == 204
    assert not any((storage_dir / "inputs").iterdir())
    resp = client.get(f"/api/tasks/{task_id}", headers=auth_headers(token))
    assert resp.status_code == 404
