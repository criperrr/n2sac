use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::{FromRow, PgPool};

use crate::db::auth::sha256_bytes_hex;
use crate::error::ServerError;

pub fn compute_strong_etag<T: Serialize>(value: &T) -> Result<String, ServerError> {
    let bytes = serde_json::to_vec(value)?;
    Ok(format!("\"{}\"", sha256_bytes_hex(&bytes)))
}

#[derive(Debug, Clone, PartialEq)]
pub struct SnapshotRecord {
    pub id: i64,
    pub account_id: i64,
    pub route_key: String,
    pub payload_json: serde_json::Value,
    pub etag: String,
    pub updated_at: DateTime<Utc>,
}

#[derive(FromRow)]
struct SnapshotRow {
    id: i64,
    account_id: i64,
    route_key: String,
    payload_json: serde_json::Value,
    etag: String,
    updated_at: DateTime<Utc>,
}

impl From<SnapshotRow> for SnapshotRecord {
    fn from(row: SnapshotRow) -> Self {
        Self {
            id: row.id,
            account_id: row.account_id,
            route_key: row.route_key,
            payload_json: row.payload_json,
            etag: row.etag,
            updated_at: row.updated_at,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SnapshotRepo {
    pool: PgPool,
}

impl SnapshotRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn upsert(
        &self,
        account_id: i64,
        route_key: &str,
        payload_json: &serde_json::Value,
        etag: &str,
        now: DateTime<Utc>,
    ) -> Result<SnapshotRecord, ServerError> {
        let row = sqlx::query_as::<_, SnapshotRow>(
            r#"
            INSERT INTO auxiliary_snapshots (
                account_id,
                route_key,
                payload_json,
                etag,
                updated_at
            )
            VALUES ($1, $2, $3, $4, $5)
            ON CONFLICT (account_id, route_key) DO UPDATE SET
                payload_json = EXCLUDED.payload_json,
                etag = EXCLUDED.etag,
                updated_at = EXCLUDED.updated_at
            RETURNING id, account_id, route_key, payload_json, etag, updated_at
            "#,
        )
        .bind(account_id)
        .bind(route_key)
        .bind(payload_json)
        .bind(etag)
        .bind(now)
        .fetch_one(&self.pool)
        .await?;

        Ok(row.into())
    }

    pub async fn get(
        &self,
        account_id: i64,
        route_key: &str,
    ) -> Result<Option<SnapshotRecord>, ServerError> {
        let row = sqlx::query_as::<_, SnapshotRow>(
            r#"
            SELECT id, account_id, route_key, payload_json, etag, updated_at
            FROM auxiliary_snapshots
            WHERE account_id = $1 AND route_key = $2
            "#,
        )
        .bind(account_id)
        .bind(route_key)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(Into::into))
    }

    pub async fn delete_for_account(&self, account_id: i64) -> Result<u64, ServerError> {
        let res = sqlx::query(
            r#"
            DELETE FROM auxiliary_snapshots
            WHERE account_id = $1
            "#,
        )
        .bind(account_id)
        .execute(&self.pool)
        .await?;

        Ok(res.rows_affected())
    }
}
