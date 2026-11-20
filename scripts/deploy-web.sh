#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REMOTE_USER="${N2SAC_USER:-criper}"
REMOTE_HOST="${N2SAC_HOST:-192.168.1.150}"
REMOTE_DIR="${N2SAC_REMOTE_DIR:-/opt/n2sac}"

echo "[deploy-web] Typechecking frontend..."
(cd "${ROOT_DIR}/app" && npx tsc --noEmit)

echo "[deploy-web] Syncing frontend to ${REMOTE_USER}@${REMOTE_HOST}:${REMOTE_DIR}..."
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

echo "[deploy-web] Rebuilding & restarting web (nginx)..."
ssh "${REMOTE_USER}@${REMOTE_HOST}" "cd '${REMOTE_DIR}' && docker compose build web && docker compose up -d --no-deps web"
echo "[deploy-web] Done! Web live at http://${REMOTE_HOST}:8080"
