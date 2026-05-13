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


def test_retry_failed_task_resets_to_pending(client: TestClient) -> None:
    register(client, "o@example.com")
    register(client, "w@example.com")
    owner = login(client, "o@example.com")
    worker = login(client, "w@example.com")

    # Create + claim + fail.
    resp = client.post(
        "/api/tasks",
        data={"name": "doomed"},
        files={"file": ("t.zip", io.BytesIO(_make_zip_bytes()), "application/zip")},
        headers=auth_headers(owner),
    )
    task_id = resp.json()["id"]
    client.post("/api/worker/claim", headers=auth_headers(worker))
    client.post(
        f"/api/worker/tasks/{task_id}/fail",
        json={"error": "ffmpeg crashed"},
        headers=auth_headers(worker),
    )

    # Owner retries.
    resp = client.post(f"/api/tasks/{task_id}/retry", headers=auth_headers(owner))
    assert resp.status_code == 200, resp.text
    body = resp.json()
    assert body["status"] == "pending"
    assert body["error"] is None

    # A worker can claim it again.
    claim = client.post("/api/worker/claim", headers=auth_headers(worker))
    assert claim.status_code == 200
    assert claim.json()["id"] == task_id


def test_retry_refuses_when_input_cleaned(client: TestClient, tmp_state) -> None:
    register(client, "o@example.com")
    register(client, "w@example.com")
    owner = login(client, "o@example.com")
    worker = login(client, "w@example.com")
    resp = client.post(
        "/api/tasks",
        data={"name": "doomed"},
        files={"file": ("t.zip", io.BytesIO(_make_zip_bytes()), "application/zip")},
        headers=auth_headers(owner),
    )
    task_id = resp.json()["id"]
    client.post("/api/worker/claim", headers=auth_headers(worker))
    client.post(
        f"/api/worker/tasks/{task_id}/fail",
        json={"error": "boom"},
        headers=auth_headers(worker),
    )

    # Simulate the cleaner having unlinked the input zip.
    from app.db import session_scope
    from app.models import Task

    with session_scope() as db:
        t = db.get(Task, task_id)
        if t.input_path:
            (tmp_state / "storage" / t.input_path).unlink(missing_ok=True)

    resp = client.post(f"/api/tasks/{task_id}/retry", headers=auth_headers(owner))
    assert resp.status_code == 410


def test_retry_refuses_when_task_still_pending(client: TestClient) -> None:
    register(client, "o@example.com")
    owner = login(client, "o@example.com")
    resp = client.post(
        "/api/tasks",
        data={"name": "still-going"},
        files={"file": ("t.zip", io.BytesIO(_make_zip_bytes()), "application/zip")},
        headers=auth_headers(owner),
    )
    task_id = resp.json()["id"]
    resp = client.post(f"/api/tasks/{task_id}/retry", headers=auth_headers(owner))
    assert resp.status_code == 409


def test_task_detail_exposes_claimer_email_during_lease(client: TestClient) -> None:
    register(client, "o@example.com")
    register(client, "robot@example.com")
    owner = login(client, "o@example.com")
    worker = login(client, "robot@example.com")
    resp = client.post(
        "/api/tasks",
        data={"name": "watch-me"},
        files={"file": ("t.zip", io.BytesIO(_make_zip_bytes()), "application/zip")},
        headers=auth_headers(owner),
    )
    task_id = resp.json()["id"]
    client.post("/api/worker/claim", headers=auth_headers(worker))

    resp = client.get(f"/api/tasks/{task_id}", headers=auth_headers(owner))
    body = resp.json()
    assert body["claimer_email"] == "robot@example.com"
    assert body["claim_expires_at"] is not None


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


