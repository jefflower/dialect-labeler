"""Public (un-authenticated) read-only endpoints.

Used by the LoginPage's marquee strip to show "system is alive" stats
to anyone who lands on the page — no login required, no PII surfaced,
just aggregate counters that already show up to anyone who can
self-register.

We deliberately keep this module thin:
  - No PII (email, names, IPs).
  - No per-task detail (id, owner, error messages).
  - Only counters and aggregates.
Anything more sensitive belongs behind `require_admin` in admin.py.
"""

from __future__ import annotations

import json
from datetime import timedelta
from typing import Annotated

from fastapi import APIRouter, Depends
from pydantic import BaseModel
from sqlalchemy import func, select
from sqlalchemy.orm import Session

from ..db import get_db
from ..models import (
    TASK_CLOSED,
    TASK_SUCCEEDED,
    Task,
    User,
    utcnow,
)

router = APIRouter(prefix="/api/public", tags=["public"])


class PublicStatsOut(BaseModel):
    """The shape the LoginPage's stat strip consumes.

    All fields are safe to expose pre-auth — they're the kind of
    number you'd put on a landing page anyway ("we've processed N
    files / Y hours of audio"). Per-user, per-task detail stays
    behind `/api/admin/stats`.
    """

    # Total registered users that have been admin-approved. Doubles as
    # a "active operators" hint on the login page.
    active_user_count: int
    # Total tasks the dispatcher has *ever* successfully processed
    # (succeeded + closed both count — closed means succeeded then the
    # user explicitly cleaned up).
    completed_task_count: int
    # Total seconds of audio across all completed task summaries that
    # bothered to record a duration_by_role breakdown. Mode 1 emits
    # this; Mode 2 doesn't yet (its summary uses segment_count, not
    # duration), so this is a lower bound on real throughput.
    processed_audio_seconds: int
    # Successful task completions in the last 24h. A "system is alive
    # right now" signal vs the all-time counters above.
    completed_last_24h: int


@router.get("/stats", response_model=PublicStatsOut)
def public_stats(db: Annotated[Session, Depends(get_db)]) -> PublicStatsOut:
    """Cheap aggregate stats. No auth required.

    Implementation note: this is a couple of `COUNT(*)`s and one
    summary_json walk. The walk is bounded by completed task count —
    for the foreseeable future we have under 10K of those, so a full
    scan + `json.loads` on each row is fine. Switch to a denormalised
    `duration_seconds` column on Task if that assumption ever breaks.
    """
    active_user_count = (
        db.execute(
            select(func.count(User.id)).where(User.is_approved.is_(True))
        ).scalar_one()
        or 0
    )

    completed_task_count = (
        db.execute(
            select(func.count(Task.id)).where(
                Task.status.in_((TASK_SUCCEEDED, TASK_CLOSED))
            )
        ).scalar_one()
        or 0
    )

    cutoff = utcnow() - timedelta(hours=24)
    completed_last_24h = (
        db.execute(
            select(func.count(Task.id)).where(
                Task.status.in_((TASK_SUCCEEDED, TASK_CLOSED)),
                Task.completed_at.is_not(None),
                Task.completed_at >= cutoff,
            )
        ).scalar_one()
        or 0
    )

    # Walk summary_json on completed tasks for duration_by_role totals.
    # Mode 1 emits {"duration_by_role": {"role_a": <ms>, ...}}; Mode 2
    # currently doesn't, so this undercounts. That's OK for a public
    # statistic — we'd rather under-report than expose more detail.
    summary_rows = (
        db.execute(
            select(Task.summary_json).where(
                Task.status.in_((TASK_SUCCEEDED, TASK_CLOSED)),
                Task.summary_json.is_not(None),
            )
        )
        .scalars()
        .all()
    )
    total_ms = 0
    for raw in summary_rows:
        if not raw:
            continue
        try:
            data = json.loads(raw)
        except (TypeError, ValueError):
            continue
        by_role = data.get("duration_by_role") if isinstance(data, dict) else None
        if isinstance(by_role, dict):
            for v in by_role.values():
                if isinstance(v, (int, float)) and v > 0:
                    total_ms += int(v)

    return PublicStatsOut(
        active_user_count=int(active_user_count),
        completed_task_count=int(completed_task_count),
        processed_audio_seconds=total_ms // 1000,
        completed_last_24h=int(completed_last_24h),
    )
