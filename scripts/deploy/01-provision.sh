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
  dnf install -y --allowerasing \
    docker-ce docker-ce-cli containerd.io docker-compose-plugin
  systemctl enable --now docker
fi

step "3b. docker daemon registry mirror"
mkdir -p /etc/docker
if ! grep -q "mirrors.aliyun.com/docker" /etc/docker/daemon.json 2>/dev/null; then
  cat > /etc/docker/daemon.json <<EOF
{
  "registry-mirrors": [
    "https://mirrors.aliyun.com/docker/",
    "https://docker.m.daocloud.io"
  ],
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
CADDY_VERSION="${CADDY_VERSION:-2.10.0}"
if have caddy && caddy version 2>/dev/null | grep -q "$CADDY_VERSION"; then
  echo "caddy $CADDY_VERSION already installed"
else
  # Default to the official static binary build. Single file, no repo to
  # maintain. Architecture detection covers x86_64 + arm64 hosts.
  arch=$(uname -m)
  case "$arch" in
    x86_64) caddy_arch=amd64 ;;
    aarch64) caddy_arch=arm64 ;;
    *) echo "unsupported arch: $arch"; exit 1 ;;
  esac
  url="https://github.com/caddyserver/caddy/releases/download/v${CADDY_VERSION}/caddy_${CADDY_VERSION}_linux_${caddy_arch}.tar.gz"
  echo "downloading $url"
  tmp=$(mktemp -d)
  curl -fsSL -o "$tmp/caddy.tar.gz" "$url"
  tar -xzf "$tmp/caddy.tar.gz" -C "$tmp" caddy
  install -m 0755 "$tmp/caddy" /usr/local/bin/caddy
  rm -rf "$tmp"

  # Dedicated unprivileged user that owns the cert/config dirs.
  if ! id caddy >/dev/null 2>&1; then
    useradd --system --home /var/lib/caddy --shell /usr/sbin/nologin caddy
  fi
  mkdir -p /etc/caddy /var/lib/caddy /var/log/caddy
  chown -R caddy:caddy /var/lib/caddy /var/log/caddy
  # Lay down a placeholder Caddyfile so the service starts cleanly
  # before 03-caddy.sh overwrites it with the real reverse-proxy config.
  if [[ ! -f /etc/caddy/Caddyfile ]]; then
    echo ":80 { respond \"caddy: awaiting Caddyfile\" 200 }" > /etc/caddy/Caddyfile
  fi

  # Install systemd unit. The contents below match caddyserver/dist's
  # canonical unit (caddyserver.com/docs/running#unit-files), inlined so
  # we don't depend on the dist repo being reachable.
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
  systemctl enable --now caddy
  echo "caddy installed: $(caddy version)"
fi

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