def _complete_task_as_worker(client: TestClient, owner_token: str, worker_token: str) -> str:
    """Create a task as owner, run it through the worker lifecycle to
    `succeeded`, return the task id. Helper for access-control tests
    that need an output to attempt cross-user download."""
    resp = client.post(
        "/api/tasks",
        data={"name": "owned"},
        files={"file": ("t.zip", io.BytesIO(_make_zip_bytes()), "application/zip")},
        headers=auth_headers(owner_token),
    )
    task_id = resp.json()["id"]
    client.post("/api/worker/claim", headers=auth_headers(worker_token))
    client.put(
        f"/api/worker/tasks/{task_id}/output",
        content=b"output-bytes",
        headers={**auth_headers(worker_token), "Content-Type": "application/zip"},
    )
    client.post(
        f"/api/worker/tasks/{task_id}/complete",
        json={"summary": {"segment_count": 1}},
        headers=auth_headers(worker_token),
    )
    return task_id


def test_cannot_download_another_users_output(client: TestClient) -> None:
    register(client, "a@example.com")  # admin
    register(client, "b@example.com")
    register(client, "c@example.com")
    register(client, "w@example.com")  # worker
    token_b = login(client, "b@example.com")
    token_c = login(client, "c@example.com")
    token_w = login(client, "w@example.com")
    task_id = _complete_task_as_worker(client, token_b, token_w)
    # Sanity: owner can download.
    resp = client.get(f"/api/tasks/{task_id}/output", headers=auth_headers(token_b))
    assert resp.status_code == 200
    # Non-owner non-admin must NOT see the bytes.
    resp = client.get(f"/api/tasks/{task_id}/output", headers=auth_headers(token_c))
    assert resp.status_code in (403, 404)


def test_cannot_delete_another_users_task(client: TestClient) -> None:
    register(client, "a@example.com")  # admin
    register(client, "b@example.com")
    register(client, "c@example.com")
    token_b = login(client, "b@example.com")
    token_c = login(client, "c@example.com")
    resp = client.post(
        "/api/tasks",
        data={"name": "b-secret"},
        files={"file": ("t.zip", io.BytesIO(_make_zip_bytes()), "application/zip")},
        headers=auth_headers(token_b),
    )
    task_id = resp.json()["id"]
    resp = client.delete(f"/api/tasks/{task_id}", headers=auth_headers(token_c))
    assert resp.status_code in (403, 404)
    # Owner still sees the task.
    resp = client.get(f"/api/tasks/{task_id}", headers=auth_headers(token_b))
    assert resp.status_code == 200


def test_cannot_retry_another_users_task(client: TestClient) -> None:
    register(client, "a@example.com")
    register(client, "b@example.com")
    register(client, "c@example.com")
    register(client, "w@example.com")
    token_b = login(client, "b@example.com")
    token_c = login(client, "c@example.com")
    token_w = login(client, "w@example.com")
    resp = client.post(
        "/api/tasks",
        data={"name": "owned"},
        files={"file": ("t.zip", io.BytesIO(_make_zip_bytes()), "application/zip")},
        headers=auth_headers(token_b),
    )
    task_id = resp.json()["id"]
    client.post("/api/worker/claim", headers=auth_headers(token_w))
    client.post(
        f"/api/worker/tasks/{task_id}/fail",
        json={"error": "boom"},
        headers=auth_headers(token_w),
    )
    resp = client.post(f"/api/tasks/{task_id}/retry", headers=auth_headers(token_c))
    assert resp.status_code in (403, 404)


def test_admin_can_delete_other_users_task(client: TestClient) -> None:
    """First registrant is admin (conftest pattern). They can delete
    any user's task — used for cleanup of abandoned uploads."""
    register(client, "admin@example.com")  # becomes admin
    register(client, "b@example.com")
    token_admin = login(client, "admin@example.com")
    token_b = login(client, "b@example.com")
    resp = client.post(
        "/api/tasks",
        data={"name": "to-prune"},
        files={"file": ("t.zip", io.BytesIO(_make_zip_bytes()), "application/zip")},
        headers=auth_headers(token_b),
    )
    task_id = resp.json()["id"]
    resp = client.delete(f"/api/tasks/{task_id}", headers=auth_headers(token_admin))
    assert resp.status_code == 204
    resp = client.get(f"/api/tasks/{task_id}", headers=auth_headers(token_b))
    assert resp.status_code == 404


