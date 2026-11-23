#!/usr/bin/env bash
# Standalone local administrative DB & Election Panel injector for n²sac.
# Completely separated from app/ and nsac-server/ (never shipped in production).
#
# Usage:
#   ./scripts/admin-db-panel/inject.sh            # start server (if needed) + inject panel
#   ./scripts/admin-db-panel/inject.sh --reset    # also reset local DB to User (#1) + 1 bot (#2)
set -euo pipefail

DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PORT="${DB_CONTROL_PORT:-18081}"
LOG_FILE="${DIR}/.server.log"

if ! curl -sf "http://127.0.0.1:${PORT}/state" >/dev/null 2>&1; then
  echo "[admin-db-panel] Starting local DB control server on 127.0.0.1:${PORT}..."
  setsid nohup node "${DIR}/server.mjs" </dev/null >"${LOG_FILE}" 2>&1 &
  for _ in $(seq 1 20); do
    curl -sf "http://127.0.0.1:${PORT}/state" >/dev/null 2>&1 && break
    sleep 0.25
  done
fi

node "${DIR}/inject.mjs" "$@"
