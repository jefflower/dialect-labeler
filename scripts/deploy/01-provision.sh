#!/usr/bin/env bash
# 01-provision.sh — install OS-level dependencies on the dispatcher box.
#
# Idempotent. Safe to re-run. Each step skips itself when its outputs are
# already present.
#
# Targets:
#   - Alibaba Cloud Linux 3 / OpenAnolis (RHEL 8 衍生). Should also work
#     on stock CentOS / RHEL / Rocky / AlmaLinux 8/9.
#
# What it does:
#   1. Ensure a 2 GiB swapfile (the default Aliyun ECS comes with 0 swap;
#      docker image builds can OOM on the 1.8 GiB RAM tier without it).
#   2. Install git via dnf.
#   3. Install Docker Engine + the Compose plugin from the official
#      docker-ce repo, mirrored through Aliyun (fast in CN).
#   4. Configure a daemon.json registry-mirror so `docker pull` from
#      Docker Hub goes through Aliyun.
#   5. Download the Caddy server static binary to /usr/local/bin and
#      install a systemd unit. We use the official static binary (vs.
#      dnf/COPR) for portability — same binary works on any RHEL-family
#      distro and on Debian/Ubuntu.
#   6. Open firewalld for 80/443 (SSH 22 is already allowed by default).
#
# Sensitive: none. No secrets generated here. See 02-bootstrap.sh for
# secrets management.

set -euo pipefail

step() { echo; echo "▶ $*"; }
have() { command -v "$1" >/dev/null 2>&1; }

# ---------------------------------------------------------------------------
step "1. swap"
# ---------------------------------------------------------------------------
if swapon --show | grep -q '^/'; then
  echo "swap already present: $(swapon --show | tail -n +2 | head -1)"
else
  echo "creating /swapfile (2 GiB)…"
  fallocate -l 2G /swapfile
  chmod 600 /swapfile
  mkswap /swapfile
  swapon /swapfile
  grep -qE '^/swapfile' /etc/fstab || echo "/swapfile none swap sw 0 0" >> /etc/fstab
  free -h | grep -i swap
fi

# ---------------------------------------------------------------------------
step "2. git"
# ---------------------------------------------------------------------------
if have git; then
  echo "git already installed: $(git --version)"
else
  dnf install -y git
fi

# ---------------------------------------------------------------------------
step "3. docker"
# ---------------------------------------------------------------------------
if have docker && docker compose version >/dev/null 2>&1; then
  echo "docker + compose already installed: $(docker --version)"
else
  echo "installing docker-ce + compose plugin via Aliyun mirror…"
  # The docker-ce.repo at mirrors.aliyun.com is the same repo file
  # the official docker docs ship for centos, but with the download.docker.com
  # base URL rewritten to point at the mirror.
  curl -fsSL https://mirrors.aliyun.com/docker-ce/linux/centos/docker-ce.repo \
    -o /etc/yum.repos.d/docker-ce.repo
  sed -i 's|download.docker.com|mirrors.aliyun.com/docker-ce|g' \
    /etc/yum.repos.d/docker-ce.repo
  # `--allowerasing` is required because alinux 3 ships podman + runc, which
  # conflict with docker-ce / containerd.io. We deliberately replace them.
  #
  # `--exclude=docker-ce-rootless-extras` skips a weak dep that ships
  # corrupted (0-byte) from Aliyun's mirror as of 2026-05; we don't run
  # rootless docker anyway (the dispatcher binds privileged ports / runs
  # as root inside its container).
  dnf install -y --allowerasing \
    --exclude=docker-ce-rootless-extras \
    docker-ce docker-ce-cli containerd.io docker-compose-plugin
  systemctl enable --now docker
fi

step "3b. docker daemon registry mirror + DNS"
# DNS here is critical on Alibaba Cloud Linux: the host runs
# systemd-resolved with stub 127.0.0.53, which is unreachable from
# inside a container's bridge network → every `RUN pip install` and
# `RUN npm install` fails with "name resolution" timeouts. Pinning
# the docker daemon to Aliyun's internal DNS (100.100.2.{136,138})
# plus AliDNS public (223.{5,6}.{5,6}) sidesteps the stub.
mkdir -p /etc/docker
if ! grep -q '"dns"' /etc/docker/daemon.json 2>/dev/null; then
  cat > /etc/docker/daemon.json <<EOF
{
  "registry-mirrors": [
    "https://mirrors.aliyun.com/docker/",
    "https://docker.m.daocloud.io"
  ],
  "dns": ["100.100.2.136", "100.100.2.138", "223.5.5.5", "223.6.6.6"],
  "log-driver": "json-file",
  "log-opts": { "max-size": "20m", "max-file": "5" }
}
EOF
  systemctl restart docker
  echo "daemon.json written and docker restarted"
else
  echo "daemon.json already configured"
fi
docker info 2>/dev/null | grep -i "registry mirror" || true

