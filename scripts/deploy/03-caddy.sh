#!/usr/bin/env bash
# 03-caddy.sh — write the production Caddyfile and reload caddy.
#
# Idempotent. Re-runs replace the Caddyfile in place + reload.
#
# Defaults to `47-93-1-242.nip.io` (the user's Aliyun ECS public IP via
# nip.io magic DNS) so Let's Encrypt issues a real TLS cert without
# requiring a purchased domain. Override at any time:
#
#   CADDY_DOMAIN=dispatcher.example.com bash 03-caddy.sh
#
# When you move to a real domain, point its A record at this server's
# public IP, re-run this script with CADDY_DOMAIN set, and caddy will
# auto-issue + auto-renew the new cert.

set -euo pipefail

step() { echo; echo "▶ $*"; }

CADDY_DOMAIN="${CADDY_DOMAIN:-47-93-1-242.nip.io}"
ADMIN_EMAIL_FOR_LE="${ADMIN_EMAIL_FOR_LE:-}"  # optional; LE will email cert-expiry alerts here

# ---------------------------------------------------------------------------
step "1. write Caddyfile (domain=$CADDY_DOMAIN)"
# ---------------------------------------------------------------------------
mkdir -p /etc/caddy
{
  if [[ -n "$ADMIN_EMAIL_FOR_LE" ]]; then
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
        reverse_proxy 127.0.0.1:8080
    }

    # Long-lived response streaming for /api/worker/* — claim/heartbeat
    # don't need it, but downloading a multi-GB output zip from the
    # worker does. Bump timeouts past defaults.
    reverse_proxy 127.0.0.1:8080 {
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
# Pause briefly so Caddy completes the ACME challenge against LE.
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
