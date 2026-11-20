#!/usr/bin/env bash
set -euo pipefail

REMOTE_USER="${N2SAC_USER:-criper}"
REMOTE_HOST="${N2SAC_HOST:-192.168.1.150}"

echo "=== Containers (${REMOTE_HOST}) ==="
ssh "${REMOTE_USER}@${REMOTE_HOST}" 'docker ps --format "table {{.Names}}\t{{.Status}}\t{{.Image}}"'

echo ""
echo "=== API Health ==="
curl -fsS "http://${REMOTE_HOST}:3000/api/v1/health" && echo ""

echo ""
echo "=== Cloudflare Tunnel URL ==="
ssh "${REMOTE_USER}@${REMOTE_HOST}" 'docker logs n2sac-cloudflared 2>&1 | grep -oE "https://[a-zA-Z0-9.-]+\.trycloudflare\.com" | tail -n 1 || echo "(named tunnel or starting...)"'

echo ""
echo "=== Recent nsac-server Logs (FCM / Discord) ==="
ssh "${REMOTE_USER}@${REMOTE_HOST}" 'docker logs --tail 20 n2sac-server 2>&1 | grep -iE "fcm|discord|starting|error|warn" || true'
