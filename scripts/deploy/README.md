# Dispatcher Deploy Scripts

Four idempotent shell scripts that bring a fresh Aliyun ECS host from
"clean image" to "production dispatcher serving HTTPS". Designed for
**Alibaba Cloud Linux 3 / OpenAnolis** (RHEL 8 衍生); should also work
on stock CentOS / Rocky / AlmaLinux 8/9 without changes.

## Order of operations

```bash
ssh root@<your-server-ip>

# Get the scripts onto the box (sparse clone, only the dispatcher subtree).
git clone --depth 1 --filter=blob:none --sparse \
  https://github.com/jefflower/dialect-labeler.git /tmp/dialect-labeler
cd /tmp/dialect-labeler
git sparse-checkout set scripts/deploy services/dispatcher

# Run in order:
bash scripts/deploy/01-provision.sh      # ~3 min (Docker download dominates)
bash scripts/deploy/02-bootstrap.sh      # ~5 min (image build + first start)
bash scripts/deploy/03-caddy.sh          # ~30s (Let's Encrypt issuance)
bash scripts/deploy/04-backup.sh         # ~5s
```

## What each script does

### `01-provision.sh` — OS-level deps

- **Swap**: creates a 2 GiB `/swapfile` if none. Default Aliyun ECS has 0
  swap; the dispatcher's docker image build (Node + Python multi-stage)
  hits ~1.5 GiB peak which is right at the edge of a 1.8 GiB-RAM box.
- **git**: `dnf install -y git`.
- **Docker + Compose plugin**: from the official `docker-ce` repo,
  mirrored through Aliyun (fast in CN). Replaces `podman` + `runc` that
  come pre-installed on alinux 3 (`--allowerasing`).
- **Docker registry mirror**: writes `/etc/docker/daemon.json` pointing
  pulls at `mirrors.aliyun.com/docker/` so image fetches stay fast.
- **Caddy 2** (static binary to `/usr/local/bin/caddy`) + dedicated
  `caddy` user + systemd unit. Single-file deploy; no repo to maintain.
- **firewalld**: opens HTTP/HTTPS (SSH 22 is already allowed; Aliyun
  Security Group is the outer gate).

**Idempotent.** Re-runs skip steps whose outputs are already present.

### `02-bootstrap.sh` — repo + secrets + bring up dispatcher

- Sparse-clones the repo into `/srv/dispatcher-repo`. Symlinks
  `/srv/dispatcher` → `/srv/dispatcher-repo/services/dispatcher` so the
  operator has a stable path to `cd` into.
- **First run**: generates a 48-byte URL-safe `JWT_SECRET` and an
  18-byte bootstrap admin password, writes them to `.env` (mode 0600),
  and **prints the admin password to stdout once**. **Capture it.**
- Subsequent runs **never overwrite `.env`** — to rotate the secret,
  edit `.env` in place and `docker compose restart`.
- `docker compose up -d --build`. Waits up to 30s for `/healthz`.

**Configurable via env vars**: `REPO_URL`, `REPO_BRANCH`, `REPO_DIR`,
`APP_DIR`, `ADMIN_EMAIL`.

### `03-caddy.sh` — reverse proxy + HTTPS

- Writes `/etc/caddy/Caddyfile` configured for **`47-93-1-242.nip.io`**
  by default. nip.io is a magic DNS service that resolves any
  `<dotted-or-hyphenated-IP>.nip.io` hostname to the corresponding IP —
  free, no DNS to manage, and Let's Encrypt happily issues a real cert
  against it.
- Validates the file (`caddy validate`) before `systemctl reload caddy`.
- Probes `https://<domain>/healthz` for up to 60s.

**Switch to a real domain later**:

```bash
CADDY_DOMAIN=dispatcher.example.com \
ADMIN_EMAIL_FOR_LE=ops@example.com \
bash scripts/deploy/03-caddy.sh
```

(Point the A record at the server first; LE issuance fails without
working DNS.)

### `04-backup.sh` — daily backup timer

- Installs `dispatcher-backup.{service,timer}` under `/etc/systemd/system/`.
- Fires once a day at **03:00 UTC** (`Persistent=true` so a downtime
  doesn't skip a backup).
- `tar czf /var/backups/dispatcher/$(date +%F).tgz -C /srv/dispatcher data`.
- Retains 30 days. Older files auto-deleted.
- Runs one backup immediately so you can verify it works.

**On-disk only.** If this disk dies, the backups die too. Wire an
off-box destination into the `.service` `ExecStart=` line (rclone to
Aliyun OSS or borg to a second host) before scaling past closed-beta.

## After deploy — verifying end-to-end

1. **Web reachable**: open `https://47-93-1-242.nip.io/` in a browser.
   You should see the React SPA login page.
2. **Admin login**: log in with the email + password printed by
   `02-bootstrap.sh`. Default email is `admin@dispatcher.local`
   (override with `ADMIN_EMAIL=...` before running 02).
3. **Invite users** via Admin → Users (admin role only).
4. **Smoke test the worker contract**: from a macOS dev box, run a
   local Tauri build pointed at the dispatcher URL, log in as the
   admin, toggle "自动接单" on, and create a test task via the web UI.
   The worker should claim → process → upload → complete the task in
   under a minute (assuming the input is small).

## Operational notes

- **Logs**: `docker compose logs -f dispatcher` (in `/srv/dispatcher`)
  or `journalctl -u caddy -f` for the reverse proxy.
- **Updating the dispatcher**: `cd /srv/dispatcher-repo && git pull &&
  cd services/dispatcher && docker compose up -d --build`.
- **Resetting the admin password** (when nobody can log in): connect
  to the SQLite DB and update `users.password_hash` directly with a
  bcrypt of the new password. The dispatcher's `bcrypt` import path
  is `/srv/dispatcher/.venv/bin/python -m bcrypt` or call from a
  Python shell.
- **Rotating `JWT_SECRET`**: edit `/srv/dispatcher/.env`, then
  `docker compose restart`. All existing tokens become invalid;
  users must log in again.

## Why these are scripts (and not a Compose-only deploy)

Three things need to live outside the docker container:

1. Caddy + Let's Encrypt — the dispatcher container only listens on
   `127.0.0.1:8080`; the public TLS endpoint terminates on Caddy.
2. The systemd backup timer — runs on the host, reads the bind-mounted
   data dir, so it survives container churn.
3. Initial secrets generation — `JWT_SECRET` and bootstrap admin
   credentials must be in `.env` before the container starts.

Each concern is one script, so re-running just one is the unit of
recovery.
