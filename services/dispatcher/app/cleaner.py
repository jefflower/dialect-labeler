"""Periodic file cleanup.

DB rows are forever. Files are NOT. This module is the only place that
deletes blobs off disk, so the policy lives in one place:

  - succeeded task:
      output deleted when downloaded OR `output_ready_at + OUTPUT_TTL` passes
      input deleted as soon as the task succeeds (it has done its job)
  - failed task:
      input kept for `FAILED_INPUT_TTL_HOURS` so an operator can retry,
      then deleted
  - claimed/running task with expired lease:
      released by `queue_ops.release_expired_claims` — under the attempt
      cap they reset to `pending`; at the cap they promote to `expired`.
  - expired task:
      input kept for the same `FAILED_INPUT_TTL_HOURS` window, then deleted.

After deletion, `files_cleaned_at` is set and `*_path` is nulled. The
file size and timestamp columns stay populated as a historical record.
"""

from __future__ import annotations

import logging
from datetime import datetime, timedelta
from typing import Iterable

from sqlalchemy import or_, select
from sqlalchemy.orm import Session

from .config import get_settings
from .db import session_scope
from .models import (
    TASK_EXPIRED,
    TASK_FAILED,
    TASK_SUCCEEDED,
    Task,
    utcnow,
)
from .queue_ops import release_expired_claims
from .storage import Storage, get_storage

log = logging.getLogger("dispatcher.cleaner")


def _now() -> datetime:
    return utcnow()


def _candidate_succeeded(db: Session, now: datetime, output_ttl: timedelta) -> Iterable[Task]:
    """succeeded tasks whose output is either downloaded or aged out."""
    threshold = now - output_ttl
    return db.execute(
        select(Task).where(
            Task.status == TASK_SUCCEEDED,
            or_(
                Task.output_downloaded_at.is_not(None),
                Task.output_ready_at.is_not(None) & (Task.output_ready_at < threshold),
            ),
            # Either file still on disk → otherwise nothing to do.
            or_(Task.input_path.is_not(None), Task.output_path.is_not(None)),
        )
    ).scalars()


def _candidate_failed_or_expired(
    db: Session, now: datetime, failed_ttl: timedelta
) -> Iterable[Task]:
    """failed/expired tasks whose input is past the retention window."""
    threshold = now - failed_ttl
    return db.execute(
        select(Task).where(
            Task.status.in_([TASK_FAILED, TASK_EXPIRED]),
            Task.input_path.is_not(None),
            Task.updated_at < threshold,
        )
    ).scalars()


def run_cleanup_pass(storage: Storage | None = None) -> dict[str, int]:
    """One sweep of the cleanup policy. Safe to call from a scheduler
    job or directly from a test. Returns counts for observability."""
    settings = get_settings()
    storage = storage or get_storage()
    output_ttl = timedelta(hours=settings.output_ttl_hours)
    failed_ttl = timedelta(hours=settings.failed_input_ttl_hours)

    counts = {
        "output_cleaned": 0,
        "input_cleaned": 0,
        "leases_recovered": 0,
        "leases_expired": 0,
    }

    with session_scope() as db:
        released = release_expired_claims(
            db, now=_now(), max_attempts=settings.max_attempts
        )
        counts["leases_recovered"] = released["recovered"]
        counts["leases_expired"] = released["expired"]

    with session_scope() as db:
        now = _now()
        for task in _candidate_succeeded(db, now, output_ttl):
            if task.output_path:
                storage.unlink(task.output_path)
                task.output_path = None
                counts["output_cleaned"] += 1
            if task.input_path:
                storage.unlink(task.input_path)
                task.input_path = None
                counts["input_cleaned"] += 1
            task.files_cleaned_at = now

    with session_scope() as db:
        now = _now()
        for task in _candidate_failed_or_expired(db, now, failed_ttl):
            if task.input_path:
                storage.unlink(task.input_path)
                task.input_path = None
                counts["input_cleaned"] += 1
            task.files_cleaned_at = now

    if any(counts.values()):
        log.info("cleanup pass: %s", counts)
    return counts


def schedule_cleaner():
    """Start the cleaner on a BackgroundScheduler. Returns the scheduler
    so the caller can shut it down on app teardown."""
    from apscheduler.schedulers.background import BackgroundScheduler
    from apscheduler.triggers.interval import IntervalTrigger

    settings = get_settings()
    scheduler = BackgroundScheduler(timezone="UTC")
    scheduler.add_job(
        run_cleanup_pass,
        trigger=IntervalTrigger(seconds=settings.clean_interval_seconds),
        id="dispatcher.cleanup",
        max_instances=1,
        coalesce=True,
        next_run_time=_now(),  # fire once at startup
    )
    scheduler.start()
    log.info(
        "cleanup scheduled every %ds; output_ttl=%.2fh failed_input_ttl=%.2fh",
        settings.clean_interval_seconds,
        settings.output_ttl_hours,
        settings.failed_input_ttl_hours,
    )
    return scheduler
