use std::sync::Arc;

use axum::extract::State;
use axum::response::Response;
use nsac_core::error::ErrorCode;
use nsac_core::sentinel::compute_class_grade_hash;
use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::auth::{DualAuth, Principal};
use crate::db::accounts::{CTI_CLASSES, normalize_class_code};
use crate::db::auth::sha256_hex;
use crate::db::snapshots::compute_strong_etag;
use crate::error::ServerError;
use crate::http::envelope::ok_json;
use crate::http::extract::AppJson;
use crate::http::nsac_routes::canonicalize_boletim;
use crate::state::{AppState, CachedBoletim};

#[derive(Deserialize, Zeroize, ZeroizeOnDrop)]
pub struct LoginRequest {
    pub nsac_email: String,
    pub nsac_password: String,
    #[serde(default)]
    pub discord_id: Option<String>,
    #[serde(default)]
    pub fcm_token: Option<String>,
}

#[derive(Serialize)]
pub struct LoginResponse {
    pub account_id: i64,
    pub nsac_email: String,
    pub class_code: Option<String>,
    pub token: String,
    pub expires_at: String,
}

pub async fn login(
    State(state): State<AppState>,
    AppJson(req): AppJson<LoginRequest>,
) -> Result<Response, ServerError> {
    let email = req.nsac_email.trim();
    if email.is_empty() || req.nsac_password.trim().is_empty() {
        return Err(ServerError::with_user_message(
            ErrorCode::InvalidParams,
            "E-mail e senha do NSAC são obrigatórios.",
            "Empty nsac_email or nsac_password on login",
        ));
    }

    let cookies = {
        let _permit = state.semaphore.acquire().await?;
        state.nsac.login(email, &req.nsac_password).await?
    };

    let (mut boletim, rotated) = {
        let _permit = state.semaphore.acquire().await?;
        state.nsac.fetch_boletim(&cookies).await?
    };
    let effective_cookies = rotated.unwrap_or(cookies);

    let class_code = boletim.current_class_code().and_then(normalize_class_code);
    if let Some(ref code) = class_code
        && !CTI_CLASSES.contains(&code.as_str())
    {
        tracing::warn!(
            class_code = %code,
            "Derived NSAC class_code is outside known CTI_CLASSES"
        );
    }

    canonicalize_boletim(&mut boletim);

    let now = state.clock.now();
    let account = state
        .accounts()
        .upsert_on_login(
            &state.crypto,
            email,
            &req.nsac_password,
            &effective_cookies,
            class_code.as_deref(),
            req.discord_id.as_deref(),
            req.fcm_token.as_deref(),
            now,
        )
        .await?;

    state.boletim().upsert_boletim(account.id, &boletim).await?;
    let canonical_boletim = state
        .boletim()
        .get_boletim(account.id)
        .await?
        .unwrap_or(boletim);

    let etag = compute_strong_etag(&canonical_boletim)?;
    state
        .caches
        .boletim
        .insert(
            account.id,
            CachedBoletim {
                data: Arc::new(canonical_boletim.clone()),
                etag,
                cached_at: now,
            },
        )
        .await;

    if let Some(ref code) = account.class_code {
        let class_hash = compute_class_grade_hash(&canonical_boletim);
        state
            .heuristic
            .initialize_class_on_login(code, &class_hash, account.id, now)
            .await?;
        // Ensure mandatory class room exists for the student's current (class_code, school_year).
        let _ = state.classrooms().resolve_user_classroom(account.id).await;
    }

    let session = state
        .auth()
        .create_user_session(account.id, state.config.user_session_ttl_secs, now)
        .await?;

    Ok(ok_json(LoginResponse {
        account_id: account.id,
        nsac_email: account.nsac_email,
        class_code: account.class_code,
        token: session.token.as_str().to_string(),
        expires_at: session.expires_at.to_rfc3339(),
    }))
}

pub async fn logout(
    State(state): State<AppState>,
    DualAuth(principal): DualAuth,
) -> Result<Response, ServerError> {
    let user = match principal {
        Principal::User(u) => u,
        Principal::Service(_) => {
            return Err(ServerError::with_user_message(
                ErrorCode::Forbidden,
                "Apenas usuários autenticados por token podem encerrar sessão.",
                "Service client attempted /auth/logout",
            ));
        }
    };

    let now = state.clock.now();
    state
        .auth()
        .revoke_session_by_raw_token(user.raw_token.as_str(), now)
        .await?;
    state
        .caches
        .invalidate_user_session(&sha256_hex(user.raw_token.as_str()))
        .await;

    Ok(ok_json(serde_json::json!({ "revoked": true })))
}
