use axum::extract::{Path, RawQuery, State};
use axum::response::{Redirect, Response};
use axum::Json;
use chrono::{Duration, Utc};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::auth::{DualAuth, Principal};
use crate::connections::oauth::{
    build_authorize_url, complete_oauth_callback, complete_steam_openid_callback,
    generate_random_token, is_oauth_configured, pkce_challenge_s256, public_base_url,
};
use crate::connections::providers::{
    MAX_CONNECTIONS_PER_ACCOUNT, ProviderDescriptor, all_provider_descriptors,
    normalize_manual_connection,
};
use crate::connections::{AccountConnectionDto, ConnectionProvider, VerificationSource};
use crate::db::auth::sha256_hex;
use crate::db::connections::UpsertConnectionParams;
use crate::error::ServerError;
use crate::http::envelope::ok_json;
use crate::http::extract::parse_query_pairs;
use crate::state::AppState;

const OAUTH_STATE_TTL_MINUTES: i64 = 10;

fn user_account_id(principal: &Principal) -> Result<i64, ServerError> {
    match principal {
        Principal::User(u) => Ok(u.account_id),
        Principal::Service(_) => Err(ServerError::forbidden(
            "Apenas contas de alunos podem gerenciar conexões de perfil.",
        )),
    }
}

#[derive(Debug, Serialize)]
pub struct AccountConnectionsPayload {
    pub connections: Vec<AccountConnectionDto>,
    pub providers: Vec<ProviderDescriptor>,
    pub max_connections: usize,
}

