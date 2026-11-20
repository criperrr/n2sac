#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REMOTE_USER="${N2SAC_USER:-criper}"
REMOTE_HOST="${N2SAC_HOST:-192.168.1.150}"
REMOTE_DIR="${N2SAC_REMOTE_DIR:-/opt/n2sac}"

echo "[deploy-server] Syncing backend to ${REMOTE_USER}@${REMOTE_HOST}:${REMOTE_DIR}..."
rsync -az --delete \
  --exclude 'target/' \
  --exclude 'app/node_modules/' \
  --exclude 'app/android/' \
  --exclude 'app/dist/' \
  --exclude '.git/' \
  --exclude '.agents/' \
  --exclude 'secrets/' \
  --exclude '.env' \
  "${ROOT_DIR}/" "${REMOTE_USER}@${REMOTE_HOST}:${REMOTE_DIR}/"

echo "[deploy-server] Rebuilding & restarting nsac-server..."
ssh "${REMOTE_USER}@${REMOTE_HOST}" "cd '${REMOTE_DIR}' && docker compose build nsac-server && docker compose up -d --no-deps nsac-server"

echo "[deploy-server] Verifying /api/v1/health..."
sleep 3
curl -fsS "http://${REMOTE_HOST}:3000/api/v1/health" && echo ""
echo "[deploy-server] Done!"
