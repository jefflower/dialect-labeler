#!/usr/bin/env bash
# 03-caddy.sh — write the Caddyfile and reload caddy.
#
# Two modes, picked by the CADDY_DOMAIN env var:
#
#   CADDY_DOMAIN=<hostname>   → HTTPS with Let's Encrypt auto-issuance.
#                               Requires inbound 80 + 443 reachable from
#                               the public internet (cloud security
#                               group + host firewall both open).
#
#   CADDY_DOMAIN=:8080         → plain HTTP on port 8080. The dispatcher
#                               is served at http://<server>:8080/. TLS
#                               is the caller's job (Aliyun SLB,
#                               Cloudflare in front, etc.). Use when
#                               the cloud security group only permits
#                               8080 inbound — the case where every
#                               other port (80/443/anything ≥1024) is
#                               firewalled off at the cloud-level.
#
# Default is the HTTP-on-8080 mode because the production target's
# Aliyun security group has been narrowed to that single port.

set -euo pipefail

step() { echo; echo "▶ $*"; }

CADDY_DOMAIN="${CADDY_DOMAIN:-:8080}"
ADMIN_EMAIL_FOR_LE="${ADMIN_EMAIL_FOR_LE:-}"  # optional; LE will email cert-expiry alerts here
UPSTREAM="${UPSTREAM:-127.0.0.1:9000}"  # dispatcher container's host-side loopback bind

# ---------------------------------------------------------------------------
step "1. write Caddyfile (site=$CADDY_DOMAIN, upstream=$UPSTREAM)"
# ---------------------------------------------------------------------------
mkdir -p /etc/caddy
{
  # The global block (only emitted when in HTTPS mode + LE email set).
  if [[ -n "$ADMIN_EMAIL_FOR_LE" && "$CADDY_DOMAIN" != :* ]]; then
    echo "{"
    echo "    email $ADMIN_EMAIL_FOR_LE"
    echo "}"
    echo
  fi
  cat <<EOF
$CADDY_DOMAIN {
    encode zstd gzip

    # Bumped well above Caddy's defaults so the dispatcher's 5 GB
    # upload cap (services/dispatcher/app/config.py:max_upload_bytes)
    # can be reached without Caddy clipping the request body.
    request_body {
        max_size 5GB
    }

    # Health check from off-host monitoring stays cheap (no logs).
    @healthz path /healthz
    handle @healthz {
        reverse_proxy $UPSTREAM
    }

    # Long-lived response streaming for /api/worker/* — claim/heartbeat
    # don't need it, but downloading a multi-GB output zip from the
    # worker does. Bump timeouts past defaults.
    reverse_proxy $UPSTREAM {
        flush_interval -1
        transport http {
            read_timeout 30m
            write_timeout 30m
        }
    }
}
EOF
} > /etc/caddy/Caddyfile

# ---------------------------------------------------------------------------
step "2. validate"
# ---------------------------------------------------------------------------
/usr/local/bin/caddy validate --config /etc/caddy/Caddyfile

# ---------------------------------------------------------------------------
step "3. reload"
# ---------------------------------------------------------------------------
systemctl reload caddy
sleep 2
systemctl is-active caddy

# ---------------------------------------------------------------------------
step "4. smoke-test"
# ---------------------------------------------------------------------------
if [[ "$CADDY_DOMAIN" == :* ]]; then
  # Plain-HTTP mode: caddy is listening on the bare port, hit /healthz
  # directly. No ACME involved → it should respond instantly.
  port="${CADDY_DOMAIN#:}"
  for i in {1..15}; do
    if curl -fsS --max-time 5 "http://127.0.0.1:$port/healthz" 2>/dev/null \
        | grep -q '"ok":true'; then
      echo "✅ http://<server>:$port/healthz returns 200"
      echo "  (TLS is the caller's job — front this with SLB / Cloudflare / etc."
      echo "   if you need https. The dispatcher itself ships plaintext over $port.)"
      exit 0
    fi
    sleep 2
  done
  echo "❌ caddy is up but /healthz didn't respond. Check:"
  echo "    journalctl -u caddy -f"
  echo "    docker compose -f /srv/dispatcher/docker-compose.yml ps"
  exit 1
fi

# HTTPS mode: pause briefly so Caddy completes the ACME challenge against LE.
echo "waiting up to 60s for the cert to be issued…"
for i in {1..30}; do
  if curl -fsS --max-time 5 "https://$CADDY_DOMAIN/healthz" 2>/dev/null | grep -q '"ok":true'; then
    echo "✅ https://$CADDY_DOMAIN/healthz returns 200"
    echo
    echo "✅ 03-caddy.sh complete"
    echo "Next: scripts/deploy/04-backup.sh"
    exit 0
  fi
  sleep 2
done

echo "⚠ https probe did not succeed within 60s."
echo "  This is OK if LE is still issuing the cert (first issuance can take a"
echo "  minute, especially if HTTP-01 needs port 80 to be reachable from the"
echo "  internet). Watch the live cert issuance with:"
echo "    journalctl -u caddy -f"
echo "  Once you see 'certificate obtained successfully', re-run:"
echo "    curl -v https://$CADDY_DOMAIN/healthz"
