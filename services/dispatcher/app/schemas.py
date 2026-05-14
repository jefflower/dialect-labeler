"""Pydantic request/response models.

Keep these on the boundary only — internal code passes ORM objects.
"""

from __future__ import annotations

from datetime import datetime
from typing import Any

from pydantic import BaseModel, EmailStr, Field


class UserOut(BaseModel):
    id: int
    email: EmailStr
    role: str
    # Admin-approval gate. New self-registered users start unapproved
    # (is_approved=False) and a freshly-issued JWT for them won't pass
    # the login endpoint — the admin must flip this true first.
    # Defaults to True for backwards compatibility with old clients
    # that don't surface the field; existing rows are grandfathered
    # to True by the inline migration.
    is_approved: bool = True
    approved_at: datetime | None = None
    approved_by_id: int | None = None
    created_at: datetime
    last_login_at: datetime | None = None

    model_config = {"from_attributes": True}


class RegisterPendingOut(BaseModel):
    """Response when a self-service registration succeeded but is
    waiting for admin approval. No token issued — the user must come
    back after the admin signs off."""

    status: str = "pending_approval"
    detail: str = "账号已创建，等待管理员审核通过后再登录。"
    user: UserOut


class RegisterIn(BaseModel):
    email: EmailStr
    password: str = Field(min_length=8, max_length=128)


class LoginIn(BaseModel):
    email: EmailStr
    password: str


class TokenOut(BaseModel):
    access_token: str
    token_type: str = "bearer"
    user: UserOut


class TaskOut(BaseModel):
    id: str
    owner_id: int
    name: str
    status: str
    # Cut mode chosen at upload time. `dialect` = Mode 1 (silence-based),
    # `semantic` = Mode 2 (Mandarin LLM-driven). Stored on the row so the
    # Worker can authoritatively translate it into CutConfig.mode.
    mode: str = "dialect"
    input_size: int | None = None
    input_uploaded_at: datetime | None = None
    output_size: int | None = None
    output_ready_at: datetime | None = None
    output_downloaded_at: datetime | None = None
    files_cleaned_at: datetime | None = None
    error: str | None = None
    summary: dict | None = None
    # Worker-pushed progress snapshot. Cleared (NULL) until the Worker
    # starts pushing updates. UI renders a progress bar when
    # `progress_percent` is non-null.
    progress_percent: int | None = None
    progress_stage: str | None = None
    progress_detail: str | None = None
    progress_updated_at: datetime | None = None
    created_at: datetime
    updated_at: datetime
    completed_at: datetime | None = None
    # Worker visibility — populated while a Worker holds the lease.
    # `claimer_email` resolves the FK on the way out so the UI can
    # show "being processed by X" without an extra fetch.
    claimer_email: str | None = None
    claim_expires_at: datetime | None = None

    model_config = {"from_attributes": True}


class TaskClaim(BaseModel):
    """Returned to a Worker after a successful /api/worker/claim."""

    id: str
    name: str
    owner_id: int
    input_size: int | None
    claim_expires_at: datetime
    # Mode the owner picked at upload time. The Worker MUST honour this
    # value when constructing CutConfig — it overrides whatever's in the
    # uploaded bundle's project.json. Always populated on a fresh claim.
    mode: str = "dialect"


class TaskCompleteIn(BaseModel):
    """Worker tells the dispatcher how the task went. `summary` payload
    is what survives after the cleaner deletes the output zip — keep it
    descriptive."""

    summary: dict[str, Any] = Field(default_factory=dict)


class TaskFailIn(BaseModel):
    error: str = Field(max_length=4000)


class TaskProgressIn(BaseModel):
    """Worker pushes a snapshot of where it is in the pipeline. Best-effort
    UI hint — losing one of these doesn't fail the task. Validation is
    permissive on purpose: a worker that emits a malformed update should
    log + drop, not crash.
    """

    percent: int = Field(ge=0, le=100)
    stage: str = Field(max_length=64)
    detail: str | None = Field(default=None, max_length=255)
