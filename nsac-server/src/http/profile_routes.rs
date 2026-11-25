use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum::extract::{Path, RawQuery, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use axum::Json;
use chrono::{Duration as ChronoDuration, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::FromRow;
use zeroize::Zeroizing;

use crate::auth::{DualAuth, Principal};
use crate::connections::oauth::{generate_random_token, pkce_challenge_s256, public_base_url};
use crate::error::ServerError;
use crate::db::profiles::{self, ProfileImage};
use crate::db::ranking::{
    avatar_path, derive_official_name_from_email, email_name_needs_verification,
    format_student_label,
};
use crate::db::moderation::{ModerationRepo, ReportKind, ReportOutcome, Restriction};
use crate::db::social::{FollowEdgeStatus, RelationshipStatus};
use crate::media::{ProfileMediaKind, sanitize_profile_image};
use crate::http::envelope::{ApiSuccess, etag_matches_if_none_match, ok_json};
use crate::state::AppState;


#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserProfilePayload {
    pub account_id: i64,
    pub first_name: String,
    pub display_name: Option<String>,
    pub bio: Option<String>,
    pub avatar_data_url: Option<String>,
    pub banner_data_url: Option<String>,
    /// Active community-moderation restrictions on the viewer's own content.
    #[serde(default)]
    pub restrictions: Vec<Restriction>,
    /// Official full name cryptographically confirmed via Google (`@unesp.br` OIDC).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verified_official_name: Option<String>,
    /// True when the institutional email looks abbreviated (e.g. `g.ullrich`, `ds.batista`)
    /// or has numeric collision suffixes and has not yet been confirmed via Google.
    #[serde(default)]
    pub official_name_needs_verification: bool,
    /// Public Google OAuth Client ID (when configured on server) so web/app can invoke GSI.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub google_oauth_client_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct VerifyGoogleNameRequest {
    pub id_token: String,
}

#[derive(Debug, Deserialize)]
struct GoogleTokenInfoResponse {
    email: Option<String>,
    email_verified: Option<String>,
    name: Option<String>,
    given_name: Option<String>,
    family_name: Option<String>,
    aud: Option<String>,
}

fn configured_google_client_id() -> Option<String> {
    std::env::var("GOOGLE_OAUTH_CLIENT_ID")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

pub fn extract_first_name_from_email(email: &str) -> String {
    let derived = derive_official_name_from_email(email);
    if derived.is_empty() {
        "Aluno".to_string()
    } else {
        derived
    }
}

fn resolve_default_official_name(email: &str, verified_official_name: Option<&str>) -> String {
    if let Some(verified) = verified_official_name.map(str::trim).filter(|s| !s.is_empty()) {
        return verified.to_string();
    }
    extract_first_name_from_email(email)
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

async fn build_user_profile_payload(
    state: &AppState,
    account_id: i64,
    row: Option<ProfileRow>,
) -> Result<UserProfilePayload, ServerError> {
    let (email, verified_official_name): (String, Option<String>) = sqlx::query_as(
        "SELECT nsac_email, verified_official_name FROM accounts WHERE id = $1",
    )
    .bind(account_id)
    .fetch_one(&state.db)
    .await?;

    let first_name = resolve_default_official_name(&email, verified_official_name.as_deref());
    let official_name_needs_verification =
        verified_official_name.is_none() && email_name_needs_verification(&email);
    let restrictions = state.moderation().restrictions(account_id).await?;
    let google_oauth_client_id = configured_google_client_id();

    let (display_name, bio, avatar_data_url, banner_data_url) = match row {
        Some(r) => (r.display_name, r.bio, r.avatar_data_url, r.banner_data_url),
        None => (None, None, None, None),
    };

    Ok(UserProfilePayload {
        account_id,
        first_name,
        display_name,
        bio,
        avatar_data_url,
        banner_data_url,
        restrictions,
        verified_official_name,
        official_name_needs_verification,
        google_oauth_client_id,
    })
}

pub async fn get_profile(
    State(state): State<AppState>,
    DualAuth(principal): DualAuth,
) -> Result<Json<ApiSuccess<UserProfilePayload>>, ServerError> {
    let account_id = match principal {
        Principal::User(u) => u.account_id,
        Principal::Service(_) => return Err(ServerError::unauthorized("User session required")),
    };

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

    let payload = build_user_profile_payload(&state, account_id, row).await?;

    Ok(Json(ApiSuccess {
        success: true,
        data: payload,
        next_poll_hint_secs: None,
    }))
}

pub async fn verify_google_name(
    State(state): State<AppState>,
    DualAuth(principal): DualAuth,
    Json(body): Json<VerifyGoogleNameRequest>,
) -> Result<Json<ApiSuccess<UserProfilePayload>>, ServerError> {
    let account_id = match principal {
        Principal::User(u) => u.account_id,
        Principal::Service(_) => return Err(ServerError::unauthorized("User session required")),
    };

    let id_token = body.id_token.trim();
    if id_token.is_empty() || id_token.len() > 8192 {
        return Err(ServerError::invalid_params("Token do Google inválido."));
    }

    let _clean_name = verify_and_persist_google_id_token(&state, account_id, id_token).await?;

    let row = sqlx::query_as::<_, ProfileRow>(
        "SELECT display_name, bio, avatar_data_url, banner_data_url FROM account_profiles WHERE account_id = $1",
    )
    .bind(account_id)
    .fetch_optional(&state.db)
    .await?;

    let payload = build_user_profile_payload(&state, account_id, row).await?;

    Ok(Json(ApiSuccess {
        success: true,
        data: payload,
        next_poll_hint_secs: None,
    }))
}

async fn verify_and_persist_google_id_token(
    state: &AppState,
    account_id: i64,
    id_token: &str,
) -> Result<String, ServerError> {
    let account_email: String = sqlx::query_scalar("SELECT nsac_email FROM accounts WHERE id = $1")
        .bind(account_id)
        .fetch_one(&state.db)
        .await?;

    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|_| ServerError::internal("Failed to build Google OIDC HTTP client"))?;

    let tokeninfo_url = reqwest::Url::parse_with_params(
        "https://oauth2.googleapis.com/tokeninfo",
        &[("id_token", id_token)],
    )
    .map_err(|_| ServerError::invalid_params("Token do Google inválido."))?;

    let resp = http
        .get(tokeninfo_url)
        .send()
        .await
        .map_err(|_| {
            ServerError::internal("Não foi possível validar o token com os servidores do Google.")
        })?;

    if !resp.status().is_success() {
        return Err(ServerError::invalid_params(
            "Token do Google expirado ou inválido. Tente confirmar novamente.",
        ));
    }

    let info: GoogleTokenInfoResponse = resp.json().await.map_err(|_| {
        ServerError::internal("Resposta inválida da verificação de identidade do Google.")
    })?;

    if info.email_verified.as_deref() != Some("true") {
        return Err(ServerError::invalid_params(
            "O e-mail desta conta Google não está verificado.",
        ));
    }

    if let Some(expected_aud) = configured_google_client_id()
        && info.aud.as_deref() != Some(expected_aud.as_str())
    {
        return Err(ServerError::invalid_params(
            "Token do Google emitido para outro aplicativo.",
        ));
    }

    let token_email = info.email.as_deref().map(str::trim).unwrap_or("");
    if !token_email.eq_ignore_ascii_case(account_email.trim()) {
        return Err(ServerError::invalid_params(format!(
            "Selecione sua própria conta Google institucional ({}) para confirmar o nome.",
            account_email
        )));
    }

    let raw_name = info
        .name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| {
            let g = info.given_name.as_deref().unwrap_or("").trim();
            let f = info.family_name.as_deref().unwrap_or("").trim();
            format!("{g} {f}").trim().to_string()
        });

    let clean_name: String = raw_name
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(80)
        .collect();

    if clean_name.chars().count() < 3 {
        return Err(ServerError::invalid_params(
            "A conta Google não retornou um nome completo válido.",
        ));
    }

    sqlx::query(
        "UPDATE accounts SET verified_official_name = $2, official_name_verified_at = NOW(), updated_at = NOW() WHERE id = $1",
    )
    .bind(account_id)
    .bind(&clean_name)
    .execute(&state.db)
    .await?;

    Ok(clean_name)
}

#[derive(Debug, Serialize)]
pub struct GoogleNameOAuthStartResponse {
    pub authorize_url: String,
}

fn sha256_hex_str(input: &str) -> String {
    let digest = Sha256::digest(input.as_bytes());
    let mut out = String::with_capacity(64);
    for b in digest {
        use std::fmt::Write;
        let _ = write!(&mut out, "{b:02x}");
    }
    out
}

fn google_name_callback_url() -> Result<String, ServerError> {
    let base = public_base_url().ok_or_else(|| {
        ServerError::invalid_params("OAUTH_PUBLIC_BASE_URL não configurado no servidor.")
    })?;
    Ok(format!("{base}/api/v1/account/verify-google-name/oauth/callback"))
}

/// POST /api/v1/account/verify-google-name/oauth/start
/// Starts a browser-based Google OIDC authorization code flow (PKCE) so Android APK users
/// authenticate in Chrome instead of WebView (avoiding Google's `403 disallowed_useragent`).
pub async fn start_google_name_oauth(
    State(state): State<AppState>,
    DualAuth(principal): DualAuth,
) -> Result<Json<ApiSuccess<GoogleNameOAuthStartResponse>>, ServerError> {
    let account_id = match principal {
        Principal::User(u) => u.account_id,
        Principal::Service(_) => return Err(ServerError::unauthorized("User session required")),
    };

    let client_id = configured_google_client_id().ok_or_else(|| {
        ServerError::invalid_params("GOOGLE_OAUTH_CLIENT_ID não configurado no servidor.")
    })?;
    let redirect_uri = google_name_callback_url()?;
    let account_email: String = sqlx::query_scalar("SELECT nsac_email FROM accounts WHERE id = $1")
        .bind(account_id)
        .fetch_one(&state.db)
        .await?;

    let raw_state = generate_random_token(24);
    let state_hash = sha256_hex_str(&raw_state);
    let verifier = Zeroizing::new(generate_random_token(32));
    let challenge = pkce_challenge_s256(&verifier);
    let pkce_ciphertext = state.crypto.encrypt(&verifier)?;
    let expires_at = Utc::now() + ChronoDuration::minutes(10);

    state
        .connections()
        .create_oauth_state(
            &state_hash,
            account_id,
            "google_name",
            Some(&pkce_ciphertext),
            "android",
            expires_at,
        )
        .await?;

    let mut url = reqwest::Url::parse("https://accounts.google.com/o/oauth2/v2/auth")
        .map_err(|_| ServerError::internal("Invalid Google OAuth URL"))?;
    {
        let mut q = url.query_pairs_mut();
        q.append_pair("client_id", &client_id);
        q.append_pair("redirect_uri", &redirect_uri);
        q.append_pair("response_type", "code");
        q.append_pair("scope", "openid email profile");
        q.append_pair("hd", "unesp.br");
        q.append_pair("login_hint", account_email.trim());
        q.append_pair("prompt", "select_account");
        q.append_pair("state", &raw_state);
        q.append_pair("code_challenge", &challenge);
        q.append_pair("code_challenge_method", "S256");
    }

    Ok(Json(ApiSuccess {
        success: true,
        data: GoogleNameOAuthStartResponse {
            authorize_url: url.into(),
        },
        next_poll_hint_secs: None,
    }))
}

fn render_google_result_html(ok: bool, title: &str, detail: &str) -> Html<String> {
    let accent = if ok { "#10a37f" } else { "#ef4444" };
    let safe_title = title.replace('<', "&lt;").replace('>', "&gt;");
    let safe_detail = detail.replace('<', "&lt;").replace('>', "&gt;");
    Html(format!(
        r#"<!doctype html><html lang="pt-BR"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>{safe_title} — n²sac</title><style>body{{margin:0;min-height:100vh;display:grid;place-items:center;background:#0e0f12;color:#f3f4f6;font-family:system-ui,-apple-system,sans-serif;padding:24px}}.card{{max-width:400px;width:100%;background:#181a20;border:1px solid #2b2f3a;border-radius:24px;padding:28px 24px;text-align:center}}.dot{{width:52px;height:52px;border-radius:16px;background:{accent}22;color:{accent};display:grid;place-items:center;margin:0 auto 14px;font-size:26px;font-weight:800}}h1{{margin:0 0 8px;font-size:1.25rem}}p{{margin:0 0 18px;color:#9ca3af;font-size:.92rem;line-height:1.5}}.hint{{font-size:.82rem;color:#6b7280}}</style></head><body><div class="card"><div class="dot">{icon}</div><h1>{safe_title}</h1><p>{safe_detail}</p><div class="hint">Você já pode voltar para o aplicativo n²sac.</div></div></body></html>"#,
        icon = if ok { "✓" } else { "!" },
    ))
}

#[derive(Deserialize)]
struct GoogleTokenExchangeResp {
    id_token: Option<String>,
}

/// GET /api/v1/account/verify-google-name/oauth/callback
pub async fn verify_google_name_oauth_callback(
    State(state): State<AppState>,
    RawQuery(raw_query): RawQuery,
) -> Html<String> {
    let pairs: Vec<(String, String)> = raw_query
        .as_deref()
        .map(|q| {
            url::form_urlencoded::parse(q.as_bytes())
                .map(|(k, v)| (k.into_owned(), v.into_owned()))
                .collect()
        })
        .unwrap_or_default();
    let find_param = |name: &str| pairs.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str());

    if find_param("error").is_some() {
        return render_google_result_html(
            false,
            "Confirmação cancelada",
            "Você cancelou o login com o Google. Volte ao n²sac para tentar novamente quando quiser.",
        );
    }

    let Some(raw_state) = find_param("state").filter(|s| !s.is_empty()) else {
        return render_google_result_html(false, "Sessão inválida", "Parâmetro de segurança ausente.");
    };
    let Some(code) = find_param("code").filter(|s| !s.is_empty()) else {
        return render_google_result_html(false, "Código ausente", "O Google não retornou um código válido.");
    };

    let state_hash = sha256_hex_str(raw_state);
    let consumed = match state
        .connections()
        .consume_oauth_state(&state_hash, "google_name")
        .await
    {
        Ok(Some(s)) => s,
        _ => {
            return render_google_result_html(
                false,
                "Link expirado",
                "Esta tentativa expirou. Volte ao n²sac e toque em Confirmar novamente.",
            );
        }
    };

    let Some(client_id) = configured_google_client_id() else {
        return render_google_result_html(false, "Não configurado", "GOOGLE_OAUTH_CLIENT_ID ausente.");
    };
    let client_secret = std::env::var("GOOGLE_OAUTH_CLIENT_SECRET")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let Some(client_secret) = client_secret else {
        return render_google_result_html(false, "Não configurado", "GOOGLE_OAUTH_CLIENT_SECRET ausente.");
    };
    let Ok(redirect_uri) = google_name_callback_url() else {
        return render_google_result_html(false, "Erro de configuração", "URL de retorno inválida.");
    };

    let verifier = match consumed.pkce_verifier_ciphertext {
        Some(ct) => state.crypto.decrypt(&ct).ok().map(Zeroizing::new),
        None => None,
    };

    let http = match reqwest::Client::builder().timeout(Duration::from_secs(12)).build() {
        Ok(c) => c,
        Err(_) => return render_google_result_html(false, "Erro interno", "Falha ao contatar o Google."),
    };

    let mut form = vec![
        ("client_id", client_id.as_str()),
        ("client_secret", client_secret.as_str()),
        ("code", code),
        ("redirect_uri", redirect_uri.as_str()),
        ("grant_type", "authorization_code"),
    ];
    if let Some(ref v) = verifier {
        form.push(("code_verifier", v.as_str()));
    }

    let token_resp = match http
        .post("https://oauth2.googleapis.com/token")
        .form(&form)
        .send()
        .await
    {
        Ok(r) if r.status().is_success() => r.json::<GoogleTokenExchangeResp>().await.ok(),
        _ => None,
    };
    let Some(id_token) = token_resp.and_then(|t| t.id_token) else {
        return render_google_result_html(
            false,
            "Falha ao validar com o Google",
            "Não foi possível trocar o código de autorização. Verifique se a conta é @unesp.br e tente novamente.",
        );
    };

    match verify_and_persist_google_id_token(&state, consumed.account_id, &id_token).await {
        Ok(verified_name) => render_google_result_html(
            true,
            "Nome oficial confirmado!",
            &format!("Seu nome foi atualizado para {verified_name}."),
        ),
        Err(err) => render_google_result_html(
            false,
            "Não foi possível confirmar",
            err.user_message(),
        ),
    }
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

    let stored = sqlx::query_as::<_, ProfileRow>(
        "SELECT display_name, bio, avatar_data_url, banner_data_url FROM account_profiles WHERE account_id = $1",
    )
    .bind(account_id)
    .fetch_optional(&state.db)
    .await?;
    let (stored_name, stored_bio, stored_avatar, stored_banner) = match stored {
        Some(r) => (r.display_name, r.bio, r.avatar_data_url, r.banner_data_url),
        None => (None, None, None, None),
    };
    let moderation = state.moderation();

    let cleaned_name = body
        .display_name
        .map(|s| s.trim().chars().take(48).collect::<String>())
        .filter(|s| !s.is_empty());
    let cleaned_bio = body
        .bio
        .map(|s| s.trim().chars().take(240).collect::<String>())
        .filter(|s| !s.is_empty());

    // Restricted accounts may keep or clear, never set new content of that kind.
    let sets_new_text = (cleaned_name.is_some() && cleaned_name != stored_name)
        || (cleaned_bio.is_some() && cleaned_bio != stored_bio);
    if sets_new_text
        && let Some(r) = moderation.restriction(account_id, ReportKind::Text).await?
    {
        return Err(ModerationRepo::restricted_error(&r));
    }
    let sets_new_media = (body.avatar_data_url.is_some() && body.avatar_data_url != stored_avatar)
        || (body.banner_data_url.is_some() && body.banner_data_url != stored_banner);
    if sets_new_media
        && let Some(r) = moderation.restriction(account_id, ReportKind::Media).await?
    {
        return Err(ModerationRepo::restricted_error(&r));
    }

    // Unchanged images are kept as stored (already sanitized; no generation loss). New ones are
    // decoded and re-encoded off the async runtime.
    let avatar = sanitize_if_new(body.avatar_data_url, stored_avatar, ProfileMediaKind::Avatar).await?;
    let banner = sanitize_if_new(body.banner_data_url, stored_banner, ProfileMediaKind::Banner).await?;

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
    .bind(avatar)
    .bind(banner)
    .fetch_one(&state.db)
    .await?;

    let payload = build_user_profile_payload(&state, account_id, Some(row)).await?;

    Ok(Json(ApiSuccess {
        success: true,
        data: payload,
        next_poll_hint_secs: None,
    }))
}

