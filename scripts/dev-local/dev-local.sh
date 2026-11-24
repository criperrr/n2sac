#!/usr/bin/env bash
# Local n²sac test environment: Postgres + nsac-server + web build served to the phone.
#
#   scripts/dev-local/dev-local.sh up        start everything and seed test users
#   scripts/dev-local/dev-local.sh reset     wipe the database and seed again
#   scripts/dev-local/dev-local.sh web       rebuild the web app (after code changes)
#   scripts/dev-local/dev-local.sh down      stop everything (data is kept)
#   scripts/dev-local/dev-local.sh status
#
# The phone needs the test APK (UI from http://localhost:4173) and USB/adb; ports 4173 (web)
# and 18080 (API) are forwarded with `adb reverse`. Synthetic data only, never production.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
FRONT_DIR="${N2SAC_FRONT_DIR:-$ROOT/../front}"
HERE="$ROOT/scripts/dev-local"
RUN="$HERE/.run"
mkdir -p "$RUN"

PG_CONTAINER=n2sac-dev-pg
PG_PORT=55433
API_PORT=18080
WEB_PORT=4173
DB_URL="postgres://postgres@127.0.0.1:$PG_PORT/n2sac_dev"

log() { printf '\033[1;36m▸ %s\033[0m\n' "$*"; }

stop_port() {
  local pid
  pid=$(ss -ltnp "sport = :$1" 2>/dev/null | grep -oP 'pid=\K[0-9]+' | head -1 || true)
  if [ -n "$pid" ]; then kill "$pid" 2>/dev/null || true; fi
}

start_db() {
  if ! docker ps --format '{{.Names}}' | grep -qx "$PG_CONTAINER"; then
    if docker ps -a --format '{{.Names}}' | grep -qx "$PG_CONTAINER"; then
      docker start "$PG_CONTAINER" >/dev/null
    else
      docker run -d --name "$PG_CONTAINER" -e POSTGRES_HOST_AUTH_METHOD=trust \
        -p "127.0.0.1:$PG_PORT:5432" -v n2sac-dev-pgdata:/var/lib/postgresql/data postgres:16-alpine >/dev/null
    fi
  fi
  for _ in $(seq 1 30); do
    docker exec "$PG_CONTAINER" pg_isready -U postgres >/dev/null 2>&1 && break
    sleep 1
  done
  docker exec "$PG_CONTAINER" psql -U postgres -tc "SELECT 1 FROM pg_database WHERE datname='n2sac_dev'" | grep -q 1 \
    || docker exec "$PG_CONTAINER" createdb -U postgres n2sac_dev
}

start_server() {
  stop_port "$API_PORT"
  log "Compilando nsac-server"
  (cd "$ROOT" && cargo build -q -p nsac-server --bin nsac-server)
  HTTP_BIND="127.0.0.1:$API_PORT" \
  RUST_LOG=info,sqlx=warn,tower_http=debug \
  DATABASE_URL="$DB_URL" \
  ENCRYPTION_KEY=0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef \
  NSAC_BASE_URL="${NSAC_BASE_URL:-http://200.145.153.1/nsac}" \
  SENTINEL_ENABLED=false \
  FCM_ENABLED=false \
  CACHE_AUTH_TTL_SECS=0 \
  GOOGLE_OAUTH_CLIENT_ID="${GOOGLE_OAUTH_CLIENT_ID:-}" \
    setsid nohup "$ROOT/target/debug/nsac-server" </dev/null >"$RUN/server.log" 2>&1 &
  for _ in $(seq 1 40); do
    curl -sf "http://127.0.0.1:$API_PORT/api/v1/health" >/dev/null && return 0
    sleep 0.5
  done
  echo "Servidor não respondeu; veja $RUN/server.log" >&2
  exit 1
}

seed() {
  log "Criando usuários de teste"
  docker exec -i "$PG_CONTAINER" psql -q -U postgres -d n2sac_dev -v ON_ERROR_STOP=1 <"$HERE/seed.sql"
  node "$HERE/dev.mjs" history
}

build_web() {
  log "Build web (API = http://localhost:$API_PORT)"
  (cd "$FRONT_DIR" && VITE_API_BASE_URL="http://localhost:$API_PORT" npm run build >"$RUN/web-build.log" 2>&1)
}

start_web() {
  stop_port "$WEB_PORT"
  (cd "$FRONT_DIR" && setsid nohup npx vite preview --port "$WEB_PORT" --strictPort --host 127.0.0.1 </dev/null >"$RUN/web.log" 2>&1 &)
  sleep 2
}

phone() {
  local serials
  serials=$(adb devices | awk 'NR>1 && $2=="device" {print $1}')
  if [ -n "$serials" ]; then
    for s in $serials; do
      adb -s "$s" reverse "tcp:$WEB_PORT" "tcp:$WEB_PORT" >/dev/null 2>&1 || true
      adb -s "$s" reverse "tcp:$API_PORT" "tcp:$API_PORT" >/dev/null 2>&1 || true
      log "adb reverse [$s]: $WEB_PORT (web) e $API_PORT (API)"
    done
  else
    echo "Celular não conectado: conecte e rode '$0 up' de novo (ou só 'adb reverse')." >&2
  fi
}

case "${1:-}" in
  up)
    start_db
    start_server
    if [ "$(docker exec "$PG_CONTAINER" psql -U postgres -d n2sac_dev -tAc 'SELECT COUNT(*) FROM accounts')" = "0" ]; then seed; fi
    build_web
    start_web
    phone
    log "Pronto. Login no celular: node scripts/dev-local/dev.mjs login voce · robôs: node scripts/dev-local/dev.mjs bots"
    ;;
  reset)
    start_db
    start_server
    seed
    ;;
  web)
    build_web
    start_web
    phone
    ;;
  down)
    stop_port "$API_PORT"
    stop_port "$WEB_PORT"
    docker stop "$PG_CONTAINER" >/dev/null 2>&1 || true
    log "Parado (dados mantidos no volume n2sac-dev-pgdata)"
    ;;
  status)
    docker ps --filter "name=$PG_CONTAINER" --format 'postgres: {{.Status}}'
    curl -sf "http://127.0.0.1:$API_PORT/api/v1/health" >/dev/null && echo "api: ok (:$API_PORT)" || echo "api: parado"
    curl -sf "http://127.0.0.1:$WEB_PORT/" >/dev/null && echo "web: ok (:$WEB_PORT)" || echo "web: parado"
    adb reverse --list 2>/dev/null || true
    ;;
  *)
    sed -n '2,12p' "$0"
    exit 1
    ;;
esac
