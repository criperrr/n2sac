use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum::extract::{Path, RawQuery, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use sqlx::FromRow;

use crate::auth::{DualAuth, Principal};
use crate::error::ServerError;
use crate::db::profiles::{self, ProfileImage};
use crate::db::ranking::{avatar_path, format_student_label};
use crate::db::social::{FollowEdgeStatus, RelationshipStatus};
use crate::http::envelope::{ApiSuccess, etag_matches_if_none_match, ok_json};
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

pub async fn get_profile(
    State(state): State<AppState>,
    DualAuth(principal): DualAuth,
) -> Result<Json<ApiSuccess<UserProfilePayload>>, ServerError> {
    let account_id = match principal {
        Principal::User(u) => u.account_id,
        Principal::Service(_) => return Err(ServerError::unauthorized("User session required")),
    };

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
    let counts = state.social().follow_counts(account_id).await?;
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
