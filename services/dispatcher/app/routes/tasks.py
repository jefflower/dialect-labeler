"""User-facing task routes.

  POST /api/tasks                -> create a task (multipart upload)
  GET  /api/tasks                -> list (own; admin sees all)
  GET  /api/tasks/{id}           -> detail
  GET  /api/tasks/{id}/output    -> stream the produced bundle
  DELETE /api/tasks/{id}         -> hard delete (owner or admin)

Downloading the output sets `output_downloaded_at`, which signals the
cleaner to unlink the file on its next pass. We deliberately do NOT
delete inline here: a flaky connection that fails mid-download still
gets a working second attempt.
"""

from __future__ import annotations

import json
import uuid
from typing import Annotated

from fastapi import (
    APIRouter,
    Depends,
    File,
    Form,
    HTTPException,
    UploadFile,
    status,
)
from fastapi.responses import FileResponse
from sqlalchemy import select
from sqlalchemy.orm import Session

from ..auth import CurrentUser
from ..db import get_db
from ..models import (
    TASK_EXPIRED,
    TASK_FAILED,
    TASK_PENDING,
    TASK_SUCCEEDED,
    Task,
    User,
    utcnow,
)
from ..schemas import TaskOut
from ..storage import Storage, get_storage, input_key, output_key

router = APIRouter(prefix="/api/tasks", tags=["tasks"])


def _task_to_out(task: Task, db: Session | None = None) -> TaskOut:
    summary = None
    if task.summary_json:
        try:
            summary = json.loads(task.summary_json)
        except json.JSONDecodeError:
            summary = None
    claimer_email: str | None = None
    if task.claimed_by is not None and db is not None:
        claimer = db.get(User, task.claimed_by)
        if claimer:
            claimer_email = claimer.email
    data = {
        "id": task.id,
        "owner_id": task.owner_id,
        "name": task.name,
        "status": task.status,
        "input_size": task.input_size,
        "input_uploaded_at": task.input_uploaded_at,
        "output_size": task.output_size,
        "output_ready_at": task.output_ready_at,
        "output_downloaded_at": task.output_downloaded_at,
        "files_cleaned_at": task.files_cleaned_at,
        "error": task.error,
        "summary": summary,
        "created_at": task.created_at,
        "updated_at": task.updated_at,
        "completed_at": task.completed_at,
        "claimer_email": claimer_email,
        "claim_expires_at": task.claim_expires_at,
    }
    return TaskOut(**data)


@router.post("", response_model=TaskOut, status_code=status.HTTP_201_CREATED)
async def create_task(
    user: CurrentUser,
    db: Annotated[Session, Depends(get_db)],
    storage: Annotated[Storage, Depends(get_storage)],
    name: Annotated[str, Form(min_length=1, max_length=255)],
    file: Annotated[UploadFile, File()],
) -> TaskOut:
    task_id = uuid.uuid4().hex
    key = input_key(task_id)

    size = await storage.save_upload(key, file)
    if size == 0:
        storage.unlink(key)
        raise HTTPException(
            status_code=status.HTTP_400_BAD_REQUEST,
            detail="Empty upload — input zip must be non-empty.",
        )

    now = utcnow()
    task = Task(
        id=task_id,
        owner_id=user.id,
        name=name,
        status=TASK_PENDING,
        input_path=key,
        input_size=size,
        input_uploaded_at=now,
    )
    db.add(task)
    db.commit()
    db.refresh(task)
    return _task_to_out(task, db)


@router.get("", response_model=list[TaskOut])
def list_tasks(
    user: CurrentUser,
    db: Annotated[Session, Depends(get_db)],
    limit: int = 100,
    offset: int = 0,
) -> list[TaskOut]:
    stmt = select(Task).order_by(Task.created_at.desc()).limit(limit).offset(offset)
    if not user.is_admin:
        stmt = stmt.where(Task.owner_id == user.id)
    rows = db.execute(stmt).scalars().all()
    return [_task_to_out(t, db) for t in rows]


def _load_owned(db: Session, task_id: str, user) -> Task:
    task = db.get(Task, task_id)
    if task is None:
        raise HTTPException(status_code=status.HTTP_404_NOT_FOUND, detail="Task not found")
    if not user.is_admin and task.owner_id != user.id:
        raise HTTPException(status_code=status.HTTP_403_FORBIDDEN, detail="Not your task")
    return task


@router.get("/{task_id}", response_model=TaskOut)
def get_task(
    task_id: str,
    user: CurrentUser,
    db: Annotated[Session, Depends(get_db)],
) -> TaskOut:
    return _task_to_out(_load_owned(db, task_id, user), db)


@router.get("/{task_id}/output")
def download_output(
    task_id: str,
    user: CurrentUser,
    db: Annotated[Session, Depends(get_db)],
    storage: Annotated[Storage, Depends(get_storage)],
) -> FileResponse:
    task = _load_owned(db, task_id, user)
    if task.status != TASK_SUCCEEDED:
        raise HTTPException(
            status_code=status.HTTP_409_CONFLICT,
            detail=f"Task is {task.status}, output not yet available",
        )
    if not task.output_path or not storage.exists(task.output_path):
        # File has been cleaned up. The metadata (summary_json, sizes)
        # is still on the row but the bundle itself is gone forever.
        raise HTTPException(
            status_code=status.HTTP_410_GONE,
            detail="Output bundle has been cleaned up; only metadata remains.",
        )

    if task.output_downloaded_at is None:
        task.output_downloaded_at = utcnow()
        db.commit()

    return FileResponse(
        path=storage.absolute_path(task.output_path),
        filename=f"{task.name or task.id}.zip",
        media_type="application/zip",
    )


@router.post("/{task_id}/retry", response_model=TaskOut)
def retry_task(
    task_id: str,
    user: CurrentUser,
    db: Annotated[Session, Depends(get_db)],
    storage: Annotated[Storage, Depends(get_storage)],
) -> TaskOut:
    """Move a failed (or expired) task back to `pending`.

    Refuses when the input zip is gone — the cleaner already collected
    it, so there's nothing for a Worker to chew on. The caller can
    still soft-delete the row with DELETE if they want it off the list.
    """
    task = _load_owned(db, task_id, user)
    if task.status not in (TASK_FAILED, TASK_EXPIRED):
        raise HTTPException(
            status_code=status.HTTP_409_CONFLICT,
            detail=f"Cannot retry task in state {task.status}",
        )
    if not task.input_path or not storage.exists(task.input_path):
        raise HTTPException(
            status_code=status.HTTP_410_GONE,
            detail="Input file has been cleaned up; cannot retry",
        )
    task.status = TASK_PENDING
    task.error = None
    task.claimed_by = None
    task.claim_expires_at = None
    task.completed_at = None
    task.updated_at = utcnow()
    # Clear any half-uploaded output from a previous attempt.
    if task.output_path:
        storage.unlink(task.output_path)
    task.output_path = None
    task.output_size = None
    task.output_ready_at = None
    task.output_downloaded_at = None
    db.commit()
    db.refresh(task)
    return _task_to_out(task, db)


@router.delete("/{task_id}", status_code=status.HTTP_204_NO_CONTENT)
def delete_task(
    task_id: str,
    user: CurrentUser,
    db: Annotated[Session, Depends(get_db)],
    storage: Annotated[Storage, Depends(get_storage)],
) -> None:
    task = _load_owned(db, task_id, user)
    if task.input_path:
        storage.unlink(task.input_path)
    if task.output_path:
        storage.unlink(task.output_path)
    db.delete(task)
    db.commit()