#[derive(Debug, Deserialize)]
pub struct UpsertManualConnectionRequest {
    /// Handle (e.g. `@usuario`) or official profile URL.
    pub handle: Option<String>,
    pub display_name: Option<String>,
    pub is_visible: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct StartOAuthRequest {
    pub platform: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct StartOAuthResponse {
    pub authorize_url: String,
    pub provider: String,
}

/// GET /api/v1/account/connections
pub async fn list_my_connections(
    State(state): State<AppState>,
    DualAuth(principal): DualAuth,
) -> Result<Response, ServerError> {
    let account_id = user_account_id(&principal)?;
    let connections = state.connections().list_for_account(account_id).await?;
    let providers = all_provider_descriptors(is_oauth_configured);
    Ok(ok_json(AccountConnectionsPayload {
        connections,
        providers,
        max_connections: MAX_CONNECTIONS_PER_ACCOUNT,
    }))
}

/// PUT /api/v1/account/connections/{provider}
pub async fn put_my_connection(
    State(state): State<AppState>,
    DualAuth(principal): DualAuth,
    Path(raw_provider): Path<String>,
    Json(body): Json<UpsertManualConnectionRequest>,
) -> Result<Response, ServerError> {
    let account_id = user_account_id(&principal)?;
    let provider = ConnectionProvider::parse(&raw_provider)
        .ok_or_else(|| ServerError::invalid_params("Plataforma de conexão desconhecida."))?;
    let repo = state.connections();

    // Visibility-only toggle for an existing connection (preserves OAuth verified badge).
    if body.handle.is_none() {
        let is_visible = body.is_visible.ok_or_else(|| {
            ServerError::invalid_params("Informe o usuário/link ou a visibilidade da conexão.")
        })?;
        let updated = repo
            .set_visibility(account_id, provider.as_str(), is_visible)
            .await?
            .ok_or_else(|| ServerError::not_found("Conexão não encontrada."))?;
        return Ok(ok_json(updated));
    }

    let exists = repo.exists_for_provider(account_id, provider.as_str()).await?;
    if !exists {
        let count = repo.count_for_account(account_id).await?;
        if count as usize >= MAX_CONNECTIONS_PER_ACCOUNT {
            return Err(ServerError::invalid_params(format!(
                "Limite máximo de {MAX_CONNECTIONS_PER_ACCOUNT} conexões atingido."
            )));
        }
    }

    let raw_handle = body.handle.as_deref().unwrap_or_default();
    let normalized = normalize_manual_connection(provider, raw_handle, body.display_name.as_deref())?;
    let saved = repo
        .upsert(UpsertConnectionParams {
            account_id,
            provider: provider.as_str(),
            external_id: None,
            handle: &normalized.handle,
            display_name: normalized.display_name.as_deref(),
            profile_url: normalized.profile_url.as_deref(),
            verification_source: VerificationSource::Manual,
            is_verified: false,
            is_visible: body.is_visible.unwrap_or(true),
        })
        .await?;

    Ok(ok_json(saved))
}

/// DELETE /api/v1/account/connections/{provider}
pub async fn delete_my_connection(
    State(state): State<AppState>,
    DualAuth(principal): DualAuth,
    Path(raw_provider): Path<String>,
) -> Result<Response, ServerError> {
    let account_id = user_account_id(&principal)?;
    let provider = ConnectionProvider::parse(&raw_provider)
        .ok_or_else(|| ServerError::invalid_params("Plataforma de conexão desconhecida."))?;
    let deleted = state
        .connections()
        .delete(account_id, provider.as_str())
        .await?;
    if !deleted {
        return Err(ServerError::not_found("Conexão não encontrada."));
    }
    Ok(ok_json(serde_json::json!({ "deleted": true, "provider": provider.as_str() })))
}

/// POST /api/v1/account/connections/oauth/{provider}/start
pub async fn start_oauth_link(
    State(state): State<AppState>,
    DualAuth(principal): DualAuth,
    Path(raw_provider): Path<String>,
    Json(body): Json<Option<StartOAuthRequest>>,
) -> Result<Response, ServerError> {
    let account_id = user_account_id(&principal)?;
    let provider = ConnectionProvider::parse(&raw_provider)
        .ok_or_else(|| ServerError::invalid_params("Plataforma de conexão desconhecida."))?;
    if !is_oauth_configured(provider) {
        return Err(ServerError::invalid_params(format!(
            "Verificação OAuth para {} ainda não está habilitada no servidor.",
            provider.label()
        )));
    }

    let platform = match body
        .and_then(|b| b.platform)
        .as_deref()
        .map(str::trim)
        .unwrap_or("web")
    {
        "android" => "android",
        _ => "web",
    };

    let raw_state = generate_random_token(24);
    let state_hash = sha256_hex(&raw_state);
    let uses_pkce = matches!(provider, ConnectionProvider::Discord | ConnectionProvider::Twitch);
    let (pkce_ciphertext, code_challenge) = if uses_pkce {
        let verifier = Zeroizing::new(generate_random_token(32));
        let challenge = pkce_challenge_s256(&verifier);
        let encrypted = state.crypto.encrypt(&verifier)?;
        (Some(encrypted), Some(challenge))
    } else {
        (None, None)
    };

    let expires_at = Utc::now() + Duration::minutes(OAUTH_STATE_TTL_MINUTES);
    state
        .connections()
        .create_oauth_state(
            &state_hash,
            account_id,
            provider.as_str(),
            pkce_ciphertext.as_deref(),
            platform,
            expires_at,
        )
        .await?;

    let authorize_url = build_authorize_url(provider, &raw_state, code_challenge.as_deref())?;
    Ok(ok_json(StartOAuthResponse {
        authorize_url,
        provider: provider.as_str().to_string(),
    }))
}

fn done_redirect(provider: &str, status: &str, imported_count: usize) -> Redirect {
    let base = public_base_url().unwrap_or_default();
    let target = format!(
        "{base}/connections/done?provider={provider}&status={status}&imported={imported_count}"
    );
    Redirect::to(&target)
}

/// GET /api/v1/connections/oauth/{provider}/callback
pub async fn oauth_callback(
    State(state): State<AppState>,
    Path(raw_provider): Path<String>,
    RawQuery(raw_query): RawQuery,
) -> Redirect {
    let Some(provider) = ConnectionProvider::parse(&raw_provider) else {
        return done_redirect("unknown", "invalid_provider", 0);
    };
    let pairs = parse_query_pairs(raw_query.as_deref());
    let find_param = |name: &str| {
        pairs
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    };

    if find_param("error").is_some() {
        return done_redirect(provider.as_str(), "cancelled", 0);
    }

    let Some(raw_state) = find_param("state").filter(|s| !s.is_empty()) else {
        return done_redirect(provider.as_str(), "missing_state", 0);
    };
    let state_hash = sha256_hex(raw_state);
    let consumed = match state
        .connections()
        .consume_oauth_state(&state_hash, provider.as_str())
        .await
    {
        Ok(Some(s)) => s,
        _ => return done_redirect(provider.as_str(), "expired_state", 0),
    };

    let repo = state.connections();

    if provider == ConnectionProvider::Steam {
        let verified = match complete_steam_openid_callback(&pairs).await {
            Ok(v) => v,
            Err(_) => return done_redirect("steam", "error", 0),
        };
        let _ = repo
            .upsert(UpsertConnectionParams {
                account_id: consumed.account_id,
                provider: verified.provider.as_str(),
                external_id: Some(&verified.external_id),
                handle: &verified.handle,
                display_name: verified.display_name.as_deref(),
                profile_url: verified.profile_url.as_deref(),
                verification_source: verified.verification_source,
                is_verified: true,
                is_visible: true,
            })
            .await;
        return done_redirect("steam", "ok", 0);
    }

    let Some(code) = find_param("code").filter(|s| !s.is_empty()) else {
        return done_redirect(provider.as_str(), "missing_code", 0);
    };

    let pkce_verifier = match consumed.pkce_verifier_ciphertext.as_deref() {
        Some(ct) => match state.crypto.decrypt(ct) {
            Ok(v) => Some(v),
            Err(_) => return done_redirect(provider.as_str(), "error", 0),
        },
        None => None,
    };

    let result = match complete_oauth_callback(
        provider,
        code,
        pkce_verifier.as_deref().map(String::as_str),
    )
    .await
    {
        Ok(r) => r,
        Err(_) => return done_redirect(provider.as_str(), "error", 0),
    };

    if repo
        .upsert(UpsertConnectionParams {
            account_id: consumed.account_id,
            provider: result.primary.provider.as_str(),
            external_id: Some(&result.primary.external_id),
            handle: &result.primary.handle,
            display_name: result.primary.display_name.as_deref(),
            profile_url: result.primary.profile_url.as_deref(),
            verification_source: result.primary.verification_source,
            is_verified: true,
            is_visible: true,
        })
        .await
        .is_err()
    {
        return done_redirect(provider.as_str(), "error", 0);
    }

    let mut imported_count = 0usize;
    for imp in result.imported {
        if repo
            .upsert(UpsertConnectionParams {
                account_id: consumed.account_id,
                provider: imp.provider.as_str(),
                external_id: Some(&imp.external_id),
                handle: &imp.handle,
                display_name: imp.display_name.as_deref(),
                profile_url: imp.profile_url.as_deref(),
                verification_source: imp.verification_source,
                is_verified: true,
                is_visible: true,
            })
            .await
            .is_ok()
        {
            imported_count += 1;
        }
    }

    done_redirect(provider.as_str(), "ok", imported_count)
}
