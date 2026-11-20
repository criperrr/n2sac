use std::fmt;

use chrono::{DateTime, Duration, Utc};
use rand::RngExt;
use sha2::{Digest, Sha256};
use sqlx::{FromRow, PgPool};
use zeroize::Zeroizing;

use crate::config::ServiceKeySeed;
use crate::error::ServerError;

pub fn sha256_bytes_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

pub fn sha256_hex(raw: &str) -> String {
    sha256_bytes_hex(raw.as_bytes())
}

fn generate_opaque_token() -> Zeroizing<String> {
    let mut bytes = [0u8; 32];
    rand::rng().fill(&mut bytes);
    Zeroizing::new(hex::encode(bytes))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceClientRecord {
    pub id: i64,
    pub name: String,
    pub scopes: Vec<String>,
    pub active: bool,
}

#[derive(Clone)]
pub struct IssuedSession {
    pub session_id: i64,
    pub account_id: i64,
    pub token: Zeroizing<String>,
    pub expires_at: DateTime<Utc>,
}

impl fmt::Debug for IssuedSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("IssuedSession")
            .field("session_id", &self.session_id)
            .field("account_id", &self.account_id)
            .field("token", &"[REDACTED]")
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserSessionLookup {
    pub session_id: i64,
    pub account_id: i64,
    pub expires_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub account_active: bool,
}

#[derive(FromRow)]
struct ServiceClientRow {
    id: i64,
    name: String,
    scopes: Vec<String>,
    active: bool,
}

#[derive(FromRow)]
struct UserSessionRow {
    session_id: i64,
    account_id: i64,
    expires_at: DateTime<Utc>,
    revoked_at: Option<DateTime<Utc>>,
    account_active: bool,
}

#[derive(Debug, Clone)]
pub struct AuthRepo {
    pool: PgPool,
}

impl AuthRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn seed_service_clients(
        &self,
        seeds: &[ServiceKeySeed],
    ) -> Result<usize, ServerError> {
        if seeds.is_empty() {
            return Ok(0);
        }

        let mut tx = self.pool.begin().await?;
        let mut count = 0usize;

        for seed in seeds {
            let key_hash = sha256_hex(seed.raw_key.as_str());
            sqlx::query(
                r#"
                INSERT INTO service_clients (name, key_hash, scopes, active)
                VALUES ($1, $2, $3, TRUE)
                ON CONFLICT (name) DO UPDATE SET
                    key_hash = EXCLUDED.key_hash,
                    scopes = EXCLUDED.scopes,
                    active = TRUE
                "#,
            )
            .bind(&seed.name)
            .bind(&key_hash)
            .bind(&seed.scopes)
            .execute(&mut *tx)
            .await?;
            count += 1;
        }

        tx.commit().await?;
        Ok(count)
    }

    pub async fn find_service_client_by_raw_key(
        &self,
        raw_key: &str,
    ) -> Result<Option<ServiceClientRecord>, ServerError> {
        let key_hash = sha256_hex(raw_key);
        let row = sqlx::query_as::<_, ServiceClientRow>(
            r#"
            SELECT id, name, scopes, active
            FROM service_clients
            WHERE key_hash = $1
            "#,
        )
        .bind(&key_hash)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(|r| ServiceClientRecord {
            id: r.id,
            name: r.name,
            scopes: r.scopes,
            active: r.active,
        }))
    }

    pub async fn create_user_session(
        &self,
        account_id: i64,
        ttl_secs: u64,
        now: DateTime<Utc>,
    ) -> Result<IssuedSession, ServerError> {
        let ttl_i64 = i64::try_from(ttl_secs.max(1)).unwrap_or(2_592_000);
        let expires_at = now + Duration::seconds(ttl_i64);
        let token = generate_opaque_token();
        let token_hash = sha256_hex(token.as_str());

        let session_id: i64 = sqlx::query_scalar(
            r#"
            INSERT INTO user_sessions (account_id, token_hash, expires_at, created_at)
            VALUES ($1, $2, $3, $4)
            RETURNING id
            "#,
        )
        .bind(account_id)
        .bind(&token_hash)
        .bind(expires_at)
        .bind(now)
        .fetch_one(&self.pool)
        .await?;

        Ok(IssuedSession {
            session_id,
            account_id,
            token,
            expires_at,
        })
    }

    pub async fn find_user_session_by_raw_token(
        &self,
        raw_token: &str,
    ) -> Result<Option<UserSessionLookup>, ServerError> {
        let token_hash = sha256_hex(raw_token);
        let row = sqlx::query_as::<_, UserSessionRow>(
            r#"
            SELECT
                s.id AS session_id,
                s.account_id,
                s.expires_at,
                s.revoked_at,
                a.active AS account_active
            FROM user_sessions s
            JOIN accounts a ON a.id = s.account_id
            WHERE s.token_hash = $1
            "#,
        )
        .bind(&token_hash)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(|r| UserSessionLookup {
            session_id: r.session_id,
            account_id: r.account_id,
            expires_at: r.expires_at,
            revoked_at: r.revoked_at,
            account_active: r.account_active,
        }))
    }

    pub async fn revoke_session_by_raw_token(
        &self,
        raw_token: &str,
        now: DateTime<Utc>,
    ) -> Result<bool, ServerError> {
        let token_hash = sha256_hex(raw_token);
        let res = sqlx::query(
            r#"
            UPDATE user_sessions
            SET revoked_at = $2
            WHERE token_hash = $1 AND revoked_at IS NULL
            "#,
        )
        .bind(&token_hash)
        .bind(now)
        .execute(&self.pool)
        .await?;

        Ok(res.rows_affected() > 0)
    }

    pub async fn revoke_all_for_account(
        &self,
        account_id: i64,
        now: DateTime<Utc>,
    ) -> Result<u64, ServerError> {
        let res = sqlx::query(
            r#"
            UPDATE user_sessions
            SET revoked_at = $2
            WHERE account_id = $1 AND revoked_at IS NULL
            "#,
        )
        .bind(account_id)
        .bind(now)
        .execute(&self.pool)
        .await?;

        Ok(res.rows_affected())
    }
}
