"""ORM models.

The cleanup story is encoded in column nullability:
  - `Task.input_path` / `output_path` go NULL after the cleaner unlinks
    the actual file. The row itself sticks around forever.
  - `Task.summary_json` carries the post-completion stats so the UI can
    still answer "how much did this task produce?" after files are gone.
"""

from __future__ import annotations

from datetime import datetime, timezone

from sqlalchemy import (
    BigInteger,
    DateTime,
    ForeignKey,
    Integer,
    String,
    Text,
)
from sqlalchemy.orm import Mapped, mapped_column, relationship

from .db import Base


def utcnow() -> datetime:
    """Single source of truth for "now" — naive UTC.

    SQLite has no native timezone support; SQLAlchemy returns naive
    datetimes on read regardless of `DateTime(timezone=True)`. Storing
    naive UTC consistently makes comparisons between freshly-built
    timestamps and DB-roundtripped ones work without TypeError.
    """
    return datetime.now(timezone.utc).replace(tzinfo=None)


# Task status values. Stored as plain strings so a migration can rename
# them without an ALTER TABLE on the Enum type.
TASK_PENDING = "pending"
TASK_CLAIMED = "claimed"
TASK_RUNNING = "running"
TASK_SUCCEEDED = "succeeded"
TASK_FAILED = "failed"
TASK_EXPIRED = "expired"
# User-explicit terminal state. The owner downloaded the output zip,
# then called POST /api/tasks/{id}/close which deletes both input and
# output files immediately and frees the owner to upload a new task.
# Distinct from `succeeded`: a succeeded task still holds disk; a closed
# task is metadata-only.
TASK_CLOSED = "closed"

# Per-owner single-task gating. A user may have at most one "active"
# task in the pipeline at a time. We define active narrowly: any state
# where the task is still consuming a worker slot or holding a
# downloadable output the user hasn't acted on yet.
ACTIVE_TASK_STATES = (
    TASK_PENDING,
    TASK_CLAIMED,
    TASK_RUNNING,
    TASK_SUCCEEDED,
)

# Cut mode values. Mirrors `CutMode` on the Tauri client. Stored as
# plain strings on the Task row so the Worker can blindly translate
# `task.mode` into `CutConfig.mode` without a lookup table. The mode
# is committed at upload time (POST /api/tasks) — the Worker IGNORES
# whatever `mode` is buried in the uploaded bundle's project.json. This
# avoids the silent-mode-mismatch class of bugs where a Windows
# uploader and a Mac Worker disagreed on what the user actually wanted.
MODE_DIALECT = "dialect"  # Mode 1: silence-based cutter (default)
MODE_SEMANTIC = "semantic"  # Mode 2: LLM-driven Mandarin semantic cutter
VALID_MODES = (MODE_DIALECT, MODE_SEMANTIC)

# Roles.
ROLE_ADMIN = "admin"
ROLE_USER = "user"


class User(Base):
    __tablename__ = "users"

    id: Mapped[int] = mapped_column(Integer, primary_key=True)
    email: Mapped[str] = mapped_column(String(254), unique=True, nullable=False)
    password_hash: Mapped[str] = mapped_column(String(255), nullable=False)
    role: Mapped[str] = mapped_column(String(16), nullable=False, default=ROLE_USER)
    created_at: Mapped[datetime] = mapped_column(
        DateTime(timezone=True), default=utcnow, nullable=False
    )
    last_login_at: Mapped[datetime | None] = mapped_column(
        DateTime(timezone=True), nullable=True
    )

    tasks: Mapped[list["Task"]] = relationship(
        "Task", back_populates="owner", foreign_keys="Task.owner_id"
    )

    @property
    def is_admin(self) -> bool:
        return self.role == ROLE_ADMIN


