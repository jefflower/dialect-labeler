"""Admin user management.

Self-service registration (/api/auth/register) is what an unprivileged
user goes through. This module is for an admin to create accounts on
someone else's behalf, demote/promote roles, and remove users.

All endpoints sit behind `require_admin`. The admin cannot delete or
demote themselves — that's a footgun guard, not a true safety property,
but it stops "I accidentally locked everyone out" in the easy case.
"""

from __future__ import annotations

from typing import Annotated

from fastapi import APIRouter, Depends, HTTPException, status
from pydantic import BaseModel, Field
from sqlalchemy import select
from sqlalchemy.orm import Session

from ..auth import AdminUser, hash_password
from ..db import get_db
from ..models import ROLE_ADMIN, ROLE_USER, User, utcnow
from ..schemas import UserOut

router = APIRouter(prefix="/api/users", tags=["users"])


class UserCreateIn(BaseModel):
    # Admin-tier endpoint, so we let the admin pick any identifier
    # string — phone, email, plain username like "admin" or
    # "worker-bot", whatever. Self-service signup is still locked to
    # phone numbers (see schemas.RegisterIn), but admin-created
    # accounts exist for service bots and named operators.
    identifier: str = Field(min_length=1, max_length=254)
    password: str = Field(min_length=8, max_length=128)
    role: str = ROLE_USER


class RoleUpdateIn(BaseModel):
    role: str


@router.get("", response_model=list[UserOut])
def list_users(
    _: AdminUser,
    db: Annotated[Session, Depends(get_db)],
) -> list[UserOut]:
    # Sort pending accounts first so the admin's queue is at the top of
    # the list — `is_approved=False` rows sort ahead of `is_approved=True`.
    # Within each group, oldest-first to keep approval order fair.
    rows = (
        db.execute(
            select(User).order_by(User.is_approved.asc(), User.created_at.asc())
        )
        .scalars()
        .all()
    )
    return [UserOut.model_validate(u) for u in rows]


@router.post("", response_model=UserOut, status_code=status.HTTP_201_CREATED)
def create_user(
    payload: UserCreateIn,
    admin: AdminUser,
    db: Annotated[Session, Depends(get_db)],
) -> UserOut:
    if payload.role not in (ROLE_ADMIN, ROLE_USER):
        raise HTTPException(
            status_code=status.HTTP_400_BAD_REQUEST,
            detail=f"role must be {ROLE_ADMIN} or {ROLE_USER}",
        )
    if db.execute(
        select(User).where(User.identifier == payload.identifier)
    ).scalar_one_or_none():
        raise HTTPException(
            status_code=status.HTTP_409_CONFLICT,
            detail="该账号已被占用",
        )
    # Admin-created accounts are pre-approved by definition — an admin
    # creating a user IS the approval. Skip the pending state so they
    # can log in immediately.
    user = User(
        identifier=payload.identifier,
        password_hash=hash_password(payload.password),
        role=payload.role,
        is_approved=True,
        approved_at=utcnow(),
        approved_by_id=admin.id,
    )
    db.add(user)
    db.commit()
    db.refresh(user)
    return UserOut.model_validate(user)


@router.post("/{user_id}/approve", response_model=UserOut)
def approve_user(
    user_id: int,
    admin: AdminUser,
    db: Annotated[Session, Depends(get_db)],
) -> UserOut:
    """Admin flips a pending account to approved so the user can log in.

    Idempotent: approving an already-approved user is a no-op (returns
    200 with the current row). Returning 200 instead of 409 keeps the
    SPA's "approve" button forgiving — double-clicking it doesn't
    surface a scary error.
    """
    user = db.get(User, user_id)
    if user is None:
        raise HTTPException(status_code=status.HTTP_404_NOT_FOUND, detail="User not found")
    if not user.is_approved:
        user.is_approved = True
        user.approved_at = utcnow()
        user.approved_by_id = admin.id
        db.commit()
        db.refresh(user)
    return UserOut.model_validate(user)


@router.patch("/{user_id}", response_model=UserOut)
def update_role(
    user_id: int,
    payload: RoleUpdateIn,
    admin: AdminUser,
    db: Annotated[Session, Depends(get_db)],
) -> UserOut:
    if payload.role not in (ROLE_ADMIN, ROLE_USER):
        raise HTTPException(
            status_code=status.HTTP_400_BAD_REQUEST,
            detail=f"role must be {ROLE_ADMIN} or {ROLE_USER}",
        )
    if user_id == admin.id and payload.role != ROLE_ADMIN:
        raise HTTPException(
            status_code=status.HTTP_400_BAD_REQUEST,
            detail="Refusing to demote yourself — ask another admin to do it",
        )
    user = db.get(User, user_id)
    if user is None:
        raise HTTPException(status_code=status.HTTP_404_NOT_FOUND, detail="User not found")
    user.role = payload.role
    db.commit()
    db.refresh(user)
    return UserOut.model_validate(user)


@router.delete("/{user_id}", status_code=status.HTTP_204_NO_CONTENT)
def delete_user(
    user_id: int,
    admin: AdminUser,
    db: Annotated[Session, Depends(get_db)],
) -> None:
    if user_id == admin.id:
        raise HTTPException(
            status_code=status.HTTP_400_BAD_REQUEST,
            detail="Refusing to delete yourself — ask another admin to do it",
        )
    user = db.get(User, user_id)
    if user is None:
        raise HTTPException(status_code=status.HTTP_404_NOT_FOUND, detail="User not found")
    db.delete(user)
    db.commit()
