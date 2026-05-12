"""Auth routes — register, login, me. Logout is client-side (drop the token)."""

from __future__ import annotations

from typing import Annotated

from fastapi import APIRouter, Depends, HTTPException, status
from sqlalchemy import select
from sqlalchemy.orm import Session

from ..auth import (
    CurrentUser,
    create_access_token,
    hash_password,
    verify_password,
)
from ..config import get_settings
from ..db import get_db
from ..models import ROLE_ADMIN, ROLE_USER, User, utcnow
from ..schemas import LoginIn, RegisterIn, TokenOut, UserOut

router = APIRouter(prefix="/api/auth", tags=["auth"])


@router.post("/register", response_model=TokenOut, status_code=status.HTTP_201_CREATED)
def register(payload: RegisterIn, db: Annotated[Session, Depends(get_db)]) -> TokenOut:
    """Self-service signup.

    Allowed when EITHER:
      - the users table is empty (bootstrap: first registrant becomes admin), OR
      - settings.allow_open_registration is true.

    Otherwise an admin must create the user via /api/users.
    """
    settings = get_settings()
    existing_count = db.execute(select(User.id).limit(1)).scalar_one_or_none()

    if existing_count is None:
        role = ROLE_ADMIN
    elif settings.allow_open_registration:
        role = ROLE_USER
    else:
        raise HTTPException(
            status_code=status.HTTP_403_FORBIDDEN,
            detail="Open registration is disabled. Ask an admin to create the account.",
        )

    if db.execute(select(User).where(User.email == payload.email)).scalar_one_or_none():
        raise HTTPException(
            status_code=status.HTTP_409_CONFLICT, detail="Email already registered"
        )

    user = User(
        email=payload.email,
        password_hash=hash_password(payload.password),
        role=role,
    )
    db.add(user)
    db.commit()
    db.refresh(user)

    token = create_access_token(user.id)
    return TokenOut(access_token=token, user=UserOut.model_validate(user))


@router.post("/login", response_model=TokenOut)
def login(payload: LoginIn, db: Annotated[Session, Depends(get_db)]) -> TokenOut:
    user = db.execute(
        select(User).where(User.email == payload.email)
    ).scalar_one_or_none()
    if user is None or not verify_password(payload.password, user.password_hash):
        # Same error on missing-user vs bad-password: prevents email
        # enumeration on a public endpoint.
        raise HTTPException(
            status_code=status.HTTP_401_UNAUTHORIZED,
            detail="Invalid email or password",
        )

    user.last_login_at = utcnow()
    db.commit()
    db.refresh(user)

    token = create_access_token(user.id)
    return TokenOut(access_token=token, user=UserOut.model_validate(user))


@router.get("/me", response_model=UserOut)
def me(user: CurrentUser) -> UserOut:
    return UserOut.model_validate(user)
