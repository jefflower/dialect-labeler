# dialect-labeler dispatcher

Cloud-side task broker + file relay + accounts for the dialect-labeler
desktop client. **All audio/LLM processing stays on the client.** The
dispatcher's only jobs are:

1. Take an input zip from a user, store it on local disk, mark it
   `pending` in the queue.
2. Hand pending tasks out to Workers one at a time via a lease-based
   claim protocol.
3. Receive the produced output bundle from a Worker, hand it back to
   the owner once.
4. **Delete files once they've served their purpose** so the host's
   disk doesn't fill up. DB rows live forever (with a `summary_json`
   snapshot of what the task produced); the actual zips get unlinked
   after 7 days or after the owner downloads, whichever comes first.

This is batch 1 of a five-batch plan. Web UI, tray Worker mode in the
Tauri client, admin pages, and the Updater endpoint come later.

## Quickstart (host Python)

```bash
cd services/dispatcher
python3.11 -m venv .venv
.venv/bin/pip install -r requirements.txt
.venv/bin/pip install pytest httpx
cp .env.example .env
# edit .env — at minimum set JWT_SECRET in production
.venv/bin/uvicorn app.main:app --port 8080
```

Then in another shell:

```bash
# First registrant becomes admin.
curl -X POST localhost:8080/api/auth/register \
  -H 'content-type: application/json' \
  -d '{"email":"admin@example.com","password":"12345678"}'

# Login, capture token.
TOKEN=$(curl -sX POST localhost:8080/api/auth/login \
  -H 'content-type: application/json' \
  -d '{"email":"admin@example.com","password":"12345678"}' \
  | python3 -c 'import json,sys; print(json.load(sys.stdin)["access_token"])')

# Upload an input zip as a task.
curl -X POST localhost:8080/api/tasks \
  -H "authorization: bearer $TOKEN" \
  -F 'name=demo' -F 'file=@/path/to/your-input.zip'
```

## Quickstart (Docker)

```bash
cd services/dispatcher
cp .env.example .env  # set JWT_SECRET!
docker compose up --build
```

The container mounts `./data` from the host into `/data`, so DB +
storage survive container rebuilds. Back up the whole `./data` tree.

## Layout

```
app/
  main.py         FastAPI app, lifespan, scheduler bootstrap, /healthz
  config.py       Pydantic Settings, loaded from .env / env vars
  db.py           SQLAlchemy engine, Session factory, WAL pragmas
  models.py       User, Task, Release ORM
  schemas.py      Pydantic request/response models
  auth.py         bcrypt password hashing, JWT issuance, current-user dep
  storage.py      Local-FS file relay, path-traversal-safe
  queue_ops.py    Atomic claim + lease extension
  cleaner.py      Periodic file cleanup (APScheduler)
  routes/
    auth.py       /api/auth/{register,login,me}
    tasks.py      /api/tasks (CRUD + output download)
    worker.py     /api/worker/{claim,tasks/<id>/{input,heartbeat,output,complete,fail}}
tests/            pytest suite — 24 tests across the four route surfaces
```

## Cleanup policy

| Trigger | Action |
|---|---|
| Task succeeded AND (downloaded OR `output_ready_at + OUTPUT_TTL` passed) | Delete output zip; also delete input zip (succeeded tasks have no further use for input). Set `files_cleaned_at`. |
| Task failed AND `updated_at + FAILED_INPUT_TTL` passed | Delete input zip. (Operators get the retention window to retry/debug.) |
| Task claimed/running AND `claim_expires_at < now` | Reset to `pending` so another Worker can pick it up. |

All defaults are in `.env.example`. The cleaner runs every
`CLEAN_INTERVAL_SECONDS` (default 5 minutes). Files are deleted; the
metadata row, `summary_json`, and `*_size` columns are kept forever.

## Tests

```bash
.venv/bin/pytest
```

24 tests cover: auth flows (first-admin bootstrap, open-registration
toggle, login failure modes, /me), task CRUD (create/upload, list
scoping by role, ownership checks, delete), worker lifecycle (claim →
input → heartbeat → output → complete, fail path, concurrent claims),
cleaner (download-triggered, TTL-triggered, failed-input retention,
expired-lease recovery).
