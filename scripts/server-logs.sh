#!/usr/bin/env bash
set -euo pipefail

REMOTE_USER="${N2SAC_USER:-criper}"
REMOTE_HOST="${N2SAC_HOST:-100.69.91.121}"
REMOTE_DIR="${N2SAC_REMOTE_DIR:-/opt/n2sac}"

MODE="normal"
FOLLOW=""
TAIL_LINES="120"

for arg in "$@"; do
  case "$arg" in
    --crash)        MODE="crash" ;;
    --warn)         MODE="warn" ;;
    --normal)       MODE="normal" ;;
    --deep)         MODE="deep" ;;
    --enable-deep)  MODE="enable-deep" ;;
    --disable-deep) MODE="disable-deep" ;;
    -f|--follow)    FOLLOW="-f" ;;
    --tail=*)       TAIL_LINES="${arg#*=}" ;;
    -h|--help)
      cat <<EOF
Usage: $(basename "$0") [OPTIONS]

Log inspection using Rust tracing/EnvFilter on ${REMOTE_HOST}:
  --crash         Show only fatal errors, ERROR level, and panics
  --warn          Show WARN and ERROR events
  --normal        Show standard INFO+ events (default)
  --deep          Show all DEBUG/TRACE events from nsac-server
  --enable-deep   Restart remote nsac-server with RUST_LOG=debug,nsac_server=trace,nsac_core=trace
  --disable-deep  Restore remote nsac-server to standard RUST_LOG=info,nsac_server=info,nsac_core=info
  -f, --follow    Stream logs live
  --tail=N        Number of recent lines (default: 120)
EOF
      exit 0
      ;;
  esac
done

if [ "$MODE" = "enable-deep" ]; then
  echo "[server-logs] Enabling deep tracing (RUST_LOG=debug,nsac_server=trace,nsac_core=trace)..."
  ssh "${REMOTE_USER}@${REMOTE_HOST}" "cd '${REMOTE_DIR}' && RUST_LOG=debug,nsac_server=trace,nsac_core=trace docker compose up -d --no-deps nsac-server"
  exit 0
fi

if [ "$MODE" = "disable-deep" ]; then
  echo "[server-logs] Restoring standard logging (RUST_LOG=info,nsac_server=info,nsac_core=info)..."
  ssh "${REMOTE_USER}@${REMOTE_HOST}" "cd '${REMOTE_DIR}' && RUST_LOG=info,nsac_server=info,nsac_core=info docker compose up -d --no-deps nsac-server"
  exit 0
fi

case "$MODE" in
  crash)
    ssh "${REMOTE_USER}@${REMOTE_HOST}" "docker logs ${FOLLOW} --tail ${TAIL_LINES} n2sac-server 2>&1" | grep -iE "ERROR|panic|fatal|unhealthy|failed" || true
    ;;
  warn)
    ssh "${REMOTE_USER}@${REMOTE_HOST}" "docker logs ${FOLLOW} --tail ${TAIL_LINES} n2sac-server 2>&1" | grep -iE "ERROR|WARN|panic|fatal" || true
    ;;
  normal|deep)
    ssh "${REMOTE_USER}@${REMOTE_HOST}" "docker logs ${FOLLOW} --tail ${TAIL_LINES} n2sac-server 2>&1"
    ;;
esac
