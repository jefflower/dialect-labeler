"""Admin observability — stats for the dashboard.

Single endpoint, single shape. Cheap to call (one COUNT GROUP BY +
one walk of STORAGE_DIR), so the dashboard can poll it on a 5-10s
interval without breaking a sweat.

Storage walk is bounded to one ls per known subdir (inputs / outputs /
releases) so a huge release archive doesn't make /admin/stats slow.
"""

from __future__ import annotations

import os
from datetime import timedelta
from typing import Annotated

from fastapi import APIRouter, Depends
from pydantic import BaseModel
from sqlalchemy import case, func, select
from sqlalchemy.orm import Session

from ..auth import AdminUser
from ..config import get_settings
from ..db import get_db
from ..models import (
    TASK_CLAIMED,
    TASK_FAILED,
    TASK_PENDING,
    TASK_RUNNING,
    TASK_SUCCEEDED,
    Task,
    User,
    utcnow,
)

router = APIRouter(prefix="/api/admin", tags=["admin"])


class StorageBreakdown(BaseModel):
    inputs_bytes: int
    inputs_files: int
    outputs_bytes: int
    outputs_files: int
    releases_bytes: int
    releases_files: int


class StatsOut(BaseModel):
    task_counts: dict[str, int]
    queue_depth: int
    in_flight: int
    succeeded_24h: int
    failed_24h: int
    total_user_count: int
    total_admin_count: int
    # Self-registered users waiting for an admin to approve them.
    # Dashboard surfaces this as a "你有 N 个待审核账号" badge.
    pending_user_count: int = 0
    storage: StorageBreakdown


def _storage_breakdown() -> StorageBreakdown:
    root = get_settings().storage_dir.resolve()
    sections = {"inputs": (0, 0), "outputs": (0, 0), "releases": (0, 0)}
    for name in sections:
        sub = root / name
        if not sub.is_dir():
            continue
        total_bytes = 0
        total_files = 0
        for dirpath, _, files in os.walk(sub):
            for f in files:
                try:
                    total_bytes += (os.path.getsize(os.path.join(dirpath, f)))
                    total_files += 1
                except OSError:
                    pass
        sections[name] = (total_bytes, total_files)
    return StorageBreakdown(
        inputs_bytes=sections["inputs"][0],
        inputs_files=sections["inputs"][1],
        outputs_bytes=sections["outputs"][0],
        outputs_files=sections["outputs"][1],
        releases_bytes=sections["releases"][0],
        releases_files=sections["releases"][1],
    )


@router.get("/stats", response_model=StatsOut)
def stats(_: AdminUser, db: Annotated[Session, Depends(get_db)]) -> StatsOut:
    # Histogram of task statuses in one round trip.
    rows = db.execute(
        select(Task.status, func.count(Task.id)).group_by(Task.status)
    ).all()
    counts: dict[str, int] = {row[0]: int(row[1]) for row in rows}

    queue_depth = counts.get(TASK_PENDING, 0)
    in_flight = counts.get(TASK_CLAIMED, 0) + counts.get(TASK_RUNNING, 0)

    cutoff = utcnow() - timedelta(hours=24)
    succeeded_24h = db.execute(
        select(func.count(Task.id)).where(
            Task.status == TASK_SUCCEEDED,
            Task.completed_at.is_not(None),
            Task.completed_at >= cutoff,
        )
    ).scalar_one()
    failed_24h = db.execute(
        select(func.count(Task.id)).where(
            Task.status == TASK_FAILED,
            Task.completed_at.is_not(None),
            Task.completed_at >= cutoff,
        )
    ).scalar_one()

    role_rows = db.execute(
        select(
            func.count(case((User.role == "admin", 1))),
            func.count(case((User.role == "user", 1))),
        )
    ).one()

    pending_user_count = db.execute(
        select(func.count(User.id)).where(User.is_approved.is_(False))
    ).scalar_one()

    return StatsOut(
        task_counts=counts,
        queue_depth=queue_depth,
        in_flight=in_flight,
        succeeded_24h=int(succeeded_24h or 0),
        failed_24h=int(failed_24h or 0),
        total_admin_count=int(role_rows[0] or 0),
        total_user_count=int(role_rows[1] or 0),
        pending_user_count=int(pending_user_count or 0),
        storage=_storage_breakdown(),
    )
