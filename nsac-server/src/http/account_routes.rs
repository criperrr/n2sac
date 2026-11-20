use axum::extract::{RawQuery, State};
use axum::response::Response;
use nsac_core::error::ErrorCode;
use serde::{Deserialize, Serialize};

use crate::auth::{
    DualAuth, SCOPE_ACCOUNT_LINK, SCOPE_NSAC_READ, SCOPE_NSAC_WRITE, resolve_target_account,
};
use crate::db::accounts::AccountSummary;
use crate::error::ServerError;
use crate::http::envelope::ok_json;
use crate::http::extract::{AppJson, parse_query_pairs, parse_target_query};
use crate::state::AppState;

#[derive(Serialize)]
pub struct AccountProfileResponse {
    pub account_id: i64,
    pub nsac_email: String,
    pub class_code: Option<String>,
    pub has_fcm_token: bool,
    pub discord_id: Option<String>,
    pub active: bool,
    pub created_at: String,
    pub updated_at: String,
}

impl From<AccountSummary> for AccountProfileResponse {
    fn from(summary: AccountSummary) -> Self {
        Self {
            account_id: summary.id,
            nsac_email: summary.nsac_email,
            class_code: summary.class_code,
            has_fcm_token: summary.has_fcm_token,
            discord_id: summary.discord_id,
            active: summary.active,
            created_at: summary.created_at.to_rfc3339(),
            updated_at: summary.updated_at.to_rfc3339(),
        }
    }
}

#[derive(Deserialize)]
pub struct UpdateFcmTokenRequest {
    pub fcm_token: String,
}

#[derive(Deserialize)]
pub struct LinkDiscordRequest {
    #[serde(default)]
    pub account_id: Option<i64>,
    pub discord_id: String,
}

pub async fn get_account(
    State(state): State<AppState>,
    DualAuth(principal): DualAuth,
    RawQuery(raw_query): RawQuery,
) -> Result<Response, ServerError> {
    let pairs = parse_query_pairs(raw_query.as_deref());
    let target_query = parse_target_query(&pairs)?;
    let target = resolve_target_account(
        &state,
        &principal,
        SCOPE_NSAC_READ,
        target_query.account_id,
        target_query.discord_id.as_deref(),
    )
    .await?;

    Ok(ok_json(AccountProfileResponse::from(target.summary)))
}

pub async fn put_fcm_token(
    State(state): State<AppState>,
    DualAuth(principal): DualAuth,
    RawQuery(raw_query): RawQuery,
    AppJson(body): AppJson<UpdateFcmTokenRequest>,
) -> Result<Response, ServerError> {
    let trimmed_token = body.fcm_token.trim();
    if trimmed_token.is_empty() {
        return Err(ServerError::with_user_message(
            ErrorCode::InvalidParams,
            "O campo 'fcm_token' não pode ser vazio.",
            "Empty fcm_token in PUT /account/fcm-token",
        ));
    }

    let pairs = parse_query_pairs(raw_query.as_deref());
    let target_query = parse_target_query(&pairs)?;
    let target = resolve_target_account(
        &state,
        &principal,
        SCOPE_NSAC_WRITE,
        target_query.account_id,
        target_query.discord_id.as_deref(),
    )
    .await?;

    let now = state.clock.now();
    let updated = state
        .accounts()
        .update_fcm_token(target.account_id, Some(trimmed_token), now)
        .await?;
    if !updated {
        return Err(ServerError::user_not_linked("Conta alvo não encontrada."));
    }

    let refreshed = state
        .accounts()
        .find_by_id(target.account_id)
        .await?
        .ok_or_else(|| ServerError::user_not_linked("Conta alvo não encontrada."))?;

    Ok(ok_json(AccountProfileResponse::from(refreshed)))
}

pub async fn put_discord_link(
    State(state): State<AppState>,
    DualAuth(principal): DualAuth,
    AppJson(body): AppJson<LinkDiscordRequest>,
) -> Result<Response, ServerError> {
    let trimmed_discord = body.discord_id.trim();
    if trimmed_discord.is_empty() {
        return Err(ServerError::with_user_message(
            ErrorCode::InvalidParams,
            "O campo 'discord_id' não pode ser vazio.",
            "Empty discord_id in PUT /account/discord-link",
        ));
    }

    let target = resolve_target_account(
        &state,
        &principal,
        SCOPE_ACCOUNT_LINK,
        body.account_id,
        None,
    )
    .await?;

    let now = state.clock.now();
    let updated = state
        .accounts()
        .link_discord(target.account_id, trimmed_discord, now)
        .await?;

    Ok(ok_json(AccountProfileResponse::from(updated)))
}
