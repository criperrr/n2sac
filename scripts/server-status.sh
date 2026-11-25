#!/usr/bin/env bash
set -euo pipefail

REMOTE_USER="${N2SAC_USER:-criper}"
REMOTE_HOST="${N2SAC_HOST:-100.69.91.121}"
PUBLIC_URL="${N2SAC_PUBLIC_URL:-https://notebook-server.tailddd6bf.ts.net}"
SSH_OPTS=(-T -o BatchMode=yes -o ConnectTimeout=5 -o LogLevel=ERROR)

echo "=== Public Funnel Health (${PUBLIC_URL}) ==="
curl -fsS --connect-timeout 5 --max-time 10 "${PUBLIC_URL}/api/v1/health" && echo ""
curl -fsS --connect-timeout 5 --max-time 10 "${PUBLIC_URL}/version.json" && echo ""

ssh "${SSH_OPTS[@]}" "${REMOTE_USER}@${REMOTE_HOST}" '
set -euo pipefail
echo ""
echo "=== Containers ==="
docker ps --format "table {{.Names}}\t{{.Status}}\t{{.Image}}"
echo ""
echo "=== Loopback API Health ==="
curl -fsS --connect-timeout 3 --max-time 5 http://127.0.0.1:3000/api/v1/health && echo ""
echo ""
echo "=== Tailscale Funnel ==="
tailscale funnel status 2>/dev/null || echo "(tailscale funnel status unavailable)"
echo ""
echo "=== Recent nsac-server Logs (FCM / Discord / Warnings) ==="
docker logs --tail 25 n2sac-server 2>&1 | grep -iE "fcm|discord|starting|error|warn" || true
'
