"""Atomic queue operations.

SQLite doesn't have SKIP LOCKED, but a tight `BEGIN IMMEDIATE` → `SELECT
LIMIT 1` → `UPDATE WHERE id=? AND status='pending'` sequence gives the
same property: the UPDATE returns rowcount=1 for the single winner of
each contended row, rowcount=0 for everyone else. We retry the SELECT
on rowcount=0 because the row we picked may have been claimed by another
worker between our SELECT and UPDATE.
"""

from __future__ import annotations

from datetime import datetime, timedelta, timezone

from sqlalchemy import select, update
from sqlalchemy.orm import Session

from .models import (
    TASK_CLAIMED,
    TASK_EXPIRED,
    TASK_PENDING,
    TASK_RUNNING,
    Task,
    utcnow,
)


def claim_next_pending(
    db: Session, worker_user_id: int, lease_seconds: int, max_attempts: int = 5
) -> Task | None:
    """Atomically claim the oldest pending task for `worker_user_id`.

    Returns the claimed Task or None if the queue is empty. The Task
    returned is fresh-loaded after the UPDATE so its `claim_expires_at`
    and `claimed_by` reflect the just-applied claim.
    """
    expires_at = utcnow() + timedelta(seconds=lease_seconds)

    for _ in range(max_attempts):
        candidate_id = db.execute(
            select(Task.id)
            .where(Task.status == TASK_PENDING)
            .order_by(Task.created_at.asc())
            .limit(1)
        ).scalar_one_or_none()
        if candidate_id is None:
            return None

        result = db.execute(
            update(Task)
            .where(Task.id == candidate_id, Task.status == TASK_PENDING)
            .values(
                status=TASK_CLAIMED,
                claimed_by=worker_user_id,
                claim_expires_at=expires_at,
                # Atomic SQL-side increment so two racing claims can't
                # both observe the same pre-increment value.
                attempts=Task.attempts + 1,
                updated_at=utcnow(),
            )
        )
        db.commit()
        if result.rowcount == 1:
            return db.get(Task, candidate_id)
        # Else: another worker beat us to this row, retry with the next
        # pending one.

    return None


def extend_lease(
    db: Session, task_id: str, worker_user_id: int, lease_seconds: int
) -> bool:
    """Push `claim_expires_at` out by `lease_seconds`. Returns False if
    the task isn't currently claimed by this worker (caller should abort)."""
    new_expiry = utcnow() + timedelta(seconds=lease_seconds)
    result = db.execute(
        update(Task)
        .where(
            Task.id == task_id,
            Task.claimed_by == worker_user_id,
            Task.status.in_([TASK_CLAIMED, TASK_RUNNING]),
        )
        .values(claim_expires_at=new_expiry, updated_at=utcnow())
    )
    db.commit()
    return result.rowcount == 1


def release_expired_claims(
    db: Session, now: datetime | None = None, max_attempts: int = 3
) -> dict[str, int]:
    """Handle tasks whose worker lease ran out.

    Two outcomes per row:
      - Under the retry cap → reset to `pending`, another worker picks
        it up.
      - At or over the cap → promote to `TASK_EXPIRED`. Owner can still
        retry from the UI (which resets attempts), but no worker will
        auto-claim it again.

    Returns ``{"recovered": N, "expired": M}``.
    """
    now = now or utcnow()
    base_filter = (
        Task.status.in_([TASK_CLAIMED, TASK_RUNNING]),
        Task.claim_expires_at.is_not(None),
        Task.claim_expires_at < now,
    )

    expired_result = db.execute(
        update(Task)
        .where(*base_filter, Task.attempts >= max_attempts)
        .values(
            status=TASK_EXPIRED,
            claimed_by=None,
            claim_expires_at=None,
            updated_at=now,
            error="lease expired and exceeded max_attempts",
        )
    )
    recovered_result = db.execute(
        update(Task)
        .where(*base_filter, Task.attempts < max_attempts)
        .values(
            status=TASK_PENDING,
            claimed_by=None,
            claim_expires_at=None,
            updated_at=now,
        )
    )
    db.commit()
    return {
        "recovered": recovered_result.rowcount or 0,
        "expired": expired_result.rowcount or 0,
    }
