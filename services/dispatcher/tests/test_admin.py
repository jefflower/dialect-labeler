from __future__ import annotations

import io

from fastapi.testclient import TestClient

from .conftest import auth_headers, login, register


def _zip() -> bytes:
    return b"PK\x03\x04" + b"\x00" * 60


def test_stats_requires_admin(client: TestClient) -> None:
    register(client, "admin@example.com")  # first registrant → admin
    register(client, "u@example.com")
    plain = login(client, "u@example.com")
    resp = client.get("/api/admin/stats", headers=auth_headers(plain))
    assert resp.status_code == 403


def test_empty_stats_returns_zeros(client: TestClient) -> None:
    register(client, "admin@example.com")
    admin = login(client, "admin@example.com")
    resp = client.get("/api/admin/stats", headers=auth_headers(admin))
    assert resp.status_code == 200
    body = resp.json()
    assert body["queue_depth"] == 0
    assert body["in_flight"] == 0
    assert body["succeeded_24h"] == 0
    assert body["failed_24h"] == 0
    assert body["total_admin_count"] == 1


def test_stats_reflect_pending_tasks(client: TestClient) -> None:
    # Per-user single-task constraint forces us to spread the 3 uploads
    # across 3 separate users to land 3 simultaneously-pending tasks.
    register(client, "admin@example.com")
    admin = login(client, "admin@example.com")
    for i in range(3):
        register(client, f"u{i}@example.com")
        token = login(client, f"u{i}@example.com")
        client.post(
            "/api/tasks",
            data={"name": f"t{i}"},
            files={"file": ("t.zip", io.BytesIO(_zip()), "application/zip")},
            headers=auth_headers(token),
        )
    resp = client.get("/api/admin/stats", headers=auth_headers(admin))
    body = resp.json()
    assert body["queue_depth"] == 3
    assert body["task_counts"]["pending"] == 3
    assert body["storage"]["inputs_files"] == 3
    assert body["storage"]["inputs_bytes"] > 0