async fn sanitize_if_new(
    incoming: Option<String>,
    stored: Option<String>,
    kind: ProfileMediaKind,
) -> Result<Option<String>, ServerError> {
    match incoming {
        None => Ok(None),
        Some(url) if Some(&url) == stored.as_ref() => Ok(Some(url)),
        Some(url) => tokio::task::spawn_blocking(move || sanitize_profile_image(&url, kind))
            .await
            .map_err(|_| ServerError::internal("Image sanitize task failed"))?
            .map(Some),
    }
}

#[derive(Debug, Deserialize)]
pub struct ReportRequest {
    pub kind: String,
}

#[derive(Debug, Serialize)]
pub struct ReportResponse {
    pub outcome: ReportOutcome,
}

/// POST /api/v1/profiles/{account_id}/report — `{ "kind": "media" | "text" }`.
/// Anonymous: the reported person never learns who reported.
pub async fn post_report(
    State(state): State<AppState>,
    DualAuth(principal): DualAuth,
    Path(account_id): Path<i64>,
    Json(body): Json<ReportRequest>,
) -> Result<Response, ServerError> {
    let viewer = user_account_id(&principal)?;
    let kind = ReportKind::parse(&body.kind).ok_or_else(|| ServerError::invalid_params("Unknown report kind"))?;
    if viewer != account_id {
        // Only content the reporter can actually see can be reported.
        let rel = state.social().relationship(viewer, account_id).await?;
        if rel.blocked_by_me || rel.blocked_me {
            return Err(hidden());
        }
        if !rel.is_friend {
            let owner = profiles::visibility(&state.db, account_id).await?;
            if !(owner.profile_visible() || owner.avatar_visible()) {
                return Err(hidden());
            }
        }
    }
    let outcome = state.moderation().report(viewer, account_id, kind).await?;
    Ok(ok_json(ReportResponse { outcome }))
}

