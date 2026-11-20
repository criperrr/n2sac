#!/usr/bin/env bash
set -euo pipefail

REMOTE_USER="${N2SAC_USER:-criper}"
REMOTE_HOST="${N2SAC_HOST:-192.168.1.150}"
MODE="${1:---summary}"

case "$MODE" in
  --summary)
    echo "=== Telemetry Summary (${REMOTE_HOST}) ==="
    ssh "${REMOTE_USER}@${REMOTE_HOST}" 'docker exec n2sac-postgres psql -U n2sac -d nsac_server -c "
      SELECT
        event_type,
        platform,
        COUNT(*) AS total_events,
        ROUND(AVG((metrics_json->>'\''fps_avg'\'')::numeric), 1) AS avg_fps,
        ROUND(AVG((metrics_json->>'\''jank_frames'\'')::numeric), 1) AS avg_jank,
        MAX(created_at) AS last_seen
      FROM telemetry_events
      GROUP BY event_type, platform
      ORDER BY last_seen DESC;
    "'
    ;;
  --hardware)
    echo "=== Performance by Hardware (${REMOTE_HOST}) ==="
    ssh "${REMOTE_USER}@${REMOTE_HOST}" 'docker exec n2sac-postgres psql -U n2sac -d nsac_server -c "
      SELECT
        COALESCE(hardware_json->>'\''gpu_renderer'\'', '\''unknown'\'') AS gpu,
        hardware_json->>'\''cpu_cores'\'' AS cores,
        hardware_json->>'\''device_memory_gb'\'' AS ram_gb,
        COUNT(*) AS samples,
        ROUND(AVG((metrics_json->>'\''fps_avg'\'')::numeric), 1) AS avg_fps,
        ROUND(MIN((metrics_json->>'\''fps_min'\'')::numeric), 1) AS min_fps,
        ROUND(AVG((metrics_json->>'\''js_heap_used_mb'\'')::numeric), 1) AS heap_mb
      FROM telemetry_events
      WHERE event_type = '\''performance'\''
      GROUP BY 1, 2, 3
      ORDER BY samples DESC;
    "'
    ;;
  --crashes)
    echo "=== Recent UI Crashes (${REMOTE_HOST}) ==="
    ssh "${REMOTE_USER}@${REMOTE_HOST}" 'docker exec n2sac-postgres psql -U n2sac -d nsac_server -c "
      SELECT
        id,
        created_at,
        platform,
        app_version,
        crash_json->>'\''name'\'' AS error_name,
        crash_json->>'\''message'\'' AS error_message,
        crash_json->>'\''route'\'' AS route
      FROM telemetry_events
      WHERE event_type = '\''crash'\''
      ORDER BY created_at DESC
      LIMIT 30;
    "'
    ;;
  --json)
    OUT_FILE="telemetry-export-$(date +%Y%m%d-%H%M%S).json"
    ssh "${REMOTE_USER}@${REMOTE_HOST}" 'docker exec n2sac-postgres psql -U n2sac -d nsac_server -t -A -c "
      SELECT json_agg(row_to_json(t)) FROM (
        SELECT * FROM telemetry_events ORDER BY created_at DESC LIMIT 1000
      ) t;
    "' > "${OUT_FILE}"
    echo "Exported to ${OUT_FILE}"
    ;;
  *)
    echo "Usage: $(basename "$0") [--summary | --hardware | --crashes | --json]"
    exit 1
    ;;
esac
