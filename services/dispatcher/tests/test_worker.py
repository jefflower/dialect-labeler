from __future__ import annotations

import io
import threading

import pytest
from fastapi.testclient import TestClient

from .conftest import auth_headers, login, register


def _zip() -> bytes:
    return b"PK\x03\x04" + b"\x00" * 60


def _upload_task(client: TestClient, token: str, name: str = "t") -> str:
    resp = client.post(
        "/api/tasks",
        data={"name": name},
        files={"file": ("t.zip", io.BytesIO(_zip()), "application/zip")},
        headers=auth_headers(token),
    )
    assert resp.status_code == 201
    return resp.json()["id"]


def test_claim_returns_204_when_queue_empty(client: TestClient) -> None:
    register(client, "w@example.com")
    token = login(client, "w@example.com")
    resp = client.post("/api/worker/claim", headers=auth_headers(token))
    assert resp.status_code == 204


def test_full_happy_path(client: TestClient, tmp_state) -> None:
    register(client, "owner@example.com")
    register(client, "worker@example.com")
    owner = login(client, "owner@example.com")
    worker = login(client, "worker@example.com")

    task_id = _upload_task(client, owner)

    claim = client.post("/api/worker/claim", headers=auth_headers(worker))
    assert claim.status_code == 200
    assert claim.json()["id"] == task_id

    # download input
    resp = client.get(
        f"/api/worker/tasks/{task_id}/input", headers=auth_headers(worker)
    )
    assert resp.status_code == 200
    assert resp.content[:4] == b"PK\x03\x04"

    # heartbeat
    resp = client.post(
        f"/api/worker/tasks/{task_id}/heartbeat", headers=auth_headers(worker)
    )
    assert resp.status_code == 204

    # upload output (raw PUT body, not multipart)
    out_bytes = b"PK\x05\x06" + b"\x01" * 200
    resp = client.put(
        f"/api/worker/tasks/{task_id}/output",
        content=out_bytes,
        headers={**auth_headers(worker), "Content-Type": "application/zip"},
    )
    assert resp.status_code == 204, resp.text

    # complete with summary
    summary = {
        "segment_count": 12,
        "duration_by_role": {"user": 5000, "assistant": 7000},
        "elapsed_ms": 4500,
    }
    resp = client.post(
        f"/api/worker/tasks/{task_id}/complete",
        json={"summary": summary},
        headers=auth_headers(worker),
    )
    assert resp.status_code == 204

    # owner can now read summary back
    resp = client.get(f"/api/tasks/{task_id}", headers=auth_headers(owner))
    body = resp.json()
    assert body["status"] == "succeeded"
    assert body["summary"]["segment_count"] == 12
    assert body["summary"]["duration_by_role"]["assistant"] == 7000

    # owner downloads output
    resp = client.get(
        f"/api/tasks/{task_id}/output", headers=auth_headers(owner)
    )
    assert resp.status_code == 200
    assert resp.content == out_bytes


def test_fail_records_error_and_releases_lease(client: TestClient) -> None:
    register(client, "owner@example.com")
    register(client, "w@example.com")
    owner = login(client, "owner@example.com")
    worker = login(client, "w@example.com")
    task_id = _upload_task(client, owner)

    client.post("/api/worker/claim", headers=auth_headers(worker))
    resp = client.post(
        f"/api/worker/tasks/{task_id}/fail",
        json={"error": "ffmpeg missing"},
        headers=auth_headers(worker),
    )
    assert resp.status_code == 204
    resp = client.get(f"/api/tasks/{task_id}", headers=auth_headers(owner))
    body = resp.json()
    assert body["status"] == "failed"
    assert body["error"] == "ffmpeg missing"


def test_complete_without_output_upload_is_400(client: TestClient) -> None:
    register(client, "owner@example.com")
    register(client, "w@example.com")
    owner = login(client, "owner@example.com")
    worker = login(client, "w@example.com")
    task_id = _upload_task(client, owner)
    client.post("/api/worker/claim", headers=auth_headers(worker))
    resp = client.post(
        f"/api/worker/tasks/{task_id}/complete",
        json={"summary": {}},
        headers=auth_headers(worker),
    )
    assert resp.status_code == 400


def test_unclaimed_worker_cannot_download_input(client: TestClient) -> None:
    register(client, "owner@example.com")
    register(client, "w@example.com")
    owner = login(client, "owner@example.com")
    worker = login(client, "w@example.com")
    task_id = _upload_task(client, owner)
    # No claim → no lease → 409
    resp = client.get(
        f"/api/worker/tasks/{task_id}/input", headers=auth_headers(worker)
    )
    assert resp.status_code == 409


def test_concurrent_claims_only_one_wins_per_task(client: TestClient) -> None:
    """Stress the atomic claim: two workers racing on a single task —
    exactly one gets it, the other gets 204."""
    register(client, "owner@example.com")
    register(client, "w1@example.com")
    register(client, "w2@example.com")
    owner = login(client, "owner@example.com")
    w1 = login(client, "w1@example.com")
    w2 = login(client, "w2@example.com")
    _upload_task(client, owner)

    results: list[int] = []

    def claim(tok: str) -> None:
        r = client.post("/api/worker/claim", headers=auth_headers(tok))
        results.append(r.status_code)

    t1 = threading.Thread(target=claim, args=(w1,))
    t2 = threading.Thread(target=claim, args=(w2,))
    t1.start()
    t2.start()
    t1.join()
    t2.join()

    # One must be 200 (claimed), the other 204 (empty queue after rival's
    # claim).
    assert sorted(results) == [200, 204]
