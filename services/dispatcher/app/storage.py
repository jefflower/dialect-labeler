"""Local-disk file relay.

The dispatcher never touches user payloads in memory beyond a buffer at a
time — uploads stream from the multipart parser into a temp file, then
get atomically renamed into place; downloads use FastAPI's FileResponse
which sendfile()'s directly to the socket.

Path safety: every operation reduces its argument to an absolute path
under `storage_dir` and refuses anything that escapes via `..` symlinks
or absolute-path injection. The wrapper class keeps that check in one
place so route handlers can't accidentally skip it.
"""

from __future__ import annotations

import os
import shutil
import tempfile
from pathlib import Path
from typing import BinaryIO, Iterator

from fastapi import UploadFile

from .config import get_settings


class StorageError(Exception):
    pass


class Storage:
    """File-system facade rooted at `Settings.storage_dir`."""

    def __init__(self, root: Path | None = None) -> None:
        self.root = (root or get_settings().storage_dir).resolve()
        self.root.mkdir(parents=True, exist_ok=True)

    # -- internal --------------------------------------------------------
    def _resolve(self, rel: str) -> Path:
        """Map a relative key like 'inputs/abc.zip' to an absolute path
        inside the storage root. Rejects anything that climbs out."""
        if not rel or rel.startswith("/") or "\x00" in rel:
            raise StorageError(f"Invalid storage key: {rel!r}")
        candidate = (self.root / rel).resolve()
        try:
            candidate.relative_to(self.root)
        except ValueError as exc:
            raise StorageError(f"Path escapes storage root: {rel!r}") from exc
        return candidate

    # -- writers ---------------------------------------------------------
    async def save_upload(self, rel: str, upload: UploadFile) -> int:
        """Stream `upload` into `rel`, replacing any existing file
        atomically. Returns bytes written.

        Atomicity: writes to a sibling .partial file, then `os.replace`
        — POSIX guarantees the rename is atomic on the same filesystem,
        so an interrupted upload never leaves a half-baked file in place
        of a previous good one.
        """
        target = self._resolve(rel)
        target.parent.mkdir(parents=True, exist_ok=True)
        tmp_fd, tmp_name = tempfile.mkstemp(
            dir=target.parent, prefix=target.name + ".", suffix=".partial"
        )
        os.close(tmp_fd)
        tmp_path = Path(tmp_name)
        total = 0
        try:
            with tmp_path.open("wb") as fh:
                while True:
                    chunk = await upload.read(1024 * 1024)  # 1 MiB
                    if not chunk:
                        break
                    fh.write(chunk)
                    total += len(chunk)
            os.replace(tmp_path, target)
        except Exception:
            tmp_path.unlink(missing_ok=True)
            raise
        return total

    def save_stream(self, rel: str, source: BinaryIO) -> int:
        """Sync variant for tests / scripts. Same atomicity guarantee."""
        target = self._resolve(rel)
        target.parent.mkdir(parents=True, exist_ok=True)
        tmp_fd, tmp_name = tempfile.mkstemp(
            dir=target.parent, prefix=target.name + ".", suffix=".partial"
        )
        tmp_path = Path(tmp_name)
        total = 0
        try:
            with os.fdopen(tmp_fd, "wb") as fh:
                while True:
                    chunk = source.read(1024 * 1024)
                    if not chunk:
                        break
                    fh.write(chunk)
                    total += len(chunk)
            os.replace(tmp_path, target)
        except Exception:
            tmp_path.unlink(missing_ok=True)
            raise
        return total

    async def save_async_chunks(self, rel: str, chunks) -> int:
        """Write an async-iterable of bytes chunks to `rel`, atomically.

        Used by the worker output upload, which streams a raw PUT body
        (not multipart) — so there's no UploadFile to drive `save_upload`
        with. The iterator must yield `bytes`; empty chunks are tolerated.
        """
        target = self._resolve(rel)
        target.parent.mkdir(parents=True, exist_ok=True)
        tmp_fd, tmp_name = tempfile.mkstemp(
            dir=target.parent, prefix=target.name + ".", suffix=".partial"
        )
        os.close(tmp_fd)
        tmp_path = Path(tmp_name)
        total = 0
        try:
            with tmp_path.open("wb") as fh:
                async for chunk in chunks:
                    if chunk:
                        fh.write(chunk)
                        total += len(chunk)
            os.replace(tmp_path, target)
        except Exception:
            tmp_path.unlink(missing_ok=True)
            raise
        return total

    # -- readers ---------------------------------------------------------
    def absolute_path(self, rel: str) -> Path:
        """Return the on-disk Path for `rel`. Raises if it doesn't exist."""
        path = self._resolve(rel)
        if not path.is_file():
            raise FileNotFoundError(path)
        return path

    def exists(self, rel: str) -> bool:
        try:
            return self._resolve(rel).is_file()
        except StorageError:
            return False

    def iter_chunks(self, rel: str, chunk_size: int = 1024 * 1024) -> Iterator[bytes]:
        with self._resolve(rel).open("rb") as fh:
            while True:
                chunk = fh.read(chunk_size)
                if not chunk:
                    return
                yield chunk

    # -- destructive -----------------------------------------------------
    def unlink(self, rel: str) -> bool:
        try:
            self._resolve(rel).unlink()
            return True
        except (FileNotFoundError, StorageError):
            return False

    def reset_for_tests(self) -> None:
        """Wipe the whole storage tree. Tests only."""
        if self.root.exists():
            shutil.rmtree(self.root)
        self.root.mkdir(parents=True, exist_ok=True)


# Module-level singleton, lazily constructed.
_storage: Storage | None = None


def get_storage() -> Storage:
    global _storage
    if _storage is None:
        _storage = Storage()
    return _storage


def reset_storage_for_tests() -> None:
    global _storage
    _storage = None


# Path layout helpers — keep the naming convention in one place so routes
# and the cleaner can't drift.
def input_key(task_id: str) -> str:
    return f"inputs/{task_id}.zip"


def output_key(task_id: str) -> str:
    return f"outputs/{task_id}.zip"


def release_key(version: str, target: str, filename: str) -> str:
    return f"releases/{version}/{target}/{filename}"