def test_cannot_upload_when_active_task_exists(client: TestClient) -> None:
    """Per-user single-task gate. Second create_task while the first is
    still pending must be rejected with 409 BEFORE the new zip is
    accepted to disk."""
    register(client, "u@example.com")
    token = login(client, "u@example.com")
    resp = client.post(
        "/api/tasks",
        data={"name": "first"},
        files={"file": ("a.zip", io.BytesIO(_make_zip_bytes()), "application/zip")},
        headers=auth_headers(token),
    )
    assert resp.status_code == 201
    resp = client.post(
        "/api/tasks",
        data={"name": "second"},
        files={"file": ("b.zip", io.BytesIO(_make_zip_bytes()), "application/zip")},
        headers=auth_headers(token),
    )
    assert resp.status_code == 409
    assert "已有正在进行的任务" in resp.text


def test_close_succeeded_task_clears_files_and_frees_slot(
    client: TestClient, tmp_state
) -> None:
    register(client, "o@example.com")
    register(client, "w@example.com")
    owner = login(client, "o@example.com")
    worker = login(client, "w@example.com")
    task_id = _complete_task_as_worker(client, owner, worker)

    resp = client.post(
        "/api/tasks",
        data={"name": "blocked"},
        files={"file": ("b.zip", io.BytesIO(_make_zip_bytes()), "application/zip")},
        headers=auth_headers(owner),
    )
    assert resp.status_code == 409

    resp = client.post(f"/api/tasks/{task_id}/close", headers=auth_headers(owner))
    assert resp.status_code == 200
    assert resp.json()["status"] == "closed"

    storage_dir = tmp_state / "storage"
    assert not any((storage_dir / "outputs").iterdir())
    assert not any((storage_dir / "inputs").iterdir())

    resp = client.get(f"/api/tasks/{task_id}/output", headers=auth_headers(owner))
    assert resp.status_code in (409, 410)

    resp = client.post(
        "/api/tasks",
        data={"name": "after-close"},
        files={"file": ("c.zip", io.BytesIO(_make_zip_bytes()), "application/zip")},
        headers=auth_headers(owner),
    )
    assert resp.status_code == 201


def test_cannot_close_pending_task(client: TestClient) -> None:
    register(client, "u@example.com")
    token = login(client, "u@example.com")
    resp = client.post(
        "/api/tasks",
        data={"name": "still-pending"},
        files={"file": ("t.zip", io.BytesIO(_make_zip_bytes()), "application/zip")},
        headers=auth_headers(token),
    )
    task_id = resp.json()["id"]
    resp = client.post(f"/api/tasks/{task_id}/close", headers=auth_headers(token))
    assert resp.status_code == 409


def test_cannot_close_another_users_task(client: TestClient) -> None:
    register(client, "a@example.com")
    register(client, "b@example.com")
    register(client, "c@example.com")
    register(client, "w@example.com")
    token_b = login(client, "b@example.com")
    token_c = login(client, "c@example.com")
    token_w = login(client, "w@example.com")
    task_id = _complete_task_as_worker(client, token_b, token_w)
    resp = client.post(f"/api/tasks/{task_id}/close", headers=auth_headers(token_c))
    assert resp.status_code in (403, 404)