# ---------------------------------------------------------------------------
step "4. caddy"
# ---------------------------------------------------------------------------
# Sub-step idempotent: each piece (binary / user / dirs / placeholder
# Caddyfile / systemd unit) checks itself. Re-running the script after a
# half-done install (e.g. ghproxy download failed but binary partially
# fetched) backfills only what's missing.

CADDY_VERSION="${CADDY_VERSION:-2.10.0}"

# 4.1 binary at /usr/local/bin/caddy
if have caddy && caddy version 2>/dev/null | grep -q "$CADDY_VERSION"; then
  echo "caddy $CADDY_VERSION already at $(command -v caddy)"
else
  arch=$(uname -m)
  case "$arch" in
    x86_64) caddy_arch=amd64 ;;
    aarch64) caddy_arch=arm64 ;;
    *) echo "unsupported arch: $arch"; exit 1 ;;
  esac
  caddy_file="caddy_${CADDY_VERSION}_linux_${caddy_arch}.tar.gz"
  upstream="https://github.com/caddyserver/caddy/releases/download/v${CADDY_VERSION}/${caddy_file}"
  # GitHub releases from inside China often stall at a few MB/s or
  # outright hang. Try ghproxy first, fall back to upstream. ghproxy
  # rotates frequently — list multiple variants so any one being down
  # doesn't block us.
  mirrors=(
    "https://ghproxy.net/${upstream}"
    "https://mirror.ghproxy.com/${upstream}"
    "$upstream"
  )
  tmp=$(mktemp -d)
  for m in "${mirrors[@]}"; do
    echo "fetching $m"
    if curl -fsSL --connect-timeout 10 --max-time 180 -o "$tmp/caddy.tar.gz" "$m"; then
      echo "✓ got it"
      break
    fi
    echo "  failed, trying next mirror…"
  done
  if [[ ! -s "$tmp/caddy.tar.gz" ]]; then
    echo "❌ all caddy mirrors failed"
    exit 1
  fi
  tar -xzf "$tmp/caddy.tar.gz" -C "$tmp" caddy
  install -m 0755 "$tmp/caddy" /usr/local/bin/caddy
  rm -rf "$tmp"
  echo "caddy installed: $(caddy version)"
fi

# 4.2 dedicated unprivileged user
if ! id caddy >/dev/null 2>&1; then
  useradd --system --home /var/lib/caddy --shell /usr/sbin/nologin caddy
  echo "created caddy user"
fi

# 4.3 config + cert + log directories
mkdir -p /etc/caddy /var/lib/caddy /var/log/caddy
chown -R caddy:caddy /var/lib/caddy /var/log/caddy

# 4.4 placeholder Caddyfile so the service can start before 03 lays
# down the real reverse-proxy config. Must be multi-line — single-line
# `:80 { … }` confuses the Caddyfile adapter.
if [[ ! -f /etc/caddy/Caddyfile ]]; then
  cat > /etc/caddy/Caddyfile <<'CFG'
:80 {
    respond "caddy: awaiting Caddyfile from 03-caddy.sh" 200
}
CFG
fi

# 4.5 systemd unit (matches caddyserver/dist canonical unit)
if [[ ! -f /etc/systemd/system/caddy.service ]]; then
  cat > /etc/systemd/system/caddy.service <<'UNIT'
[Unit]
Description=Caddy
Documentation=https://caddyserver.com/docs/
After=network.target network-online.target
Requires=network-online.target

[Service]
Type=notify
User=caddy
Group=caddy
ExecStart=/usr/local/bin/caddy run --environ --config /etc/caddy/Caddyfile
ExecReload=/usr/local/bin/caddy reload --config /etc/caddy/Caddyfile --force
TimeoutStopSec=5s
LimitNOFILE=1048576
PrivateTmp=true
ProtectSystem=full
AmbientCapabilities=CAP_NET_ADMIN CAP_NET_BIND_SERVICE

[Install]
WantedBy=multi-user.target
UNIT
  systemctl daemon-reload
fi

# 4.6 ensure enabled + running. If we just laid down the unit, this
# starts it. If it was already up, this is a no-op.
systemctl enable --quiet caddy
systemctl is-active --quiet caddy || systemctl start caddy
echo "caddy systemd: $(systemctl is-active caddy)"

# ---------------------------------------------------------------------------
step "5. firewalld (open 80/443; SSH 22 was already permitted)"
# ---------------------------------------------------------------------------
if have firewall-cmd; then
  if ! systemctl is-active --quiet firewalld; then
    systemctl enable --now firewalld
  fi
  firewall-cmd --permanent --add-service=http >/dev/null 2>&1 || true
  firewall-cmd --permanent --add-service=https >/dev/null 2>&1 || true
  firewall-cmd --reload >/dev/null 2>&1 || true
  firewall-cmd --list-services
else
  echo "firewalld not installed; skipping (Aliyun ECS security group is the real gate)"
fi

# ---------------------------------------------------------------------------
echo
echo "✅ 01-provision.sh complete"
echo "Next: scripts/deploy/02-bootstrap.sh"
