"""Client release distribution.

Two faces:

  - **Admin**: list / upload / delete release artifacts via /api/releases.
    Each release is (version, channel, target, installer, signature, notes).
    The signature is whatever `tauri signer sign installer.msi` produced —
    we don't generate or verify it server-side, just hand it back when the
    client asks for the manifest.

  - **Tauri Updater**: GET /api/updater/{target}/{current_version} —
    returns the Tauri 2 updater manifest if a newer stable release exists,
    or 204 No Content if the client is already current. The Tauri config
    points its `endpoints` at this URL with {target} / {current_version}
    placeholders.

  - **Downloads page**: GET /api/releases/latest?target=windows-x86_64 →
    public lightweight metadata + the actual installer at
    /api/releases/{id}/download. No auth required so the install page
    can be linked from anywhere.
"""

from __future__ import annotations

import re
from typing import Annotated

from fastapi import (
    APIRouter,
    Depends,
    File,
    Form,
    HTTPException,
    Response,
    UploadFile,
    status,
)
from fastapi.responses import FileResponse
from pydantic import BaseModel
from sqlalchemy import select
from sqlalchemy.orm import Session

from ..auth import AdminUser
from ..db import get_db
from ..models import Release, utcnow
from ..storage import Storage, get_storage, release_key

router = APIRouter(tags=["releases"])

ALLOWED_TARGETS = {"windows-x86_64", "darwin-x86_64", "darwin-aarch64", "linux-x86_64"}
ALLOWED_CHANNELS = {"stable", "beta"}
SAFE_FILENAME = re.compile(r"^[A-Za-z0-9._-]+$")
# Semantic-ish version: digits.dots, optional -prerelease suffix.
SAFE_VERSION = re.compile(r"^\d+(\.\d+){0,3}(-[A-Za-z0-9.]+)?$")


class ReleaseOut(BaseModel):
    id: int
    version: str
    channel: str
    target: str
    notes: str | None = None
    installer_size: int | None = None
    has_signature: bool = False
    created_at: str

    model_config = {"from_attributes": True}


def _release_to_out(r: Release, storage: Storage) -> ReleaseOut:
    size: int | None = None
    if r.installer_path:
        try:
            size = storage.absolute_path(r.installer_path).stat().st_size
        except FileNotFoundError:
            size = None
    return ReleaseOut(
        id=r.id,
        version=r.version,
        channel=r.channel,
        target=r.target,
        notes=r.notes,
        installer_size=size,
        has_signature=bool(r.signature),
        created_at=r.created_at.isoformat(),
    )


def _version_tuple(v: str) -> tuple[int, ...]:
    """Convert "0.1.10" → (0, 1, 10). Prerelease suffixes are ignored
    so "0.1.0-beta" compares equal to "0.1.0"; good enough for the
    "is there something newer" check."""
    head = v.split("-", 1)[0]
    parts = head.split(".")
    out: list[int] = []
    for p in parts:
        try:
            out.append(int(p))
        except ValueError:
            break
    return tuple(out)


# ----------- Admin: list / upload / delete --------------------------------


@router.get("/api/releases", response_model=list[ReleaseOut])
def list_releases(
    _: AdminUser,
    db: Annotated[Session, Depends(get_db)],
    storage: Annotated[Storage, Depends(get_storage)],
) -> list[ReleaseOut]:
    rows = db.execute(select(Release).order_by(Release.created_at.desc())).scalars().all()
    return [_release_to_out(r, storage) for r in rows]


@router.post(
    "/api/releases", response_model=ReleaseOut, status_code=status.HTTP_201_CREATED
)
async def upload_release(
    _: AdminUser,
    db: Annotated[Session, Depends(get_db)],
    storage: Annotated[Storage, Depends(get_storage)],
    version: Annotated[str, Form()],
    target: Annotated[str, Form()],
    file: Annotated[UploadFile, File()],
    channel: Annotated[str, Form()] = "stable",
    notes: Annotated[str | None, Form()] = None,
    signature: Annotated[str | None, Form()] = None,
) -> ReleaseOut:
    if not SAFE_VERSION.match(version):
        raise HTTPException(status_code=400, detail=f"Invalid version: {version!r}")
    if target not in ALLOWED_TARGETS:
        raise HTTPException(
            status_code=400,
            detail=f"target must be one of {sorted(ALLOWED_TARGETS)}",
        )
    if channel not in ALLOWED_CHANNELS:
        raise HTTPException(
            status_code=400,
            detail=f"channel must be one of {sorted(ALLOWED_CHANNELS)}",
        )
    if not file.filename:
        raise HTTPException(status_code=400, detail="installer must have a filename")
    if not SAFE_FILENAME.match(file.filename):
        raise HTTPException(
            status_code=400,
            detail="installer filename may only contain letters, digits, dot, underscore, dash",
        )

    if db.execute(
        select(Release).where(
            Release.version == version,
            Release.channel == channel,
            Release.target == target,
        )
    ).scalar_one_or_none():
        raise HTTPException(
            status_code=409,
            detail=f"Release {version} for {target}/{channel} already exists",
        )

    key = release_key(version, target, file.filename)
    size = await storage.save_upload(key, file)
    if size == 0:
        storage.unlink(key)
        raise HTTPException(status_code=400, detail="Empty installer upload")

    release = Release(
        version=version,
        channel=channel,
        target=target,
        installer_path=key,
        signature=signature or None,
        notes=notes or None,
    )
    db.add(release)
    db.commit()
    db.refresh(release)
    return _release_to_out(release, storage)