def test_failed_task_does_not_block_new_uploads(client: TestClient) -> None:
    register(client, "o@example.com")
    register(client, "w@example.com")
    owner = login(client, "o@example.com")
    worker = login(client, "w@example.com")
    resp = client.post(
        "/api/tasks",
        data={"name": "doomed"},
        files={"file": ("a.zip", io.BytesIO(_make_zip_bytes()), "application/zip")},
        headers=auth_headers(owner),
    )
    task_id = resp.json()["id"]
    client.post("/api/worker/claim", headers=auth_headers(worker))
    client.post(
        f"/api/worker/tasks/{task_id}/fail",
        json={"error": "boom"},
        headers=auth_headers(worker),
    )
    resp = client.post(
        "/api/tasks",
        data={"name": "fresh"},
        files={"file": ("b.zip", io.BytesIO(_make_zip_bytes()), "application/zip")},
        headers=auth_headers(owner),
    )
    assert resp.status_code == 201


def test_retry_blocked_by_other_active_task(client: TestClient) -> None:
    register(client, "o@example.com")
    register(client, "w@example.com")
    owner = login(client, "o@example.com")
    worker = login(client, "w@example.com")
    resp = client.post(
        "/api/tasks",
        data={"name": "old-fail"},
        files={"file": ("a.zip", io.BytesIO(_make_zip_bytes()), "application/zip")},
        headers=auth_headers(owner),
    )
    task_a = resp.json()["id"]
    client.post("/api/worker/claim", headers=auth_headers(worker))
    client.post(
        f"/api/worker/tasks/{task_a}/fail",
        json={"error": "x"},
        headers=auth_headers(worker),
    )
    client.post(
        "/api/tasks",
        data={"name": "active"},
        files={"file": ("b.zip", io.BytesIO(_make_zip_bytes()), "application/zip")},
        headers=auth_headers(owner),
    )
    resp = client.post(f"/api/tasks/{task_a}/retry", headers=auth_headers(owner))
    assert resp.status_code == 409


# ---------------------------------------------------------------------------
# Mode field — committed at upload time, returned on every read. Default is
# `dialect` for backwards-compat with old clients.
# ---------------------------------------------------------------------------


def test_create_task_defaults_mode_to_dialect(client: TestClient) -> None:
    """Old clients that don't send `mode` should still get a working task."""
    register(client, "u@example.com")
    token = login(client, "u@example.com")
    resp = client.post(
        "/api/tasks",
        data={"name": "no-mode"},
        files={"file": ("t.zip", io.BytesIO(_make_zip_bytes()), "application/zip")},
        headers=auth_headers(token),
    )
    assert resp.status_code == 201, resp.text
    assert resp.json()["mode"] == "dialect"


def test_create_task_accepts_semantic_mode(client: TestClient) -> None:
    register(client, "u@example.com")
    token = login(client, "u@example.com")
    resp = client.post(
        "/api/tasks",
        data={"name": "mando", "mode": "semantic"},
        files={"file": ("t.zip", io.BytesIO(_make_zip_bytes()), "application/zip")},
        headers=auth_headers(token),
    )
    assert resp.status_code == 201, resp.text
    body = resp.json()
    assert body["mode"] == "semantic"

    # Round-trips through GET so we know the column was actually persisted.
    detail = client.get(f"/api/tasks/{body['id']}", headers=auth_headers(token)).json()
    assert detail["mode"] == "semantic"


def test_create_task_rejects_invalid_mode(client: TestClient) -> None:
    """Typo defence — we'd rather 400 than silently default."""
    register(client, "u@example.com")
    token = login(client, "u@example.com")
    resp = client.post(
        "/api/tasks",
        data={"name": "typo", "mode": "diaclet"},
        files={"file": ("t.zip", io.BytesIO(_make_zip_bytes()), "application/zip")},
        headers=auth_headers(token),
    )
    assert resp.status_code == 400, resp.text
    assert "diaclet" in resp.text


