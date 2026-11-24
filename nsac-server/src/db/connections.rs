use chrono::{DateTime, Utc};
use sqlx::{FromRow, PgPool};

use crate::connections::{AccountConnectionDto, PublicConnectionDto, VerificationSource};
use crate::error::ServerError;

#[derive(Clone)]
pub struct ConnectionRepo {
    pool: PgPool,
}

#[derive(Debug, Clone)]
pub struct UpsertConnectionParams<'a> {
    pub account_id: i64,
    pub provider: &'a str,
    pub external_id: Option<&'a str>,
    pub handle: &'a str,
    pub display_name: Option<&'a str>,
    pub profile_url: Option<&'a str>,
    pub verification_source: VerificationSource,
    pub is_verified: bool,
    pub is_visible: bool,
}

#[derive(Debug, FromRow)]
struct ConnectionRow {
    provider: String,
    handle: String,
    display_name: Option<String>,
    profile_url: Option<String>,
    verification_source: String,
    is_verified: bool,
    is_visible: bool,
    display_order: i32,
    updated_at: DateTime<Utc>,
}

#[derive(Debug, FromRow)]
pub struct ConsumedOAuthState {
    pub account_id: i64,
    pub provider: String,
    pub pkce_verifier_ciphertext: Option<String>,
    pub client_platform: String,
}

