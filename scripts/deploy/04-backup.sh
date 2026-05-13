#!/usr/bin/env bash
# 04-backup.sh — install a daily off-process backup of the dispatcher
# data dir as a systemd .service + .timer pair.
#
# Backups go to /var/backups/dispatcher/YYYY-MM-DD.tgz. We keep 30 days.
# This is single-host only — it does NOT push to off-box storage. If
# this disk dies, the backups die with it. Wire an off-box rsync /
# rclone / borg target into the .service ExecStart= line when you have
# a destination ready.
#
# Idempotent. Re-runs overwrite the units in place; nothing destructive.

set -euo pipefail

step() { echo; echo "▶ $*"; }

DATA_DIR="${DATA_DIR:-/srv/dispatcher/data}"
BACKUP_DIR="${BACKUP_DIR:-/var/backups/dispatcher}"
RETAIN_DAYS="${RETAIN_DAYS:-30}"

# ---------------------------------------------------------------------------
step "1. ensure backup target directory"
# ---------------------------------------------------------------------------
mkdir -p "$BACKUP_DIR"
chmod 700 "$BACKUP_DIR"

# ---------------------------------------------------------------------------
step "2. write systemd .service"
# ---------------------------------------------------------------------------
cat > /etc/systemd/system/dispatcher-backup.service <<EOF
[Unit]
Description=dialect-labeler dispatcher daily backup
Documentation=https://github.com/jefflower/dialect-labeler/blob/main/scripts/deploy/README.md
ConditionPathIsDirectory=$DATA_DIR

[Service]
Type=oneshot
# Tar the whole data dir (SQLite DB + uploaded zips + summary JSON).
# Pipe through gzip; SQLite WAL files are small so the savings are
# modest, but compression is essentially free and the resulting
# tarball is portable.
ExecStart=/bin/sh -c '/usr/bin/tar czf $BACKUP_DIR/\$(date +%%F).tgz -C $(dirname $DATA_DIR) $(basename $DATA_DIR)'
ExecStart=/usr/bin/find $BACKUP_DIR -name "*.tgz" -mtime +$RETAIN_DAYS -delete
Nice=10
IOSchedulingClass=best-effort
IOSchedulingPriority=5
EOF

# ---------------------------------------------------------------------------
step "3. write systemd .timer (03:00 daily)"
# ---------------------------------------------------------------------------
cat > /etc/systemd/system/dispatcher-backup.timer <<'EOF'
[Unit]
Description=Daily backup of the dispatcher data dir
Requires=dispatcher-backup.service

[Timer]
OnCalendar=*-*-* 03:00:00
# If the box was off at 03:00, run the backup as soon as it boots.
Persistent=true
RandomizedDelaySec=600

[Install]
WantedBy=timers.target
EOF

# ---------------------------------------------------------------------------
step "4. enable + start"
# ---------------------------------------------------------------------------
systemctl daemon-reload
systemctl enable --now dispatcher-backup.timer
systemctl list-timers --no-pager | grep dispatcher-backup || true

# ---------------------------------------------------------------------------
step "5. fire one immediately as a sanity check"
# ---------------------------------------------------------------------------
echo "running first backup synchronously to verify…"
systemctl start dispatcher-backup.service
ls -lh "$BACKUP_DIR" | tail -5

echo
echo "✅ 04-backup.sh complete"
echo "→ on-disk backups under $BACKUP_DIR (single-host only — wire an"
echo "  off-box destination into dispatcher-backup.service before going"
echo "  beyond closed-beta scale)"