class Task(Base):
    __tablename__ = "tasks"

    # UUID hex as text — SQLite has no native UUID; keeping it as a string
    # makes joins and grep-the-DB trivial.
    id: Mapped[str] = mapped_column(String(32), primary_key=True)
    owner_id: Mapped[int] = mapped_column(
        ForeignKey("users.id", ondelete="CASCADE"), nullable=False, index=True
    )
    name: Mapped[str] = mapped_column(String(255), nullable=False)
    status: Mapped[str] = mapped_column(
        String(16), nullable=False, default=TASK_PENDING, index=True
    )

    claimed_by: Mapped[int | None] = mapped_column(
        ForeignKey("users.id", ondelete="SET NULL"), nullable=True
    )
    claim_expires_at: Mapped[datetime | None] = mapped_column(
        DateTime(timezone=True), nullable=True
    )
    # Total times the task has been claimed by any worker. Incremented
    # atomically on each successful claim. When it exceeds MAX_ATTEMPTS
    # and the lease expires again, the cleaner promotes the task to
    # `TASK_EXPIRED` instead of resetting to pending — stops a poison
    # input from looping a worker pool indefinitely.
    attempts: Mapped[int] = mapped_column(
        Integer, nullable=False, default=0, server_default="0"
    )

    # Cut mode committed at upload time. See `MODE_*` constants above.
    # Defaults to `dialect` so old rows created before this column was
    # added round-trip cleanly.
    mode: Mapped[str] = mapped_column(
        String(16),
        nullable=False,
        default=MODE_DIALECT,
        server_default=MODE_DIALECT,
    )

    # Worker-pushed progress snapshot. Worker calls
    # POST /api/worker/tasks/{id}/progress at stage transitions and
    # roughly every 5s within long stages. Owner polling GET /api/tasks
    # then sees the latest values without needing realtime channels.
    # `progress_percent` is 0..100, `progress_stage` is a short label
    # (e.g. "ASR" / "切割" / "LLM 改写"), `progress_detail` is a free-form
    # one-liner (e.g. "12 / 30 files · 38%"). All cleared on retry.
    progress_percent: Mapped[int | None] = mapped_column(
        Integer, nullable=True
    )
    progress_stage: Mapped[str | None] = mapped_column(
        String(64), nullable=True
    )
    progress_detail: Mapped[str | None] = mapped_column(
        String(255), nullable=True
    )
    progress_updated_at: Mapped[datetime | None] = mapped_column(
        DateTime(timezone=True), nullable=True
    )

    # Files. Path is relative to STORAGE_DIR. After cleanup these go NULL
    # while {input,output}_size and the timestamps stay populated as a
    # historical record.
    input_path: Mapped[str | None] = mapped_column(String(255), nullable=True)
    input_size: Mapped[int | None] = mapped_column(BigInteger, nullable=True)
    input_uploaded_at: Mapped[datetime | None] = mapped_column(
        DateTime(timezone=True), nullable=True
    )
    output_path: Mapped[str | None] = mapped_column(String(255), nullable=True)
    output_size: Mapped[int | None] = mapped_column(BigInteger, nullable=True)
    output_ready_at: Mapped[datetime | None] = mapped_column(
        DateTime(timezone=True), nullable=True
    )
    output_downloaded_at: Mapped[datetime | None] = mapped_column(
        DateTime(timezone=True), nullable=True
    )
    files_cleaned_at: Mapped[datetime | None] = mapped_column(
        DateTime(timezone=True), nullable=True
    )

    error: Mapped[str | None] = mapped_column(Text, nullable=True)
    # JSON-encoded TaskSummary the Worker sends at /complete. Stays in DB
    # forever; this is the "what did this task produce" report after the
    # files are deleted. SQLite has no native JSON column — using Text
    # plus json.loads/json.dumps at the boundary keeps things portable
    # when we eventually move to Postgres.
    summary_json: Mapped[str | None] = mapped_column(Text, nullable=True)

    created_at: Mapped[datetime] = mapped_column(
        DateTime(timezone=True), default=utcnow, nullable=False, index=True
    )
    updated_at: Mapped[datetime] = mapped_column(
        DateTime(timezone=True), default=utcnow, onupdate=utcnow, nullable=False
    )
    completed_at: Mapped[datetime | None] = mapped_column(
        DateTime(timezone=True), nullable=True
    )

    owner: Mapped[User] = relationship(
        "User", foreign_keys=[owner_id], back_populates="tasks"
    )


class Release(Base):
    __tablename__ = "releases"

    id: Mapped[int] = mapped_column(Integer, primary_key=True)
    version: Mapped[str] = mapped_column(String(64), nullable=False)
    channel: Mapped[str] = mapped_column(String(16), nullable=False, default="stable")
    target: Mapped[str] = mapped_column(String(32), nullable=False)
    installer_path: Mapped[str] = mapped_column(String(255), nullable=False)
    signature: Mapped[str | None] = mapped_column(Text, nullable=True)
    notes: Mapped[str | None] = mapped_column(Text, nullable=True)
    created_at: Mapped[datetime] = mapped_column(
        DateTime(timezone=True), default=utcnow, nullable=False
    )
