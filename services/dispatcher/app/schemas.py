"""Pydantic request/response models.

Keep these on the boundary only — internal code passes ORM objects.
"""

from __future__ import annotations

import re
from datetime import datetime
from typing import Any

from pydantic import BaseModel, Field, field_validator

# Chinese mainland mobile prefix: 1[3-9]xxxxxxxxx, 11 digits total.
# Anything starting with 12 or other special codes is filtered out at
# the regex level — they're virtual / service numbers that don't
# correspond to a person who can log in.
MOBILE_RE = re.compile(r"^1[3-9]\d{9}$")


class UserOut(BaseModel):
    id: int
    # The unified credential field — see `models.User.identifier` for
    # the format rules. We expose it on the wire as `identifier` so
    # the SPA can stop pretending all accounts are emails.
    identifier: str
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
    """Self-service signup: 11-digit Chinese mobile number only.

    No email path on this endpoint — admins create email/username
    accounts directly via `/api/users`. Keeping self-registration to
    a single canonical format means we don't have to debate which
    "looks like a phone number" rules apply across regions; if you
    need an internal account, ask an admin.
    """

    phone: str = Field(min_length=11, max_length=11, description="11-digit mobile number")
    password: str = Field(min_length=8, max_length=128)

    @field_validator("phone")
    @classmethod
    def _validate_phone(cls, v: str) -> str:
        if not MOBILE_RE.match(v):
            raise ValueError("手机号格式不正确，应为 11 位 1 开头的数字")
        return v


class LoginIn(BaseModel):
    """Login takes any string as the identifier — a mobile number, a
    username like `admin`, or a legacy email address. Validation
    against the DB is exact-match, so wrong-shape inputs just produce
    the standard 401 (no information leak)."""

    identifier: str = Field(min_length=1, max_length=254)
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
