"""Worker-facing routes.

Lifecycle a Worker walks:

  POST /api/worker/claim                  -> TaskClaim (or 204 No Content)
  GET  /api/worker/tasks/{id}/input       -> stream input zip
  POST /api/worker/tasks/{id}/heartbeat   -> extend the lease
  PUT  /api/worker/tasks/{id}/output      -> upload output zip
  POST /api/worker/tasks/{id}/complete    -> mark succeeded, attach summary
  POST /api/worker/tasks/{id}/fail        -> mark failed, attach error

Each non-claim endpoint asserts that the caller currently holds the
task's lease. A worker that lost its lease (lease expired, then another
worker claimed) gets a 409 and is expected to drop the local work and
loop back to /claim.
"""

from __future__ import annotations

import json
from typing import Annotated

from fastapi import APIRouter, Depends, HTTPException, Request, Response, status
from fastapi.responses import FileResponse
from sqlalchemy.orm import Session

from ..auth import CurrentUser
from ..config import get_settings
from ..db import get_db
from ..models import (
    TASK_CLAIMED,
    TASK_FAILED,
    TASK_RUNNING,
    TASK_SUCCEEDED,
    Task,
    utcnow,
)
from ..queue_ops import claim_next_pending, extend_lease
from ..schemas import TaskClaim, TaskCompleteIn, TaskFailIn, TaskProgressIn
from ..storage import Storage, get_storage, output_key

router = APIRouter(prefix="/api/worker", tags=["worker"])


def _load_held(db: Session, task_id: str, user_id: int) -> Task:
    task = db.get(Task, task_id)
    if task is None:
        raise HTTPException(status_code=status.HTTP_404_NOT_FOUND, detail="Task not found")
    if task.claimed_by != user_id or task.status not in (TASK_CLAIMED, TASK_RUNNING):
        raise HTTPException(
            status_code=status.HTTP_409_CONFLICT,
            detail="You no longer hold this task's lease",
        )
    if task.claim_expires_at and task.claim_expires_at < utcnow():
        raise HTTPException(
            status_code=status.HTTP_409_CONFLICT,
            detail="Lease expired — task has been returned to the queue",
        )
    return task


@router.post("/claim", response_model=TaskClaim | None)
def claim(
    user: CurrentUser,
    db: Annotated[Session, Depends(get_db)],
) -> TaskClaim | Response:
    settings = get_settings()
    task = claim_next_pending(db, user.id, settings.lease_ttl_seconds)
    if task is None:
        return Response(status_code=status.HTTP_204_NO_CONTENT)
    return TaskClaim(
        id=task.id,
        name=task.name,
        owner_id=task.owner_id,
        input_size=task.input_size,
        claim_expires_at=task.claim_expires_at,  # type: ignore[arg-type]
        mode=task.mode or "dialect",
    )


@router.get("/tasks/{task_id}/input")
def download_input(
    task_id: str,
    user: CurrentUser,
    db: Annotated[Session, Depends(get_db)],
    storage: Annotated[Storage, Depends(get_storage)],
) -> FileResponse:
    task = _load_held(db, task_id, user.id)
    if not task.input_path or not storage.exists(task.input_path):
        raise HTTPException(
            status_code=status.HTTP_410_GONE,
            detail="Input file is gone",
        )
    # Flip to 'running' on first input fetch — useful in the UI for
    # distinguishing "claimed but not started" from "actively processing".
    if task.status == TASK_CLAIMED:
        task.status = TASK_RUNNING
        db.commit()
    return FileResponse(
        path=storage.absolute_path(task.input_path),
        filename=f"{task.id}.zip",
        media_type="application/zip",
    )


@router.post("/tasks/{task_id}/heartbeat", status_code=status.HTTP_204_NO_CONTENT)
def heartbeat(
    task_id: str,
    user: CurrentUser,
    db: Annotated[Session, Depends(get_db)],
) -> None:
    settings = get_settings()
    ok = extend_lease(db, task_id, user.id, settings.lease_ttl_seconds)
    if not ok:
        raise HTTPException(
            status_code=status.HTTP_409_CONFLICT,
            detail="No active lease for this task",
        )


@router.put("/tasks/{task_id}/output", status_code=status.HTTP_204_NO_CONTENT)
async def upload_output(
    task_id: str,
    request: Request,
    user: CurrentUser,
    db: Annotated[Session, Depends(get_db)],
    storage: Annotated[Storage, Depends(get_storage)],
) -> None:
    task = _load_held(db, task_id, user.id)
    key = output_key(task.id)
    total = await storage.save_async_chunks(key, request.stream())
    if total == 0:
        storage.unlink(key)
        raise HTTPException(
            status_code=status.HTTP_400_BAD_REQUEST, detail="Empty output upload"
        )
    task.output_path = key
    task.output_size = total
    task.output_ready_at = utcnow()
    db.commit()


@router.post("/tasks/{task_id}/complete", response_model=None, status_code=status.HTTP_204_NO_CONTENT)
def complete(
    task_id: str,
    payload: TaskCompleteIn,
    user: CurrentUser,
    db: Annotated[Session, Depends(get_db)],
) -> None:
    task = _load_held(db, task_id, user.id)
    if not task.output_path:
        raise HTTPException(
            status_code=status.HTTP_400_BAD_REQUEST,
            detail="Upload the output before marking complete",
        )
    task.status = TASK_SUCCEEDED
    task.completed_at = utcnow()
    task.summary_json = json.dumps(payload.summary, ensure_ascii=False)
    task.claim_expires_at = None
    db.commit()


@router.post("/tasks/{task_id}/fail", status_code=status.HTTP_204_NO_CONTENT)
def fail(
    task_id: str,
    payload: TaskFailIn,
    user: CurrentUser,
    db: Annotated[Session, Depends(get_db)],
) -> None:
    task = _load_held(db, task_id, user.id)
    task.status = TASK_FAILED
    task.error = payload.error
    task.completed_at = utcnow()
    task.claim_expires_at = None
    db.commit()


@router.post(
    "/tasks/{task_id}/progress",
    status_code=status.HTTP_204_NO_CONTENT,
)
def progress(
    task_id: str,
    payload: TaskProgressIn,
    user: CurrentUser,
    db: Annotated[Session, Depends(get_db)],
) -> None:
    """Worker-pushed progress snapshot.

    Persisted to the row so owner polling `GET /api/tasks/{id}` sees the
    latest values without a realtime channel. Lossy by design — Worker
    is expected to fire one of these every 5s within a long stage plus
    one at every stage transition, but we don't care if a few drop on
    the wire. Re-extends the lease as a side effect so the Worker
    doesn't need to interleave heartbeat + progress calls.
    """
    task = _load_held(db, task_id, user.id)
    task.progress_percent = max(0, min(100, payload.percent))
    task.progress_stage = payload.stage
    task.progress_detail = payload.detail
    task.progress_updated_at = utcnow()
    db.commit()
    # Treat the progress push as a soft heartbeat so the Worker doesn't
    # have to interleave two calls. extend_lease commits independently.
    settings = get_settings()
    extend_lease(db, task_id, user.id, settings.lease_ttl_seconds)