def test_claim_returns_mode_so_worker_can_route(client: TestClient) -> None:
    """Worker reads `mode` off the claim payload and uses it to override
    whatever is in the bundle's project.json. Without this field the Mac
    Worker would silently process a Mode-2 upload with Mode-1 settings."""
    register(client, "o@example.com")
    register(client, "w@example.com")
    owner = login(client, "o@example.com")
    worker = login(client, "w@example.com")
    client.post(
        "/api/tasks",
        data={"name": "mando", "mode": "semantic"},
        files={"file": ("t.zip", io.BytesIO(_make_zip_bytes()), "application/zip")},
        headers=auth_headers(owner),
    )
    resp = client.post("/api/worker/claim", headers=auth_headers(worker))
    assert resp.status_code == 200, resp.text
    assert resp.json()["mode"] == "semantic"


# ---------------------------------------------------------------------------
# Progress push — Worker reports stage + percent so owner polling sees it.
# ---------------------------------------------------------------------------


def test_progress_push_updates_task_row(client: TestClient) -> None:
    register(client, "o@example.com")
    register(client, "w@example.com")
    owner = login(client, "o@example.com")
    worker = login(client, "w@example.com")
    create = client.post(
        "/api/tasks",
        data={"name": "p"},
        files={"file": ("t.zip", io.BytesIO(_make_zip_bytes()), "application/zip")},
        headers=auth_headers(owner),
    )
    task_id = create.json()["id"]
    client.post("/api/worker/claim", headers=auth_headers(worker))

    push = client.post(
        f"/api/worker/tasks/{task_id}/progress",
        json={"percent": 37, "stage": "ASR", "detail": "12 / 30 files"},
        headers=auth_headers(worker),
    )
    assert push.status_code == 204, push.text

    detail = client.get(f"/api/tasks/{task_id}", headers=auth_headers(owner)).json()
    assert detail["progress_percent"] == 37
    assert detail["progress_stage"] == "ASR"
    assert detail["progress_detail"] == "12 / 30 files"
    assert detail["progress_updated_at"] is not None


def test_progress_clamped_to_0_100(client: TestClient) -> None:
    """The schema enforces ge/le; pydantic returns 422 for out-of-range."""
    register(client, "o@example.com")
    register(client, "w@example.com")
    owner = login(client, "o@example.com")
    worker = login(client, "w@example.com")
    create = client.post(
        "/api/tasks",
        data={"name": "p"},
        files={"file": ("t.zip", io.BytesIO(_make_zip_bytes()), "application/zip")},
        headers=auth_headers(owner),
    )
    task_id = create.json()["id"]
    client.post("/api/worker/claim", headers=auth_headers(worker))
    resp = client.post(
        f"/api/worker/tasks/{task_id}/progress",
        json={"percent": 250, "stage": "x"},
        headers=auth_headers(worker),
    )
    assert resp.status_code == 422


def test_retry_clears_stale_progress(client: TestClient) -> None:
    """A retried task starting from zero should not advertise the
    previous attempt's last progress value."""
    register(client, "o@example.com")
    register(client, "w@example.com")
    owner = login(client, "o@example.com")
    worker = login(client, "w@example.com")
    create = client.post(
        "/api/tasks",
        data={"name": "p"},
        files={"file": ("t.zip", io.BytesIO(_make_zip_bytes()), "application/zip")},
        headers=auth_headers(owner),
    )
    task_id = create.json()["id"]
    client.post("/api/worker/claim", headers=auth_headers(worker))
    client.post(
        f"/api/worker/tasks/{task_id}/progress",
        json={"percent": 70, "stage": "almost", "detail": "..."},
        headers=auth_headers(worker),
    )
    client.post(
        f"/api/worker/tasks/{task_id}/fail",
        json={"error": "boom"},
        headers=auth_headers(worker),
    )
    client.post(f"/api/tasks/{task_id}/retry", headers=auth_headers(owner))
    detail = client.get(f"/api/tasks/{task_id}", headers=auth_headers(owner)).json()
    assert detail["progress_percent"] is None
    assert detail["progress_stage"] is None
    assert detail["progress_detail"] is None