impl ConnectionRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn list_for_account(&self, account_id: i64) -> Result<Vec<AccountConnectionDto>, ServerError> {
        let rows = sqlx::query_as::<_, ConnectionRow>(
            r#"
            SELECT provider, handle, display_name, profile_url,
                   verification_source, is_verified, is_visible, display_order, updated_at
            FROM account_connections
            WHERE account_id = $1
            ORDER BY display_order ASC, id ASC
            "#,
        )
        .bind(account_id)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .into_iter()
            .map(|r| AccountConnectionDto {
                provider: r.provider,
                handle: r.handle,
                display_name: r.display_name,
                profile_url: r.profile_url,
                verification_source: VerificationSource::parse(&r.verification_source),
                is_verified: r.is_verified,
                is_visible: r.is_visible,
                display_order: r.display_order,
                updated_at: r.updated_at,
            })
            .collect())
    }

    pub async fn list_public_for_account(
        &self,
        account_id: i64,
    ) -> Result<Vec<PublicConnectionDto>, ServerError> {
        let rows = sqlx::query_as::<_, ConnectionRow>(
            r#"
            SELECT provider, handle, display_name, profile_url,
                   verification_source, is_verified, is_visible, display_order, updated_at
            FROM account_connections
            WHERE account_id = $1 AND is_visible = TRUE
            ORDER BY display_order ASC, id ASC
            "#,
        )
        .bind(account_id)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .into_iter()
            .map(|r| PublicConnectionDto {
                provider: r.provider,
                handle: r.handle,
                display_name: r.display_name,
                profile_url: r.profile_url,
                verification_source: VerificationSource::parse(&r.verification_source),
                is_verified: r.is_verified,
            })
            .collect())
    }

    pub async fn count_for_account(&self, account_id: i64) -> Result<i64, ServerError> {
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM account_connections WHERE account_id = $1",
        )
        .bind(account_id)
        .fetch_one(&self.pool)
        .await?;
        Ok(count)
    }

    pub async fn exists_for_provider(
        &self,
        account_id: i64,
        provider: &str,
    ) -> Result<bool, ServerError> {
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM account_connections WHERE account_id = $1 AND provider = $2)",
        )
        .bind(account_id)
        .bind(provider)
        .fetch_one(&self.pool)
        .await?;
        Ok(exists)
    }

    pub async fn upsert(&self, params: UpsertConnectionParams<'_>) -> Result<AccountConnectionDto, ServerError> {
        let row = sqlx::query_as::<_, ConnectionRow>(
            r#"
            INSERT INTO account_connections (
                account_id, provider, external_id, handle, display_name, profile_url,
                verification_source, is_verified, is_visible, display_order, updated_at
            )
            VALUES (
                $1, $2, $3, $4, $5, $6, $7, $8, $9,
                COALESCE((SELECT MAX(display_order) + 1 FROM account_connections WHERE account_id = $1), 0),
                NOW()
            )
            ON CONFLICT (account_id, provider) DO UPDATE SET
                external_id = EXCLUDED.external_id,
                handle = EXCLUDED.handle,
                display_name = EXCLUDED.display_name,
                profile_url = EXCLUDED.profile_url,
                verification_source = EXCLUDED.verification_source,
                is_verified = EXCLUDED.is_verified,
                is_visible = EXCLUDED.is_visible,
                updated_at = NOW()
            RETURNING provider, handle, display_name, profile_url,
                      verification_source, is_verified, is_visible, display_order, updated_at
            "#,
        )
        .bind(params.account_id)
        .bind(params.provider)
        .bind(params.external_id)
        .bind(params.handle)
        .bind(params.display_name)
        .bind(params.profile_url)
        .bind(params.verification_source.as_str())
        .bind(params.is_verified)
        .bind(params.is_visible)
        .fetch_one(&self.pool)
        .await?;

        Ok(AccountConnectionDto {
            provider: row.provider,
            handle: row.handle,
            display_name: row.display_name,
            profile_url: row.profile_url,
            verification_source: VerificationSource::parse(&row.verification_source),
            is_verified: row.is_verified,
            is_visible: row.is_visible,
            display_order: row.display_order,
            updated_at: row.updated_at,
        })
    }

    pub async fn set_visibility(
        &self,
        account_id: i64,
        provider: &str,
        is_visible: bool,
    ) -> Result<Option<AccountConnectionDto>, ServerError> {
        let row = sqlx::query_as::<_, ConnectionRow>(
            r#"
            UPDATE account_connections
            SET is_visible = $3, updated_at = NOW()
            WHERE account_id = $1 AND provider = $2
            RETURNING provider, handle, display_name, profile_url,
                      verification_source, is_verified, is_visible, display_order, updated_at
            "#,
        )
        .bind(account_id)
        .bind(provider)
        .bind(is_visible)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(|r| AccountConnectionDto {
            provider: r.provider,
            handle: r.handle,
            display_name: r.display_name,
            profile_url: r.profile_url,
            verification_source: VerificationSource::parse(&r.verification_source),
            is_verified: r.is_verified,
            is_visible: r.is_visible,
            display_order: r.display_order,
            updated_at: r.updated_at,
        }))
    }

    pub async fn delete(&self, account_id: i64, provider: &str) -> Result<bool, ServerError> {
        let result = sqlx::query(
            "DELETE FROM account_connections WHERE account_id = $1 AND provider = $2",
        )
        .bind(account_id)
        .bind(provider)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() > 0)
    }

    pub async fn create_oauth_state(
        &self,
        state_hash: &str,
        account_id: i64,
        provider: &str,
        pkce_verifier_ciphertext: Option<&str>,
        client_platform: &str,
        expires_at: DateTime<Utc>,
    ) -> Result<(), ServerError> {
        // Best-effort opportunistic cleanup of expired states.
        let _ = sqlx::query("DELETE FROM oauth_link_states WHERE expires_at < NOW()")
            .execute(&self.pool)
            .await;

        sqlx::query(
            r#"
            INSERT INTO oauth_link_states (
                state_hash, account_id, provider, pkce_verifier_ciphertext, client_platform, expires_at
            )
            VALUES ($1, $2, $3, $4, $5, $6)
            "#,
        )
        .bind(state_hash)
        .bind(account_id)
        .bind(provider)
        .bind(pkce_verifier_ciphertext)
        .bind(client_platform)
        .bind(expires_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Atomically consumes a single-use OAuth state if it has not expired.
    pub async fn consume_oauth_state(
        &self,
        state_hash: &str,
        expected_provider: &str,
    ) -> Result<Option<ConsumedOAuthState>, ServerError> {
        let row = sqlx::query_as::<_, ConsumedOAuthState>(
            r#"
            DELETE FROM oauth_link_states
            WHERE state_hash = $1 AND provider = $2 AND expires_at > NOW()
            RETURNING account_id, provider, pkce_verifier_ciphertext, client_platform
            "#,
        )
        .bind(state_hash)
        .bind(expected_provider)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
    }
}
