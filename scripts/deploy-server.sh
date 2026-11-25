#!/usr/bin/env bash
# Image-only backend deploy for n²sac (zero source rsync to remote server).
# Builds n2sac-server:latest locally, streams it via SSH, and restarts only
# the nsac-server container inside /opt/n2sac/docker-compose.yml (--no-build).
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REMOTE_USER="${N2SAC_USER:-criper}"
REMOTE_HOST="${N2SAC_HOST:-100.69.91.121}"
REMOTE_DIR="${N2SAC_REMOTE_DIR:-/opt/n2sac}"
DRY_RUN="false"

if [ "${1:-}" = "--dry-run" ]; then
  DRY_RUN="true"
fi

if [ "${DRY_RUN}" = "true" ]; then
  echo "[deploy-server] DRY-RUN: verifying Dockerfile and remote compose (${REMOTE_USER}@${REMOTE_HOST}:${REMOTE_DIR})..."
  test -f "${ROOT_DIR}/Dockerfile"
  ssh -T -o BatchMode=yes -o LogLevel=ERROR "${REMOTE_USER}@${REMOTE_HOST}" \
    "cd '${REMOTE_DIR}' && docker compose --dry-run up -d --no-deps --no-build nsac-server"
  echo "[deploy-server] DRY-RUN OK (zero source rsync, image-only deploy)."
  exit 0
fi

echo "[deploy-server] Building n2sac-server:latest locally (fast BuildKit)..."
docker build -f "${ROOT_DIR}/Dockerfile" -t n2sac-server:latest "${ROOT_DIR}"

echo "[deploy-server] Streaming pre-built n2sac-server:latest image to ${REMOTE_USER}@${REMOTE_HOST} (no source rsync)..."
docker save n2sac-server:latest | gzip | ssh -T -o LogLevel=ERROR "${REMOTE_USER}@${REMOTE_HOST}" "
  set -euo pipefail
  gunzip | docker load
  cd '${REMOTE_DIR}'
  docker compose up -d --no-deps --no-build nsac-server
"

PUBLIC_URL="${N2SAC_PUBLIC_URL:-https://notebook-server.tailddd6bf.ts.net}"
echo "[deploy-server] Verifying ${PUBLIC_URL}/api/v1/health..."
sleep 3
curl -fsS --max-time 10 "${PUBLIC_URL}/api/v1/health" && echo ""
echo "[deploy-server] Done!"
