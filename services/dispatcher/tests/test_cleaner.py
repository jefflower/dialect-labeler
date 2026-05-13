from __future__ import annotations

import io
from datetime import timedelta

import pytest
from fastapi.testclient import TestClient

from .conftest import auth_headers, login, register


def _zip() -> bytes:
    return b"PK\x03\x04" + b"\x00" * 60


def _run_full_task(client: TestClient, owner_token: str, worker_token: str) -> str:
    resp = client.post(
        "/api/tasks",
        data={"name": "t"},
        files={"file": ("t.zip", io.BytesIO(_zip()), "application/zip")},
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
        json={"summary": {"segment_count": 1, "duration_by_role": {"user": 100}}},
        headers=auth_headers(worker_token),
    )
    return task_id


def test_cleanup_deletes_output_after_owner_download(client: TestClient, tmp_state) -> None:
    register(client, "o@example.com")
    register(client, "w@example.com")
    owner = login(client, "o@example.com")
    worker = login(client, "w@example.com")
    task_id = _run_full_task(client, owner, worker)

    # Owner downloads → output_downloaded_at gets set.
    resp = client.get(f"/api/tasks/{task_id}/output", headers=auth_headers(owner))
    assert resp.status_code == 200

    # Run the cleaner manually.
    from app.cleaner import run_cleanup_pass

    counts = run_cleanup_pass()
    assert counts["output_cleaned"] == 1
    # Input was also nuked (succeeded tasks have no further use for input).
    assert counts["input_cleaned"] == 1

    # Files gone on disk.
    storage_dir = tmp_state / "storage"
    assert not any((storage_dir / "outputs").glob("*"))
    assert not any((storage_dir / "inputs").glob("*"))

    # Re-download returns 410 — metadata still there.
    resp = client.get(f"/api/tasks/{task_id}/output", headers=auth_headers(owner))
    assert resp.status_code == 410
    resp = client.get(f"/api/tasks/{task_id}", headers=auth_headers(owner))
    assert resp.status_code == 200
    body = resp.json()
    assert body["summary"]["segment_count"] == 1
    assert body["files_cleaned_at"] is not None
    assert body["output_size"] == len(b"output-bytes")


def test_cleanup_deletes_output_after_ttl_even_without_download(
    client: TestClient, tmp_state, monkeypatch
) -> None:
    register(client, "o@example.com")
    register(client, "w@example.com")
    owner = login(client, "o@example.com")
    worker = login(client, "w@example.com")
    task_id = _run_full_task(client, owner, worker)

    # Backdate output_ready_at so it's past the configured TTL.
    from app.db import session_scope
    from app.models import Task, utcnow

    with session_scope() as db:
        task = db.get(Task, task_id)
        task.output_ready_at = utcnow() - timedelta(days=365)

    from app.cleaner import run_cleanup_pass

    counts = run_cleanup_pass()
    assert counts["output_cleaned"] == 1


def test_cleanup_keeps_failed_input_until_ttl(client: TestClient, tmp_state) -> None:
    register(client, "o@example.com")
    register(client, "w@example.com")
    owner = login(client, "o@example.com")
    worker = login(client, "w@example.com")
    resp = client.post(
        "/api/tasks",
        data={"name": "t"},
        files={"file": ("t.zip", io.BytesIO(_zip()), "application/zip")},
        headers=auth_headers(owner),
    )
    task_id = resp.json()["id"]
    client.post("/api/worker/claim", headers=auth_headers(worker))
    client.post(
        f"/api/worker/tasks/{task_id}/fail",
        json={"error": "boom"},
        headers=auth_headers(worker),
    )

    from app.cleaner import run_cleanup_pass

    # Within TTL → not cleaned.
    counts = run_cleanup_pass()
    assert counts["input_cleaned"] == 0

    # Backdate updated_at and re-run → cleaned.
    from app.db import session_scope
    from app.models import Task, utcnow

    with session_scope() as db:
        task = db.get(Task, task_id)
        task.updated_at = utcnow() - timedelta(days=365)

    counts = run_cleanup_pass()
    assert counts["input_cleaned"] == 1


def test_expired_lease_returns_task_to_pending(client: TestClient) -> None:
    register(client, "o@example.com")
    register(client, "w@example.com")
    owner = login(client, "o@example.com")
    worker = login(client, "w@example.com")
    resp = client.post(
        "/api/tasks",
        data={"name": "t"},
        files={"file": ("t.zip", io.BytesIO(_zip()), "application/zip")},
        headers=auth_headers(owner),
    )
    task_id = resp.json()["id"]
    client.post("/api/worker/claim", headers=auth_headers(worker))

    # Backdate the lease.
    from app.db import session_scope
    from app.models import Task, utcnow

    with session_scope() as db:
        task = db.get(Task, task_id)
        task.claim_expires_at = utcnow() - timedelta(seconds=1)

    from app.cleaner import run_cleanup_pass

    counts = run_cleanup_pass()
    assert counts["leases_recovered"] == 1
    assert counts["leases_expired"] == 0
    # The same task should now be claimable again.
    resp = client.post("/api/worker/claim", headers=auth_headers(worker))
    assert resp.status_code == 200
    assert resp.json()["id"] == task_id


