#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REMOTE_USER="${N2SAC_USER:-criper}"
REMOTE_HOST="${N2SAC_HOST:-100.69.91.121}"
REMOTE_DIR="${N2SAC_REMOTE_DIR:-/opt/n2sac}"

echo "[deploy-all] Syncing full project to ${REMOTE_USER}@${REMOTE_HOST}:${REMOTE_DIR}..."
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

echo "[deploy-all] Building n2sac-server:latest and n2sac-web:latest locally (fast BuildKit)..."
docker build -f "${ROOT_DIR}/Dockerfile" -t n2sac-server:latest "${ROOT_DIR}"
docker build -f "${ROOT_DIR}/Dockerfile.web" -t n2sac-web:latest "${ROOT_DIR}"

echo "[deploy-all] Streaming pre-built images to ${REMOTE_USER}@${REMOTE_HOST} & restarting stack (--no-build)..."
docker save n2sac-server:latest n2sac-web:latest | gzip | ssh "${REMOTE_USER}@${REMOTE_HOST}" "
  set -euo pipefail
  gunzip | docker load
  rm -rf /tmp/n2sac-downloads-backup /tmp/n2sac-prev-assets
  docker cp n2sac-web:/usr/share/nginx/html/downloads /tmp/n2sac-downloads-backup 2>/dev/null || true
  docker cp n2sac-web:/usr/share/nginx/html/assets /tmp/n2sac-prev-assets 2>/dev/null || true
  cd '${REMOTE_DIR}'
  docker compose up -d --remove-orphans --no-build
  if [ -d /tmp/n2sac-prev-assets ]; then
    docker cp /tmp/n2sac-prev-assets/. n2sac-web:/usr/share/nginx/html/assets/ 2>/dev/null || true
    rm -rf /tmp/n2sac-prev-assets
  fi
  if [ -d /tmp/n2sac-downloads-backup ]; then
    docker cp /tmp/n2sac-downloads-backup n2sac-web:/usr/share/nginx/html/downloads
    rm -rf /tmp/n2sac-downloads-backup
  fi
"

sleep 4
"${ROOT_DIR}/scripts/server-status.sh"