fn user_account_id(principal: &Principal) -> Result<i64, ServerError> {
    match principal {
        Principal::User(u) => Ok(u.account_id),
        Principal::Service(_) => Err(ServerError::unauthorized("User session required")),
    }
}

/// Per-viewer budgets per minute (stop ID sweeping and directory scraping).
const PROFILE_VIEWS_PER_MINUTE: u32 = 90;
const SEARCHES_PER_MINUTE: u32 = 40;
/// `(bucket, viewer)` → (window start, uses in the window).
type BudgetMap = HashMap<(&'static str, i64), (Instant, u32)>;
static BUDGETS: Mutex<Option<BudgetMap>> = Mutex::new(None);

fn consume_budget(bucket: &'static str, viewer: i64, per_minute: u32) -> bool {
    let mut guard = BUDGETS.lock().unwrap_or_else(|e| e.into_inner());
    let map = guard.get_or_insert_with(HashMap::new);
    let now = Instant::now();
    if map.len() > 10_000 {
        map.retain(|_, (start, _)| now.duration_since(*start) < Duration::from_secs(60));
    }
    let slot = map.entry((bucket, viewer)).or_insert((now, 0));
    if now.duration_since(slot.0) >= Duration::from_secs(60) {
        *slot = (now, 0);
    }
    slot.1 += 1;
    slot.1 <= per_minute
}

fn consume_profile_view(viewer: i64) -> bool {
    consume_budget("profile", viewer, PROFILE_VIEWS_PER_MINUTE)
}

fn hidden() -> ServerError {
    // Same answer for "does not exist" and "private": nothing to learn by probing IDs.
    ServerError::not_found("Profile not available")
}

#[derive(Debug, Serialize)]
pub struct PublicProfilePayload {
    pub account_id: i64,
    pub is_me: bool,
    pub display_label: String,
    pub class_code: String,
    pub bio: Option<String>,
    pub avatar_url: Option<String>,
    pub banner_url: Option<String>,
    pub followers_count: i64,
    pub following_count: i64,
    pub relationship: RelationshipStatus,
    #[serde(default)]
    pub connections: Vec<crate::connections::PublicConnectionDto>,
}

/// GET /api/v1/profiles/{account_id}: a classmate's public profile.
pub async fn get_public_profile(
    State(state): State<AppState>,
    DualAuth(principal): DualAuth,
    Path(account_id): Path<i64>,
) -> Result<Response, ServerError> {
    let viewer = user_account_id(&principal)?;
    let is_me = viewer == account_id;
    let relationship = state.social().relationship(viewer, account_id).await?;
    if !is_me {
        if !consume_profile_view(viewer) {
            return Err(ServerError::rate_limited(
                "Muitos perfis abertos em pouco tempo. Tente de novo em instantes.",
                "Profile view budget exceeded",
            ));
        }
        if relationship.blocked_by_me || relationship.blocked_me {
            return Err(hidden());
        }
        let vis = profiles::visibility(&state.db, account_id).await?;
        if !vis.profile_visible() && !relationship.is_friend {
            return Err(hidden());
        }
    }

    let row = profiles::public_profile(&state.db, account_id).await?.ok_or_else(hidden)?;
    let social_repo = state.social();
    let conn_repo = state.connections();
    let (counts, connections) = tokio::try_join!(
        social_repo.follow_counts(account_id),
        conn_repo.list_public_for_account(account_id),
    )?;
    let version = row.version.unwrap_or(0);
    let display_label = format_student_label(
        account_id,
        &row.nsac_email,
        row.hide_name.unwrap_or(false),
        row.ranking_display_name.as_deref(),
        false,
    );
    Ok(ok_json(PublicProfilePayload {
        account_id,
        is_me,
        display_label,
        class_code: row.class_code.unwrap_or_else(|| "CTI".to_string()),
        bio: row.bio,
        avatar_url: row.has_avatar.then(|| avatar_path(account_id, version)),
        banner_url: row
            .has_banner
            .then(|| format!("/api/v1/profiles/{account_id}/banner?v={version}")),
        followers_count: counts.followers_count,
        following_count: counts.following_count,
        relationship,
        connections,
    }))
}

/// GET /api/v1/profiles/{account_id}/avatar
pub async fn get_avatar(
    state: State<AppState>,
    auth: DualAuth,
    path: Path<i64>,
    headers: HeaderMap,
) -> Result<Response, ServerError> {
    serve_image(state, auth, path, headers, ProfileImage::Avatar).await
}

/// GET /api/v1/profiles/{account_id}/banner
pub async fn get_banner(
    state: State<AppState>,
    auth: DualAuth,
    path: Path<i64>,
    headers: HeaderMap,
) -> Result<Response, ServerError> {
    serve_image(state, auth, path, headers, ProfileImage::Banner).await
}

/// Raw image bytes with a strong ETag; URLs carry `?v=<updated_at>`, so they are immutable.
async fn serve_image(
    State(state): State<AppState>,
    DualAuth(principal): DualAuth,
    Path(account_id): Path<i64>,
    headers: HeaderMap,
    kind: ProfileImage,
) -> Result<Response, ServerError> {
    let viewer = user_account_id(&principal)?;
    if viewer != account_id {
        let rel = state.social().relationship(viewer, account_id).await?;
        if rel.blocked_by_me || rel.blocked_me {
            return Err(hidden());
        }
        if !rel.is_friend {
            let owner = profiles::visibility(&state.db, account_id).await?;
            let allowed = match kind {
                ProfileImage::Avatar => owner.avatar_visible(),
                ProfileImage::Banner => owner.profile_visible(),
            };
            if !allowed {
                return Err(hidden());
            }
        }
    }

    let stored = profiles::image(&state.db, account_id, kind).await?.ok_or_else(hidden)?;
    let etag = format!("\"{}-{account_id}-{}\"", match kind { ProfileImage::Avatar => "av", ProfileImage::Banner => "bn" }, stored.version);
    let cache = HeaderValue::from_static("private, max-age=604800, immutable");
    let if_none_match = headers.get(header::IF_NONE_MATCH).and_then(|v| v.to_str().ok());
    if etag_matches_if_none_match(if_none_match, &etag) {
        let mut res = StatusCode::NOT_MODIFIED.into_response();
        res.headers_mut().insert(header::CACHE_CONTROL, cache);
        if let Ok(v) = HeaderValue::from_str(&etag) {
            res.headers_mut().insert(header::ETAG, v);
        }
        return Ok(res);
    }

    let (mime, bytes) = profiles::decode_data_url(&stored.data_url).ok_or_else(hidden)?;
    let mut res = (StatusCode::OK, bytes).into_response();
    let h = res.headers_mut();
    if let Ok(v) = HeaderValue::from_str(&mime) {
        h.insert(header::CONTENT_TYPE, v);
    }
    h.insert(header::CACHE_CONTROL, cache);
    h.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    if let Ok(v) = HeaderValue::from_str(&etag) {
        h.insert(header::ETAG, v);
    }
    Ok(res)
}

#[derive(Debug, Serialize)]
pub struct StudentSearchResult {
    pub account_id: i64,
    pub display_label: String,
    pub class_code: String,
    pub avatar_url: Option<String>,
    pub has_public_profile: bool,
    pub requires_follow_approval: bool,
    pub followers_count: i64,
    pub following_count: i64,
    pub relationship: RelationshipStatus,
}

/// Name tokens and query tokens, accent/case-insensitive.
fn tokens(text: &str) -> Vec<String> {
    nsac_core::filters::normalize_text(text)
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .collect()
}

/// Ranking score of a name for a query (`None` = no match): every query word must start a word
/// of the name ("gab cam" → "Gabriel Camaforte"); a name starting with the query ranks first.
fn name_match_score(name: &str, query: &[String]) -> Option<u8> {
    if query.is_empty() {
        return None;
    }
    let words = tokens(name);
    if !query.iter().all(|q| words.iter().any(|w| w.starts_with(q.as_str()))) {
        return None;
    }
    Some(if words.first().is_some_and(|w| w.starts_with(query[0].as_str())) { 0 } else { 1 })
}

/// GET /api/v1/students/search?q=… — find students by name, or by their exact e-mail.
/// Only students who show their name in the ranking or turned on "be found" appear; invisible
/// students never do. E-mails are matched exactly (never by prefix) and never returned.
pub async fn search_students(
    State(state): State<AppState>,
    DualAuth(principal): DualAuth,
    RawQuery(raw_query): RawQuery,
) -> Result<Response, ServerError> {
    let viewer = user_account_id(&principal)?;
    let pairs = crate::http::extract::parse_query_pairs(raw_query.as_deref());
    let q = pairs
        .iter()
        .find(|(k, _)| k == "q")
        .map(|(_, v)| v.trim().to_string())
        .unwrap_or_default();
    if q.chars().count() < 2 || q.chars().count() > 120 {
        return Ok(ok_json(Vec::<StudentSearchResult>::new()));
    }
    if !consume_budget("search", viewer, SEARCHES_PER_MINUTE) {
        return Err(ServerError::rate_limited(
            "Muitas buscas em pouco tempo. Tente de novo em instantes.",
            "Student search budget exceeded",
        ));
    }

    let by_email = q.contains('@');
    let q_email = q.to_lowercase();
    let q_tokens = tokens(&q);
    let blocked_ids = state.social().blocked_peer_ids(viewer).await?;
    let (out_map, in_map) = state.social().relationship_map_for(viewer).await?;

    let mut hits: Vec<(u8, String, StudentSearchResult)> = Vec::new();
    for c in profiles::search_candidates(&state.db).await? {
        if c.account_id == viewer || blocked_ids.contains(&c.account_id) {
            continue;
        }
        let vis = c.visibility();
        if !vis.findable() {
            continue;
        }
        let label = format_student_label(c.account_id, &c.nsac_email, false, c.ranking_display_name.as_deref(), false);
        // Also match the student's real email-derived name (the name shown in the ranking when a
        // custom profile nickname like "Ayanami-san" is set on `account_profiles`).
        let real_name = format_student_label(c.account_id, &c.nsac_email, false, None, false);
        let score = if by_email {
            (c.nsac_email.trim().to_lowercase() == q_email).then_some(0)
        } else {
            [
                name_match_score(&label, &q_tokens),
                name_match_score(&real_name, &q_tokens),
            ]
            .into_iter()
            .flatten()
            .min()
        };
        let Some(score) = score else { continue };
        let outgoing = out_map
            .get(&c.account_id)
            .copied()
            .unwrap_or(FollowEdgeStatus::None);
        let incoming = in_map
            .get(&c.account_id)
            .copied()
            .unwrap_or(FollowEdgeStatus::None);
        let is_friend =
            outgoing == FollowEdgeStatus::Accepted && incoming == FollowEdgeStatus::Accepted;
        hits.push((
            score,
            nsac_core::filters::normalize_text(&label),
            StudentSearchResult {
                account_id: c.account_id,
                display_label: label,
                class_code: c.class_code.unwrap_or_else(|| "CTI".to_string()),
                avatar_url: (is_friend || vis.avatar_visible())
                    .then_some(c.avatar_version)
                    .flatten()
                    .map(|v| avatar_path(c.account_id, v)),
                has_public_profile: is_friend || vis.profile_visible(),
                requires_follow_approval: !vis.named_in_ranking(),
                followers_count: c.followers_count,
                following_count: c.following_count,
                relationship: RelationshipStatus {
                    outgoing,
                    incoming,
                    is_friend,
                    blocked_by_me: false,
                    blocked_me: false,
                },
            },
        ));
    }
    hits.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    Ok(ok_json(hits.into_iter().take(20).map(|(_, _, r)| r).collect::<Vec<_>>()))
}

#[derive(Debug, Deserialize)]
pub struct UpdatePrivacyRequest {
    #[serde(default)]
    pub discoverable: Option<bool>,
    #[serde(default)]
    pub invisible: Option<bool>,
}

/// PUT /api/v1/account/privacy — "be found in search" and invisible mode (account-wide).
pub async fn put_privacy(
    State(state): State<AppState>,
    DualAuth(principal): DualAuth,
    Json(body): Json<UpdatePrivacyRequest>,
) -> Result<Response, ServerError> {
    let account_id = user_account_id(&principal)?;
    let prefs = state
        .ranking()
        .set_privacy(account_id, body.discoverable, body.invisible)
        .await?;
    Ok(ok_json(prefs))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_search_matches_word_prefixes_accent_insensitively() {
        let q = |s: &str| tokens(s);
        assert_eq!(name_match_score("Gabriel Camaforte", &q("gab")), Some(0));
        assert_eq!(name_match_score("Gabriel Camaforte", &q("cam")), Some(1));
        assert_eq!(name_match_score("Gabriel Camaforte", &q("gab cam")), Some(0));
        assert_eq!(name_match_score("João Conceição", &q("joao conceicao")), Some(0));
        assert_eq!(name_match_score("JOÃO", &q("Jo")), Some(0));
        assert_eq!(name_match_score("Gabriel Camaforte", &q("abriel")), None, "no mid-word match");
        assert_eq!(name_match_score("Gabriel Camaforte", &q("gab silva")), None, "every word must match");
        assert_eq!(name_match_score("Gabriel", &q("  ")), None);
    }
}
