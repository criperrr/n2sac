use std::fmt;

use chrono::{DateTime, Utc};
use nsac_core::crypto::{CryptoService, SecretString};
use nsac_core::models::NsacSessionCookies;
use sqlx::{FromRow, PgPool};

use crate::error::ServerError;

pub const CTI_CLASSES: [&str; 24] = [
    "INI1", "INI2", "INI3", "INI1A", "INI1B", "INI2A", "INI2B", "INI3A", "INI3B", "ELI1", "ELI2",
    "ELI3", "MEI1", "MEI2", "MEI3", "INF1", "INF2", "INF3", "ELE1", "ELE2", "ELE3", "MEC1", "MEC2",
    "MEC3",
];

pub fn normalize_class_code(raw: &str) -> Option<String> {
    let normalized = raw.trim().to_ascii_uppercase();
    if normalized.is_empty() {
        None
    } else {
        Some(normalized)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountSummary {
    pub id: i64,
    pub nsac_email: String,
    pub class_code: Option<String>,
    pub has_fcm_token: bool,
    pub fcm_token: Option<String>,
    pub discord_id: Option<String>,
    pub active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Clone)]
pub struct DecryptedAccountSecrets {
    pub account_id: i64,
    pub nsac_email: String,
    pub password: SecretString,
    pub cookies: Option<NsacSessionCookies>,
    pub class_code: Option<String>,
    pub fcm_token: Option<String>,
    pub active: bool,
}

impl fmt::Debug for DecryptedAccountSecrets {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DecryptedAccountSecrets")
            .field("account_id", &self.account_id)
            .field("nsac_email", &self.nsac_email)
            .field("password", &"[REDACTED]")
            .field("cookies", &self.cookies.as_ref().map(|_| "[REDACTED]"))
            .field("class_code", &self.class_code)
            .field("has_fcm_token", &self.fcm_token.is_some())
            .field("active", &self.active)
            .finish()
    }
}