@router.delete("/api/releases/{release_id}", status_code=status.HTTP_204_NO_CONTENT)
def delete_release(
    release_id: int,
    _: AdminUser,
    db: Annotated[Session, Depends(get_db)],
    storage: Annotated[Storage, Depends(get_storage)],
) -> None:
    r = db.get(Release, release_id)
    if r is None:
        raise HTTPException(status_code=404, detail="Release not found")
    if r.installer_path:
        storage.unlink(r.installer_path)
    db.delete(r)
    db.commit()


# ----------- Public downloads page -----------------------------------------


class LatestOut(BaseModel):
    version: str
    target: str
    channel: str
    notes: str | None
    download_url: str
    installer_size: int | None
    pub_date: str


@router.get("/api/releases/latest", response_model=LatestOut)
def latest_release(
    db: Annotated[Session, Depends(get_db)],
    storage: Annotated[Storage, Depends(get_storage)],
    target: str = "windows-x86_64",
    channel: str = "stable",
) -> LatestOut:
    rows = db.execute(
        select(Release)
        .where(Release.target == target, Release.channel == channel)
        .order_by(Release.created_at.desc())
    ).scalars().all()
    if not rows:
        raise HTTPException(status_code=404, detail="No release available")
    newest = max(rows, key=lambda r: _version_tuple(r.version))
    size: int | None = None
    if newest.installer_path:
        try:
            size = storage.absolute_path(newest.installer_path).stat().st_size
        except FileNotFoundError:
            size = None
    return LatestOut(
        version=newest.version,
        target=newest.target,
        channel=newest.channel,
        notes=newest.notes,
        download_url=f"/api/releases/{newest.id}/download",
        installer_size=size,
        pub_date=newest.created_at.isoformat() + "Z",
    )


@router.get("/api/releases/{release_id}/download")
def download_installer(
    release_id: int,
    db: Annotated[Session, Depends(get_db)],
    storage: Annotated[Storage, Depends(get_storage)],
) -> FileResponse:
    r = db.get(Release, release_id)
    if r is None or not r.installer_path:
        raise HTTPException(status_code=404, detail="Release not found")
    abs_path = storage.absolute_path(r.installer_path)
    return FileResponse(
        path=abs_path,
        filename=abs_path.name,
        media_type="application/octet-stream",
    )


# ----------- Tauri Updater manifest ----------------------------------------


@router.get("/api/updater/{target}/{current_version}")
def updater_manifest(
    target: str,
    current_version: str,
    db: Annotated[Session, Depends(get_db)],
    storage: Annotated[Storage, Depends(get_storage)],
    request_url_root: str | None = None,  # filled by FastAPI request if needed
) -> Response:
    """Return the Tauri 2 updater manifest if `target` has a newer stable
    release than `current_version`, otherwise 204 No Content (Tauri's
    "no update" signal).

    Tauri 2 endpoint URL shape:
        https://example.com/api/updater/{{target}}/{{current_version}}
    """
    if target not in ALLOWED_TARGETS:
        return Response(status_code=204)

    rows = (
        db.execute(
            select(Release)
            .where(Release.target == target, Release.channel == "stable")
            .order_by(Release.created_at.desc())
        )
        .scalars()
        .all()
    )
    if not rows:
        return Response(status_code=204)
    newest = max(rows, key=lambda r: _version_tuple(r.version))

    if _version_tuple(newest.version) <= _version_tuple(current_version):
        return Response(status_code=204)

    if not newest.signature:
        # Tauri refuses to install an unsigned update by design. Surface
        # an explicit 204 here so the client doesn't loop on a fetch that
        # would otherwise return an unverifiable manifest. The admin
        # needs to re-upload the release with --signature.
        return Response(status_code=204)

    body = {
        "version": newest.version,
        "notes": newest.notes or "",
        "pub_date": newest.created_at.isoformat() + "Z",
        "url": f"/api/releases/{newest.id}/download",
        "signature": newest.signature,
    }
    from fastapi.responses import JSONResponse

    return JSONResponse(content=body)