def test_claim_increments_attempts(client: TestClient) -> None:
    register(client, "o@example.com")
    register(client, "w@example.com")
    owner = login(client, "o@example.com")
    worker = login(client, "w@example.com")
    resp = client.post(
        "/api/tasks",
        data={"name": "t"},
        files={"file": ("t.zip", io.BytesIO(_zip()), "application/zip")},
        headers=auth_headers(owner),
    )
    task_id = resp.json()["id"]

    from app.db import session_scope
    from app.models import Task

    with session_scope() as db:
        assert db.get(Task, task_id).attempts == 0

    client.post("/api/worker/claim", headers=auth_headers(worker))
    with session_scope() as db:
        assert db.get(Task, task_id).attempts == 1


def test_lease_expires_to_expired_after_max_attempts(client: TestClient) -> None:
    """After max_attempts repeated lease expirations the cleaner promotes
    the task to `expired` and stops handing it back to workers."""
    from app.config import get_settings
    from app.cleaner import run_cleanup_pass
    from app.db import session_scope
    from app.models import Task, TASK_EXPIRED, utcnow

    register(client, "o@example.com")
    register(client, "w@example.com")
    owner = login(client, "o@example.com")
    worker = login(client, "w@example.com")
    resp = client.post(
        "/api/tasks",
        data={"name": "poison"},
        files={"file": ("t.zip", io.BytesIO(_zip()), "application/zip")},
        headers=auth_headers(owner),
    )
    task_id = resp.json()["id"]
    max_attempts = get_settings().max_attempts

    # Repeatedly: claim, let the lease lapse, run the cleaner.
    for cycle in range(max_attempts):
        resp = client.post("/api/worker/claim", headers=auth_headers(worker))
        assert resp.status_code == 200
        with session_scope() as db:
            db.get(Task, task_id).claim_expires_at = utcnow() - timedelta(seconds=1)
        counts = run_cleanup_pass()
        if cycle < max_attempts - 1:
            assert counts["leases_recovered"] == 1
            assert counts["leases_expired"] == 0
        else:
            assert counts["leases_recovered"] == 0
            assert counts["leases_expired"] == 1

    # Task is now expired and not handed out again.
    with session_scope() as db:
        task = db.get(Task, task_id)
        assert task.status == TASK_EXPIRED
        assert task.attempts == max_attempts
        assert task.claimed_by is None

    resp = client.post("/api/worker/claim", headers=auth_headers(worker))
    assert resp.status_code == 204


def test_owner_retry_resets_attempts(client: TestClient) -> None:
    """An expired task can be retried by the owner; attempts resets so the
    cap doesn't immediately fire again on the next lease cycle."""
    from app.cleaner import run_cleanup_pass
    from app.config import get_settings
    from app.db import session_scope
    from app.models import TASK_EXPIRED, TASK_PENDING, Task, utcnow

    register(client, "o@example.com")
    register(client, "w@example.com")
    owner = login(client, "o@example.com")
    worker = login(client, "w@example.com")
    resp = client.post(
        "/api/tasks",
        data={"name": "poison"},
        files={"file": ("t.zip", io.BytesIO(_zip()), "application/zip")},
        headers=auth_headers(owner),
    )
    task_id = resp.json()["id"]

    # Drive it to expired.
    for _ in range(get_settings().max_attempts):
        client.post("/api/worker/claim", headers=auth_headers(worker))
        with session_scope() as db:
            db.get(Task, task_id).claim_expires_at = utcnow() - timedelta(seconds=1)
        run_cleanup_pass()
    with session_scope() as db:
        assert db.get(Task, task_id).status == TASK_EXPIRED

    # Retry as owner.
    resp = client.post(f"/api/tasks/{task_id}/retry", headers=auth_headers(owner))
    assert resp.status_code == 200
    with session_scope() as db:
        task = db.get(Task, task_id)
        assert task.status == TASK_PENDING
        assert task.attempts == 0


def test_expired_input_cleaned_after_ttl(client: TestClient, tmp_state) -> None:
    """An expired task's input zip is collected by the same failed_input_ttl
    window the failed branch uses (since the input is again pointless)."""
    from app.cleaner import run_cleanup_pass
    from app.config import get_settings
    from app.db import session_scope
    from app.models import Task, utcnow

    register(client, "o@example.com")
    register(client, "w@example.com")
    owner = login(client, "o@example.com")
    worker = login(client, "w@example.com")
    resp = client.post(
        "/api/tasks",
        data={"name": "t"},
        files={"file": ("t.zip", io.BytesIO(_zip()), "application/zip")},
        headers=auth_headers(owner),
    )
    task_id = resp.json()["id"]

    # Drive to expired.
    for _ in range(get_settings().max_attempts):
        client.post("/api/worker/claim", headers=auth_headers(worker))
        with session_scope() as db:
            db.get(Task, task_id).claim_expires_at = utcnow() - timedelta(seconds=1)
        run_cleanup_pass()

    # Within TTL → input untouched.
    counts = run_cleanup_pass()
    assert counts["input_cleaned"] == 0

    # Past TTL → input is collected.
    with session_scope() as db:
        db.get(Task, task_id).updated_at = utcnow() - timedelta(days=365)
    counts = run_cleanup_pass()
    assert counts["input_cleaned"] == 1
