use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::state::AppState;

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

async fn ensure_telemetry_table(pool: &sqlx::PgPool) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS telemetry_events (
            id BIGSERIAL PRIMARY KEY,
            event_type TEXT NOT NULL,
            app_version TEXT NOT NULL DEFAULT '',
            platform TEXT NOT NULL DEFAULT '',
            device_fingerprint TEXT NOT NULL DEFAULT '',
            hardware_json JSONB NOT NULL DEFAULT '{}'::jsonb,
            metrics_json JSONB NOT NULL DEFAULT '{}'::jsonb,
            crash_json JSONB,
            created_at TIMESTAMPTZ NOT NULL DEFAULT now()
        )
        "#,
    )
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn ingest_telemetry(
    State(state): State<AppState>,
    Json(payload): Json<TelemetryIngestRequest>,
) -> Result<(StatusCode, Json<TelemetryIngestResponse>), StatusCode> {
    let event_type = match payload.event_type.as_str() {
        "performance" | "crash" => payload.event_type.as_str(),
        _ => return Err(StatusCode::BAD_REQUEST),
    };

    ensure_telemetry_table(&state.db).await.map_err(|err| {
        tracing::error!(error = %err, "Failed to ensure telemetry_events table");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

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
    .bind(payload.device_fingerprint.chars().take(128).collect::<String>())
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
