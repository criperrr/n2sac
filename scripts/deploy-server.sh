#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REMOTE_USER="${N2SAC_USER:-criper}"
REMOTE_HOST="${N2SAC_HOST:-100.69.91.121}"
REMOTE_DIR="${N2SAC_REMOTE_DIR:-/opt/n2sac}"

echo "[deploy-server] Syncing backend sources to ${REMOTE_USER}@${REMOTE_HOST}:${REMOTE_DIR}..."
rsync -az --delete \
  --exclude 'target/' \
  --exclude 'app/node_modules/' \
  --exclude 'app/android/' \
  --exclude 'app/dist/' \
  --exclude '.git/' \
  --exclude '.agents/' \
  --exclude 'secrets/' \
  --exclude 'scripts/admin-db-panel/' \
  --exclude 'scripts/dev-local/' \
  --exclude 'docker-compose.test.yml' \
  --exclude 'deploy/nginx.test.conf' \
  --exclude '.env' \
  "${ROOT_DIR}/" "${REMOTE_USER}@${REMOTE_HOST}:${REMOTE_DIR}/"

echo "[deploy-server] Building n2sac-server:latest locally (fast BuildKit)..."
docker build -f "${ROOT_DIR}/Dockerfile" -t n2sac-server:latest "${ROOT_DIR}"

echo "[deploy-server] Streaming pre-built n2sac-server:latest image to ${REMOTE_USER}@${REMOTE_HOST}..."
docker save n2sac-server:latest | gzip | ssh "${REMOTE_USER}@${REMOTE_HOST}" "gunzip | docker load && cd '${REMOTE_DIR}' && docker compose up -d --no-deps --no-build nsac-server"

echo "[deploy-server] Verifying /api/v1/health..."
sleep 3
curl -fsS "http://${REMOTE_HOST}:3000/api/v1/health" && echo ""
echo "[deploy-server] Done!"
