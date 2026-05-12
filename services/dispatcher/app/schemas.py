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
    created_at: datetime
    last_login_at: datetime | None = None

    model_config = {"from_attributes": True}


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
    input_size: int | None = None
    input_uploaded_at: datetime | None = None
    output_size: int | None = None
    output_ready_at: datetime | None = None
    output_downloaded_at: datetime | None = None
    files_cleaned_at: datetime | None = None
    error: str | None = None
    summary: dict | None = None
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


class TaskCompleteIn(BaseModel):
    """Worker tells the dispatcher how the task went. `summary` payload
    is what survives after the cleaner deletes the output zip — keep it
    descriptive."""

    summary: dict[str, Any] = Field(default_factory=dict)


class TaskFailIn(BaseModel):
    error: str = Field(max_length=4000)
