use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::state::AppState;

const TELEMETRY_WINDOW: Duration = Duration::from_secs(60);
const TELEMETRY_MAX_PER_DEVICE_PER_MIN: usize = 12;
const TELEMETRY_MAX_GLOBAL_PER_MIN: usize = 180;
const TELEMETRY_MAX_TRACKED_DEVICES: usize = 2_048;

struct TelemetryRateState {
    global: VecDeque<Instant>,
    per_device: HashMap<String, VecDeque<Instant>>,
}

fn check_telemetry_rate_limit(device_key: &str) -> bool {
    static LIMITER: OnceLock<Mutex<TelemetryRateState>> = OnceLock::new();
    let mutex = LIMITER.get_or_init(|| {
        Mutex::new(TelemetryRateState {
            global: VecDeque::new(),
            per_device: HashMap::new(),
        })
    });
    let mut state = mutex.lock().unwrap_or_else(|e| e.into_inner());
    let now = Instant::now();

    while let Some(&front) = state.global.front() {
        if now.duration_since(front) >= TELEMETRY_WINDOW {
            state.global.pop_front();
        } else {
            break;
        }
    }
    if state.global.len() >= TELEMETRY_MAX_GLOBAL_PER_MIN {
        return false;
    }

    if state.per_device.len() >= TELEMETRY_MAX_TRACKED_DEVICES {
        state.per_device.retain(|_, q| {
            while let Some(&front) = q.front() {
                if now.duration_since(front) >= TELEMETRY_WINDOW {
                    q.pop_front();
                } else {
                    break;
                }
            }
            !q.is_empty()
        });
    }

    let queue = state.per_device.entry(device_key.to_string()).or_default();
    while let Some(&front) = queue.front() {
        if now.duration_since(front) >= TELEMETRY_WINDOW {
            queue.pop_front();
        } else {
            break;
        }
    }
    if queue.len() >= TELEMETRY_MAX_PER_DEVICE_PER_MIN {
        return false;
    }

    queue.push_back(now);
    state.global.push_back(now);
    true
}

#[derive(Debug, Deserialize)]
pub struct TelemetryIngestRequest {
    pub event_type: String,
    #[serde(default)]
    pub app_version: String,
    #[serde(default)]
    pub platform: String,
    #[serde(default)]
    pub device_fingerprint: String,
    #[serde(default)]
    pub hardware: Value,
    #[serde(default)]
    pub metrics: Value,
    #[serde(default)]
    pub crash: Option<Value>,
}

#[derive(Debug, Serialize)]
pub struct TelemetryIngestResponse {
    pub success: bool,
    pub id: i64,
}

pub async fn ingest_telemetry(
    State(state): State<AppState>,
    Json(payload): Json<TelemetryIngestRequest>,
) -> Result<(StatusCode, Json<TelemetryIngestResponse>), StatusCode> {
    let event_type = match payload.event_type.as_str() {
        "performance" | "crash" => payload.event_type.as_str(),
        _ => return Err(StatusCode::BAD_REQUEST),
    };

    let fp_key: String = {
        let trimmed = payload.device_fingerprint.trim();
        if trimmed.is_empty() {
            "anonymous".to_string()
        } else {
            trimmed.chars().take(128).collect()
        }
    };
    if !check_telemetry_rate_limit(&fp_key) {
        return Err(StatusCode::TOO_MANY_REQUESTS);
    }

    let row: (i64,) = sqlx::query_as(
        r#"
        INSERT INTO telemetry_events (
            event_type,
            app_version,
            platform,
            device_fingerprint,
            hardware_json,
            metrics_json,
            crash_json
        )
        VALUES ($1, $2, $3, $4, $5, $6, $7)
        RETURNING id
        "#,
    )
    .bind(event_type)
    .bind(payload.app_version.chars().take(64).collect::<String>())
    .bind(payload.platform.chars().take(64).collect::<String>())
    .bind(&fp_key)
    .bind(&payload.hardware)
    .bind(&payload.metrics)
    .bind(&payload.crash)
    .fetch_one(&state.db)
    .await
    .map_err(|err| {
        tracing::error!(error = %err, "Failed to insert telemetry event");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    if event_type == "crash" {
        tracing::error!(
            telemetry_id = row.0,
            platform = %payload.platform,
            app_version = %payload.app_version,
            "Client UI crash reported via telemetry"
        );
    } else {
        tracing::debug!(
            telemetry_id = row.0,
            platform = %payload.platform,
            "Client performance telemetry ingested"
        );
    }

    Ok((
        StatusCode::CREATED,
        Json(TelemetryIngestResponse {
            success: true,
            id: row.0,
        }),
    ))
}
