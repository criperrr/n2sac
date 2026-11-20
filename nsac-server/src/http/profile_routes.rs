use axum::extract::State;
use axum::Json;
use serde::{Deserialize, Serialize};
use sqlx::FromRow;

use crate::auth::{DualAuth, Principal};
use crate::error::ServerError;
use crate::http::envelope::ApiSuccess;
use crate::state::AppState;

const MAX_AVATAR_LEN: usize = 450_000;
const MAX_BANNER_LEN: usize = 650_000;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserProfilePayload {
    pub account_id: i64,
    pub first_name: String,
    pub display_name: Option<String>,
    pub bio: Option<String>,
    pub avatar_data_url: Option<String>,
    pub banner_data_url: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateProfileRequest {
    pub display_name: Option<String>,
    pub bio: Option<String>,
    pub avatar_data_url: Option<String>,
    pub banner_data_url: Option<String>,
}

#[derive(FromRow)]
struct ProfileRow {
    display_name: Option<String>,
    bio: Option<String>,
    avatar_data_url: Option<String>,
    banner_data_url: Option<String>,
}

pub fn extract_first_name_from_email(email: &str) -> String {
    let local = email.split('@').next().unwrap_or("").trim();
    let first = local
        .split(['.', '_', '-'])
        .next()
        .unwrap_or("")
        .trim();
    if first.is_empty() {
        return "Aluno".to_string();
    }
    let mut chars = first.chars();
    match chars.next() {
        Some(c) => format!("{}{}", c.to_uppercase(), chars.as_str().to_lowercase()),
        None => "Aluno".to_string(),
    }
}

async fn ensure_profile_table(pool: &sqlx::PgPool) -> Result<(), ServerError> {
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS account_profiles (
            account_id BIGINT PRIMARY KEY REFERENCES accounts(id) ON DELETE CASCADE,
            display_name TEXT NULL,
            bio TEXT NULL,
            avatar_data_url TEXT NULL,
            banner_data_url TEXT NULL,
            updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
        )
        "#,
    )
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn get_profile(
    State(state): State<AppState>,
    DualAuth(principal): DualAuth,
) -> Result<Json<ApiSuccess<UserProfilePayload>>, ServerError> {
    let account_id = match principal {
        Principal::User(u) => u.account_id,
        Principal::Service(_) => return Err(ServerError::unauthorized("User session required")),
    };

    ensure_profile_table(&state.db).await?;

    let email: String = sqlx::query_scalar("SELECT nsac_email FROM accounts WHERE id = $1")
        .bind(account_id)
        .fetch_one(&state.db)
        .await?;

    let row = sqlx::query_as::<_, ProfileRow>(
        r#"
        SELECT display_name, bio, avatar_data_url, banner_data_url
        FROM account_profiles
        WHERE account_id = $1
        "#,
    )
    .bind(account_id)
    .fetch_optional(&state.db)
    .await?;

    let first_name = extract_first_name_from_email(&email);
    let payload = match row {
        Some(r) => UserProfilePayload {
            account_id,
            first_name,
            display_name: r.display_name,
            bio: r.bio,
            avatar_data_url: r.avatar_data_url,
            banner_data_url: r.banner_data_url,
        },
        None => UserProfilePayload {
            account_id,
            first_name,
            display_name: None,
            bio: None,
            avatar_data_url: None,
            banner_data_url: None,
        },
    };

    Ok(Json(ApiSuccess {
        success: true,
        data: payload,
        next_poll_hint_secs: None,
    }))
}

pub async fn put_profile(
    State(state): State<AppState>,
    DualAuth(principal): DualAuth,
    Json(body): Json<UpdateProfileRequest>,
) -> Result<Json<ApiSuccess<UserProfilePayload>>, ServerError> {
    let account_id = match principal {
        Principal::User(u) => u.account_id,
        Principal::Service(_) => return Err(ServerError::unauthorized("User session required")),
    };

    if let Some(ref avatar) = body.avatar_data_url
        && avatar.len() > MAX_AVATAR_LEN
    {
        return Err(ServerError::invalid_params(
            "Foto de perfil muito grande. Use uma imagem menor.",
        ));
    }
    if let Some(ref banner) = body.banner_data_url
        && banner.len() > MAX_BANNER_LEN
    {
        return Err(ServerError::invalid_params(
            "Banner muito grande. Use uma imagem menor.",
        ));
    }

    ensure_profile_table(&state.db).await?;

    let cleaned_name = body
        .display_name
        .map(|s| s.trim().chars().take(48).collect::<String>())
        .filter(|s| !s.is_empty());
    let cleaned_bio = body
        .bio
        .map(|s| s.trim().chars().take(240).collect::<String>())
        .filter(|s| !s.is_empty());

    let row = sqlx::query_as::<_, ProfileRow>(
        r#"
        INSERT INTO account_profiles (
            account_id, display_name, bio, avatar_data_url, banner_data_url, updated_at
        )
        VALUES ($1, $2, $3, $4, $5, NOW())
        ON CONFLICT (account_id) DO UPDATE SET
            display_name = EXCLUDED.display_name,
            bio = EXCLUDED.bio,
            avatar_data_url = EXCLUDED.avatar_data_url,
            banner_data_url = EXCLUDED.banner_data_url,
            updated_at = NOW()
        RETURNING display_name, bio, avatar_data_url, banner_data_url
        "#,
    )
    .bind(account_id)
    .bind(cleaned_name)
    .bind(cleaned_bio)
    .bind(body.avatar_data_url)
    .bind(body.banner_data_url)
    .fetch_one(&state.db)
    .await?;

    let email: String = sqlx::query_scalar("SELECT nsac_email FROM accounts WHERE id = $1")
        .bind(account_id)
        .fetch_one(&state.db)
        .await?;

    Ok(Json(ApiSuccess {
        success: true,
        data: UserProfilePayload {
            account_id,
            first_name: extract_first_name_from_email(&email),
            display_name: row.display_name,
            bio: row.bio,
            avatar_data_url: row.avatar_data_url,
            banner_data_url: row.banner_data_url,
        },
        next_poll_hint_secs: None,
    }))
}