#[derive(FromRow)]
struct AccountSummaryRow {
    id: i64,
    nsac_email: String,
    class_code: Option<String>,
    fcm_token: Option<String>,
    discord_id: Option<String>,
    active: bool,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl From<AccountSummaryRow> for AccountSummary {
    fn from(row: AccountSummaryRow) -> Self {
        let has_fcm_token = row.fcm_token.as_deref().is_some_and(|t| !t.is_empty());
        Self {
            id: row.id,
            nsac_email: row.nsac_email,
            class_code: row.class_code,
            has_fcm_token,
            fcm_token: row.fcm_token,
            discord_id: row.discord_id,
            active: row.active,
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

#[derive(FromRow)]
struct AccountSecretsRow {
    id: i64,
    nsac_email: String,
    nsac_pass_enc: String,
    nsac_cookies_enc: Option<String>,
    class_code: Option<String>,
    fcm_token: Option<String>,
    active: bool,
}

#[derive(Debug, Clone)]
pub struct AccountRepo {
    pool: PgPool,
}

impl AccountRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn upsert_on_login(
        &self,
        crypto: &CryptoService,
        email: &str,
        plain_pass: &str,
        cookies: &NsacSessionCookies,
        class_code: Option<&str>,
        discord_id: Option<&str>,
        fcm_token: Option<&str>,
        now: DateTime<Utc>,
    ) -> Result<AccountSummary, ServerError> {
        let normalized_email = email.trim().to_ascii_lowercase();
        if normalized_email.is_empty() {
            return Err(ServerError::invalid_params(
                "NSAC email cannot be empty during account upsert",
            ));
        }

        let pass_enc = crypto.encrypt(plain_pass)?;
        let cookie_header = cookies.to_cookie_header();
        let cookies_enc = crypto.encrypt(cookie_header.as_str())?;
        let normalized_class = class_code.and_then(normalize_class_code);
        let trimmed_discord = discord_id.map(str::trim).filter(|s| !s.is_empty());
        let trimmed_fcm = fcm_token.map(str::trim).filter(|s| !s.is_empty());

        let row = sqlx::query_as::<_, AccountSummaryRow>(
            r#"
            INSERT INTO accounts (
                nsac_email,
                nsac_pass_enc,
                nsac_cookies_enc,
                class_code,
                fcm_token,
                discord_id,
                active,
                created_at,
                updated_at
            )
            VALUES ($1, $2, $3, $4, $5, $6, TRUE, $7, $7)
            ON CONFLICT (nsac_email) DO UPDATE SET
                nsac_pass_enc = EXCLUDED.nsac_pass_enc,
                nsac_cookies_enc = EXCLUDED.nsac_cookies_enc,
                class_code = COALESCE(EXCLUDED.class_code, accounts.class_code),
                fcm_token = COALESCE(EXCLUDED.fcm_token, accounts.fcm_token),
                discord_id = COALESCE(EXCLUDED.discord_id, accounts.discord_id),
                active = TRUE,
                updated_at = EXCLUDED.updated_at
            RETURNING id, nsac_email, class_code, fcm_token, discord_id, active, created_at, updated_at
            "#,
        )
        .bind(&normalized_email)
        .bind(&pass_enc)
        .bind(&cookies_enc)
        .bind(normalized_class)
        .bind(trimmed_fcm)
        .bind(trimmed_discord)
        .bind(now)
        .fetch_one(&self.pool)
        .await?;

        Ok(row.into())
    }

    pub async fn find_by_id(&self, id: i64) -> Result<Option<AccountSummary>, ServerError> {
        let row = sqlx::query_as::<_, AccountSummaryRow>(
            r#"
            SELECT id, nsac_email, class_code, fcm_token, discord_id, active, created_at, updated_at
            FROM accounts
            WHERE id = $1
            "#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(Into::into))
    }

    pub async fn find_by_email(&self, email: &str) -> Result<Option<AccountSummary>, ServerError> {
        let normalized_email = email.trim().to_ascii_lowercase();
        let row = sqlx::query_as::<_, AccountSummaryRow>(
            r#"
            SELECT id, nsac_email, class_code, fcm_token, discord_id, active, created_at, updated_at
            FROM accounts
            WHERE LOWER(nsac_email) = $1
            "#,
        )
        .bind(&normalized_email)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(Into::into))
    }

    pub async fn find_by_discord_id(
        &self,
        discord_id: &str,
    ) -> Result<Option<AccountSummary>, ServerError> {
        let trimmed = discord_id.trim();
        let row = sqlx::query_as::<_, AccountSummaryRow>(
            r#"
            SELECT id, nsac_email, class_code, fcm_token, discord_id, active, created_at, updated_at
            FROM accounts
            WHERE discord_id = $1
            "#,
        )
        .bind(trimmed)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(Into::into))
    }

    pub async fn get_decrypted_credentials(
        &self,
        crypto: &CryptoService,
        account_id: i64,
    ) -> Result<Option<DecryptedAccountSecrets>, ServerError> {
        let row = sqlx::query_as::<_, AccountSecretsRow>(
            r#"
            SELECT id, nsac_email, nsac_pass_enc, nsac_cookies_enc, class_code, fcm_token, active
            FROM accounts
            WHERE id = $1
            "#,
        )
        .bind(account_id)
        .fetch_optional(&self.pool)
        .await?;

        let Some(row) = row else {
            return Ok(None);
        };

        let password = crypto.decrypt(&row.nsac_pass_enc)?;
        let cookies = match row.nsac_cookies_enc.as_deref() {
            Some(enc) => {
                let header = crypto.decrypt(enc)?;
                Some(NsacSessionCookies::from_cookie_header(header.as_str())?)
            }
            None => None,
        };

        Ok(Some(DecryptedAccountSecrets {
            account_id: row.id,
            nsac_email: row.nsac_email,
            password,
            cookies,
            class_code: row.class_code,
            fcm_token: row.fcm_token,
            active: row.active,
        }))
    }

    pub async fn update_cookies(
        &self,
        crypto: &CryptoService,
        account_id: i64,
        cookies: &NsacSessionCookies,
        now: DateTime<Utc>,
    ) -> Result<(), ServerError> {
        let cookie_header = cookies.to_cookie_header();
        let cookies_enc = crypto.encrypt(cookie_header.as_str())?;

        let res = sqlx::query(
            r#"
            UPDATE accounts
            SET nsac_cookies_enc = $2, updated_at = $3
            WHERE id = $1
            "#,
        )
        .bind(account_id)
        .bind(&cookies_enc)
        .bind(now)
        .execute(&self.pool)
        .await?;

        if res.rows_affected() == 0 {
            return Err(ServerError::not_found(format!(
                "Account {account_id} not found for cookie update"
            )));
        }
        Ok(())
    }

    pub async fn update_class_code(
        &self,
        account_id: i64,
        class_code: &str,
        now: DateTime<Utc>,
    ) -> Result<(), ServerError> {
        let normalized = normalize_class_code(class_code).ok_or_else(|| {
            ServerError::invalid_params("Class code cannot be empty")
        })?;

        let res = sqlx::query(
            r#"
            UPDATE accounts
            SET class_code = $2, updated_at = $3
            WHERE id = $1
            "#,
        )
        .bind(account_id)
        .bind(&normalized)
        .bind(now)
        .execute(&self.pool)
        .await?;

        if res.rows_affected() == 0 {
            return Err(ServerError::not_found(format!(
                "Account {account_id} not found for class_code update"
            )));
        }
        Ok(())
    }

    pub async fn update_fcm_token(
        &self,
        account_id: i64,
        fcm_token: Option<&str>,
        now: DateTime<Utc>,
    ) -> Result<bool, ServerError> {
        let trimmed = fcm_token.map(str::trim).filter(|s| !s.is_empty());

        let res = sqlx::query(
            r#"
            UPDATE accounts
            SET fcm_token = $2, updated_at = $3
            WHERE id = $1
            "#,
        )
        .bind(account_id)
        .bind(trimmed)
        .bind(now)
        .execute(&self.pool)
        .await?;

        Ok(res.rows_affected() > 0)
    }

    pub async fn link_discord(
        &self,
        account_id: i64,
        discord_id: &str,
        now: DateTime<Utc>,
    ) -> Result<AccountSummary, ServerError> {
        let trimmed = discord_id.trim();
        if trimmed.is_empty() {
            return Err(ServerError::invalid_params("discord_id cannot be empty"));
        }

        let row = sqlx::query_as::<_, AccountSummaryRow>(
            r#"
            UPDATE accounts
            SET discord_id = $2, updated_at = $3
            WHERE id = $1
            RETURNING id, nsac_email, class_code, fcm_token, discord_id, active, created_at, updated_at
            "#,
        )
        .bind(account_id)
        .bind(trimmed)
        .bind(now)
        .fetch_optional(&self.pool)
        .await?;

        match row {
            Some(r) => Ok(r.into()),
            None => Err(ServerError::not_found(format!(
                "Account {account_id} not found for Discord link"
            ))),
        }
    }

    pub async fn list_active_by_class(
        &self,
        class_code: &str,
    ) -> Result<Vec<AccountSummary>, ServerError> {
        let Some(normalized) = normalize_class_code(class_code) else {
            return Ok(Vec::new());
        };

        let rows = sqlx::query_as::<_, AccountSummaryRow>(
            r#"
            SELECT id, nsac_email, class_code, fcm_token, discord_id, active, created_at, updated_at
            FROM accounts
            WHERE active = TRUE AND class_code = $1
            ORDER BY id ASC
            "#,
        )
        .bind(&normalized)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows.into_iter().map(Into::into).collect())
    }

    pub async fn list_active_classes(&self) -> Result<Vec<String>, ServerError> {
        let classes = sqlx::query_scalar::<_, String>(
            r#"
            SELECT DISTINCT class_code
            FROM accounts
            WHERE active = TRUE AND class_code IS NOT NULL
            ORDER BY class_code ASC
            "#,
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(classes)
    }

    pub async fn set_active(
        &self,
        account_id: i64,
        active: bool,
        now: DateTime<Utc>,
    ) -> Result<bool, ServerError> {
        let res = sqlx::query(
            r#"
            UPDATE accounts
            SET active = $2, updated_at = $3
            WHERE id = $1
            "#,
        )
        .bind(account_id)
        .bind(active)
        .bind(now)
        .execute(&self.pool)
        .await?;

        Ok(res.rows_affected() > 0)
    }
}
