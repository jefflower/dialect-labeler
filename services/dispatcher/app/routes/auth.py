"""Auth routes — register, login, me. Logout is client-side (drop the token)."""

from __future__ import annotations

import threading
import time
from collections import deque
from typing import Annotated, Deque

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
from ..schemas import LoginIn, RegisterIn, RegisterPendingOut, TokenOut, UserOut

router = APIRouter(prefix="/api/auth", tags=["auth"])


# Per-email login rate limit. 10 attempts within a 60-second sliding
# window is loose enough that a fat-fingering user won't hit it,
# strict enough that an online dictionary attack against a single
# account is futile. In-memory only — if the process restarts, the
# attacker also gets to retry, which is fine: the bcrypt cost is the
# real defense.
_LOGIN_WINDOW_SECONDS = 60.0
_LOGIN_MAX_ATTEMPTS = 10
_login_attempts: dict[str, Deque[float]] = {}
_login_lock = threading.Lock()


def _check_login_rate_limit(email: str) -> None:
    """Raise 429 if `email` has tried to log in too many times recently."""
    now = time.monotonic()
    with _login_lock:
        bucket = _login_attempts.setdefault(email, deque())
        # Drop attempts older than the window.
        while bucket and now - bucket[0] > _LOGIN_WINDOW_SECONDS:
            bucket.popleft()
        if len(bucket) >= _LOGIN_MAX_ATTEMPTS:
            retry_after = int(_LOGIN_WINDOW_SECONDS - (now - bucket[0])) + 1
            raise HTTPException(
                status_code=status.HTTP_429_TOO_MANY_REQUESTS,
                detail=f"Too many login attempts. Retry in {retry_after}s.",
                headers={"Retry-After": str(retry_after)},
            )
        bucket.append(now)


def _reset_login_rate_limit(email: str) -> None:
    with _login_lock:
        _login_attempts.pop(email, None)


@router.post(
    "/register",
    response_model=RegisterPendingOut | TokenOut,
    status_code=status.HTTP_201_CREATED,
)
def register(
    payload: RegisterIn,
    db: Annotated[Session, Depends(get_db)],
) -> RegisterPendingOut | TokenOut:
    """Self-service signup with admin-approval gate.

    Three paths:
      - **Bootstrap** (users table empty): first registrant becomes admin
        AND is auto-approved. Returns TokenOut — they can use the system
        immediately.
      - **Self-signup with open registration on** (settings.allow_open_registration):
        account is created with `is_approved=False` and returns
        RegisterPendingOut. The user CANNOT log in until an admin flips
        `is_approved` true via POST /api/users/{id}/approve.
      - **Open registration off**: HTTP 403; admin must use /api/users.

    Why a pending state instead of straight 201 with token? Because we
    want admins to vet new users before they consume worker quota or
    upload arbitrary input. Issuing a token only on approval makes the
    UX flow obvious: register → wait → admin approves → log in.
    """
    settings = get_settings()
    existing_count = db.execute(select(User.id).limit(1)).scalar_one_or_none()

    is_bootstrap = existing_count is None
    if is_bootstrap:
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

    # Bootstrap admin is implicitly approved (no one's around to approve
    # them anyway). Everyone else lands in the pending queue.
    is_approved = is_bootstrap
    approved_at = utcnow() if is_bootstrap else None

    user = User(
        email=payload.email,
        password_hash=hash_password(payload.password),
        role=role,
        is_approved=is_approved,
        approved_at=approved_at,
    )
    db.add(user)
    db.commit()
    db.refresh(user)

    if is_bootstrap:
        # First registrant is the bootstrap admin: hand them a token
        # immediately so they don't get stuck on "waiting for admin"
        # forever.
        token = create_access_token(user.id)
        return TokenOut(access_token=token, user=UserOut.model_validate(user))

    return RegisterPendingOut(user=UserOut.model_validate(user))


@router.post("/login", response_model=TokenOut)
def login(payload: LoginIn, db: Annotated[Session, Depends(get_db)]) -> TokenOut:
    _check_login_rate_limit(payload.email)
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

    # Admin-approval gate. Pending users get a 403 with a clear message
    # so the SPA can render a "waiting for approval" state without
    # ambiguity. We DON'T merge this with the 401 above — distinguishing
    # "wrong credentials" from "credentials valid but account pending"
    # is the entire point of the feature.
    if not user.is_approved:
        raise HTTPException(
            status_code=status.HTTP_403_FORBIDDEN,
            detail="账号正在等待管理员审核，审核通过后即可登录。",
        )

    user.last_login_at = utcnow()
    db.commit()
    db.refresh(user)

    # Successful login flushes the bucket — a single typo doesn't burn a
    # legit user's retry budget for the next time they actually forget.
    _reset_login_rate_limit(payload.email)
    token = create_access_token(user.id)
    return TokenOut(access_token=token, user=UserOut.model_validate(user))


@router.get("/me", response_model=UserOut)
def me(user: CurrentUser) -> UserOut:
    return UserOut.model_validate(user)
