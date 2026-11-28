use std::sync::Arc;

use axum::extract::{Path, RawQuery, State};
use axum::http::HeaderMap;
use axum::response::Response;
use serde::Deserialize;

use crate::auth::{DualAuth, SCOPE_NSAC_READ, SCOPE_NSAC_WRITE, resolve_target_account};
use crate::db::ranking::{
    ClassroomRankingQuery, ClassroomRankingScope, ClassroomRankingSortBy, RankingPreferences,
    RankingQuery, RankingScope, RankingSortBy, YearRankingDataset, build_classroom_comparison_from_dataset,
    build_classroom_ranking, build_comparison_summary, build_custom_slice_analytics,
    build_peer_comparison_from_dataset, normalize_subject_key,
};
use crate::db::snapshots::compute_strong_etag;
use crate::error::ServerError;
use crate::http::envelope::{etag_or_304_response, ok_json};
use crate::http::extract::{AppJson, parse_query_pairs, parse_target_query};
use crate::state::AppState;

const COMPARE_POLL_HINT_SECS: u64 = 120;
const RANKING_PREFS_POLL_HINT_SECS: u64 = 300;

#[derive(Deserialize)]
pub struct UpdateRankingPreferencesRequest {
    pub opted_in: bool,
    #[serde(default)]
    pub hide_name: bool,
    #[serde(default)]
    pub display_name: Option<String>,
    /// Omitted by older clients: the stored value is kept.
    #[serde(default)]
    pub show_avatar: Option<bool>,
    #[serde(default)]
    pub public_profile: Option<bool>,
}

async fn get_cached_ranking_prefs(
    state: &AppState,
    account_id: i64,
) -> Result<RankingPreferences, ServerError> {
    if let Some(cached) = state.caches.ranking_prefs.get(&account_id).await {
        return Ok(cached);
    }
    let prefs = state.ranking().get_preferences(account_id).await?;
    state
        .caches
        .ranking_prefs
        .insert(account_id, prefs.clone())
        .await;
    Ok(prefs)
}

async fn get_cached_year_dataset(
    state: &AppState,
    school_year: i32,
) -> Result<Arc<YearRankingDataset>, ServerError> {
    if let Some(cached) = state.caches.ranking_datasets.get(&school_year).await {
        return Ok(cached);
    }
    let loaded = Arc::new(state.ranking().load_year_dataset(school_year).await?);
    state
        .caches
        .ranking_datasets
        .insert(school_year, Arc::clone(&loaded))
        .await;
    Ok(loaded)
}

fn parse_optional_bool(raw: &str) -> Option<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" => Some(true),
        "false" | "0" | "no" => Some(false),
        _ => None,
    }
}

pub async fn get_ranking_preferences(
    State(state): State<AppState>,
    DualAuth(principal): DualAuth,
    RawQuery(raw_query): RawQuery,
    headers: HeaderMap,
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

    let prefs = get_cached_ranking_prefs(&state, target.account_id).await?;
    let etag = compute_strong_etag(&prefs)?;
    etag_or_304_response(&headers, prefs, &etag, Some(RANKING_PREFS_POLL_HINT_SECS))
}

pub async fn put_ranking_preferences(
    State(state): State<AppState>,
    DualAuth(principal): DualAuth,
    RawQuery(raw_query): RawQuery,
    AppJson(body): AppJson<UpdateRankingPreferencesRequest>,
) -> Result<Response, ServerError> {
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

    if body
        .display_name
        .as_deref()
        .is_some_and(|n| !n.trim().is_empty())
        && let Some(r) = state
            .moderation()
            .restriction(target.account_id, crate::db::moderation::ReportKind::Text)
            .await?
    {
        let current = get_cached_ranking_prefs(&state, target.account_id).await?;
        if current.display_name.as_deref() != body.display_name.as_deref().map(str::trim) {
            return Err(crate::db::ModerationRepo::restricted_error(&r));
        }
    }

    let updated = state
        .ranking()
        .upsert_preferences(
            target.account_id,
            body.opted_in,
            body.hide_name,
            body.display_name.as_deref(),
            body.show_avatar,
            body.public_profile,
        )
        .await?;

    state.caches.invalidate_ranking_prefs(target.account_id).await;
    state.caches.invalidate_all_rankings().await;

    Ok(ok_json(updated))
}

/// GET /api/v1/compare
/// Query params:
/// - `scope`: `class` (default) | `grade_level` | `course` | `school` | `friends`
/// - `sort_by`: `personal_average` (default) | `delta_vs_class` | `momentum` | `consistency`
/// - `common_only`: optional `true` | `false`
/// - `year`: optional school year (defaults to student's latest year)
/// - `bimester`: optional `1`..`4`
/// - `subject`: optional subject name substring (accent-insensitive)
/// - `peer_id`: optional opted-in peer `account_id` for head-to-head subject comparison
pub async fn get_compare(
    State(state): State<AppState>,
    DualAuth(principal): DualAuth,
    RawQuery(raw_query): RawQuery,
    headers: HeaderMap,
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

    let mut scope = RankingScope::Class;
    let mut sort_by = RankingSortBy::PersonalAverage;
    let mut common_only_opt: Option<bool> = None;
    let mut year_opt: Option<i32> = None;
    let mut bimester_opt: Option<i16> = None;
    let mut subject_opt: Option<String> = None;
    let mut peer_id_opt: Option<i64> = None;

    for (k, v) in &pairs {
        match k.as_str() {
            "scope" => {
                if let Some(parsed) = RankingScope::parse(v) {
                    scope = parsed;
                }
            }
            "sort_by" | "sortBy" => {
                if let Some(parsed) = RankingSortBy::parse(v) {
                    sort_by = parsed;
                }
            }
            "common_only" | "commonOnly" => {
                if let Some(parsed) = parse_optional_bool(v) {
                    common_only_opt = Some(parsed);
                }
            }
            "year" | "schoolYear" | "school_year" => {
                if let Ok(y) = v.trim().parse::<i32>() {
                    year_opt = Some(y);
                }
            }
            "bimester" | "targetBimester" => {
                if let Ok(b) = v.trim().parse::<i16>()
                    && (1..=4).contains(&b)
                {
                    bimester_opt = Some(b);
                }
            }
            "subject" | "subjectName" => {
                let trimmed = v.trim();
                if !trimmed.is_empty() {
                    subject_opt = Some(trimmed.to_string());
                }
            }
            "peer_id" | "peerId" => {
                if let Ok(pid) = v.trim().parse::<i64>() {
                    peer_id_opt = Some(pid);
                }
            }
            _ => {}
        }
    }

    let available_years = state
        .ranking()
        .available_school_years_for_account(target.account_id)
        .await?;

    let school_year = match year_opt {
        Some(y) if available_years.is_empty() || available_years.contains(&y) => y,
        _ => available_years
            .first()
            .copied()
            .unwrap_or_else(|| chrono::Utc::now().format("%Y").to_string().parse().unwrap_or(2026)),
    };

    let ranking_repo = state.ranking();
    let (my_prefs, dataset, social) = tokio::try_join!(
        get_cached_ranking_prefs(&state, target.account_id),
        get_cached_year_dataset(&state, school_year),
        ranking_repo.load_requester_social_context(target.account_id),
    )?;

    let query = RankingQuery {
        scope,
        sort_by,
        bimester: bimester_opt,
        subject_filter: subject_opt,
        common_only: common_only_opt,
        compare_with_account_id: peer_id_opt,
    };

    let mut response = build_comparison_summary(
        &dataset,
        target.account_id,
        target.summary.class_code.as_deref(),
        my_prefs,
        &social,
        &query,
    );
    response.available_years = available_years;

    let etag = compute_strong_etag(&response)?;
    etag_or_304_response(&headers, response, &etag, Some(COMPARE_POLL_HINT_SECS))
}

/// GET /api/v1/compare/peer/{peer_id}
/// Dedicated in-memory head-to-head comparison endpoint (ETag-capable, zero grade DB queries on cache hit).
pub async fn get_peer_comparison(
    State(state): State<AppState>,
    DualAuth(principal): DualAuth,
    Path(peer_id): Path<i64>,
    RawQuery(raw_query): RawQuery,
    headers: HeaderMap,
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

    let mut year_opt: Option<i32> = None;
    let mut bimester_opt: Option<i16> = None;
    for (k, v) in &pairs {
        match k.as_str() {
            "year" | "schoolYear" | "school_year" => {
                if let Ok(y) = v.trim().parse::<i32>() {
                    year_opt = Some(y);
                }
            }
            "bimester" | "targetBimester" => {
                if let Ok(b) = v.trim().parse::<i16>()
                    && (1..=4).contains(&b)
                {
                    bimester_opt = Some(b);
                }
            }
            _ => {}
        }
    }

    let available_years = state
        .ranking()
        .available_school_years_for_account(target.account_id)
        .await?;
    let school_year = match year_opt {
        Some(y) if available_years.is_empty() || available_years.contains(&y) => y,
        _ => available_years
            .first()
            .copied()
            .unwrap_or_else(|| chrono::Utc::now().format("%Y").to_string().parse().unwrap_or(2026)),
    };

    let ranking_repo = state.ranking();
    let (dataset, social) = tokio::try_join!(
        get_cached_year_dataset(&state, school_year),
        ranking_repo.load_requester_social_context(target.account_id),
    )?;

    let detail = build_peer_comparison_from_dataset(
        &dataset,
        target.account_id,
        peer_id,
        &social,
        bimester_opt,
        None,
    )
    .ok_or_else(|| {
        ServerError::with_user_message(
            nsac_core::error::ErrorCode::NotFound,
            "Estudante não disponível para comparação.",
            format!("Peer {peer_id} not eligible or not found for comparison"),
        )
    })?;

    let etag = compute_strong_etag(&detail)?;
    etag_or_304_response(&headers, detail, &etag, Some(COMPARE_POLL_HINT_SECS))
}

#[derive(Deserialize)]
pub struct CustomAnalyticsRequest {
    #[serde(default)]
    pub school_year: Option<i32>,
    #[serde(default)]
    pub subjects: Option<Vec<String>>,
    #[serde(default)]
    pub bimesters: Option<Vec<i16>>,
}

/// POST /api/v1/compare/custom-analytics
/// Fast, cache-backed aggregated analytics for a custom subset of subjects & bimesters.
pub async fn post_custom_analytics(
    State(state): State<AppState>,
    DualAuth(principal): DualAuth,
    RawQuery(raw_query): RawQuery,
    AppJson(body): AppJson<CustomAnalyticsRequest>,
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

    let school_year = match body.school_year {
        Some(y) => y,
        None => state
            .ranking()
            .latest_school_year_for_account(target.account_id)
            .await?,
    };

    let primary_dataset = get_cached_year_dataset(&state, school_year).await?;
    let mut other_datasets: Vec<Arc<YearRankingDataset>> = Vec::new();
    for yr in [school_year - 2, school_year - 1, school_year + 1] {
        if let Ok(ds) = get_cached_year_dataset(&state, yr).await
            && !ds.accounts.is_empty()
        {
            other_datasets.push(ds);
        }
    }
    let other_refs: Vec<&YearRankingDataset> = other_datasets.iter().map(Arc::as_ref).collect();

    let res = build_custom_slice_analytics(
        &primary_dataset,
        &other_refs,
        target.account_id,
        body.subjects.as_deref(),
        body.bimesters.as_deref(),
    );
    Ok(ok_json(res))
}

/// GET /api/v1/compare/classrooms
/// Query params:
/// - `scope`: `all` (default) | `grade_level` | `course` | `integrado` | `noturno`
/// - `sort_by`: `official_average` (default) | `momentum` | `consistency`
/// - `common_only`: optional `true` | `false`
/// - `year`: optional school year (defaults to student's latest year)
/// - `bimester`: optional `1`..`4`
/// - `subject`: optional subject name substring (accent-insensitive)
/// - `compare_class`: optional target class code for head-to-head classroom comparison
pub async fn get_classroom_ranking(
    State(state): State<AppState>,
    DualAuth(principal): DualAuth,
    RawQuery(raw_query): RawQuery,
    headers: HeaderMap,
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

    let mut scope = ClassroomRankingScope::All;
    let mut sort_by = ClassroomRankingSortBy::OfficialAverage;
    let mut common_only_opt: Option<bool> = None;
    let mut year_opt: Option<i32> = None;
    let mut bimester_opt: Option<i16> = None;
    let mut subject_opt: Option<String> = None;
    let mut compare_class_opt: Option<String> = None;

    for (k, v) in &pairs {
        match k.as_str() {
            "scope" => {
                if let Some(parsed) = ClassroomRankingScope::parse(v) {
                    scope = parsed;
                }
            }
            "sort_by" | "sortBy" => {
                if let Some(parsed) = ClassroomRankingSortBy::parse(v) {
                    sort_by = parsed;
                }
            }
            "common_only" | "commonOnly" => {
                if let Some(parsed) = parse_optional_bool(v) {
                    common_only_opt = Some(parsed);
                }
            }
            "year" | "schoolYear" | "school_year" => {
                if let Ok(y) = v.trim().parse::<i32>() {
                    year_opt = Some(y);
                }
            }
            "bimester" | "targetBimester" => {
                if let Ok(b) = v.trim().parse::<i16>()
                    && (1..=4).contains(&b)
                {
                    bimester_opt = Some(b);
                }
            }
            "subject" | "subjectName" => {
                let trimmed = v.trim();
                if !trimmed.is_empty() {
                    subject_opt = Some(trimmed.to_string());
                }
            }
            "compare_class" | "compareClass" => {
                let trimmed = v.trim();
                if !trimmed.is_empty() {
                    compare_class_opt = Some(trimmed.to_ascii_uppercase());
                }
            }
            _ => {}
        }
    }

    let available_years = state
        .ranking()
        .available_school_years_for_account(target.account_id)
        .await?;

    let school_year = match year_opt {
        Some(y) if available_years.is_empty() || available_years.contains(&y) => y,
        _ => available_years
            .first()
            .copied()
            .unwrap_or_else(|| chrono::Utc::now().format("%Y").to_string().parse().unwrap_or(2026)),
    };

    let dataset = get_cached_year_dataset(&state, school_year).await?;
    let query = ClassroomRankingQuery {
        scope,
        sort_by,
        bimester: bimester_opt,
        subject_filter: subject_opt,
        common_only: common_only_opt,
        compare_class_code: compare_class_opt,
    };

    let mut response = build_classroom_ranking(
        &dataset,
        target.account_id,
        target.summary.class_code.as_deref(),
        &query,
    );
    response.available_years = available_years;

    let etag = compute_strong_etag(&response)?;
    etag_or_304_response(&headers, response, &etag, Some(COMPARE_POLL_HINT_SECS))
}

/// GET /api/v1/compare/classrooms/{class_code}
/// Head-to-head comparison between the requester's classroom and `class_code`.
pub async fn get_classroom_comparison(
    State(state): State<AppState>,
    DualAuth(principal): DualAuth,
    Path(class_code): Path<String>,
    RawQuery(raw_query): RawQuery,
    headers: HeaderMap,
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

    let mut year_opt: Option<i32> = None;
    let mut bimester_opt: Option<i16> = None;
    let mut subject_opt: Option<String> = None;
    for (k, v) in &pairs {
        match k.as_str() {
            "year" | "schoolYear" | "school_year" => {
                if let Ok(y) = v.trim().parse::<i32>() {
                    year_opt = Some(y);
                }
            }
            "bimester" | "targetBimester" => {
                if let Ok(b) = v.trim().parse::<i16>()
                    && (1..=4).contains(&b)
                {
                    bimester_opt = Some(b);
                }
            }
            "subject" | "subjectName" => {
                let trimmed = v.trim();
                if !trimmed.is_empty() {
                    subject_opt = Some(trimmed.to_string());
                }
            }
            _ => {}
        }
    }

    let available_years = state
        .ranking()
        .available_school_years_for_account(target.account_id)
        .await?;
    let school_year = match year_opt {
        Some(y) if available_years.is_empty() || available_years.contains(&y) => y,
        _ => available_years
            .first()
            .copied()
            .unwrap_or_else(|| chrono::Utc::now().format("%Y").to_string().parse().unwrap_or(2026)),
    };

    let dataset = get_cached_year_dataset(&state, school_year).await?;
    let my_class_code = dataset
        .accounts
        .get(&target.account_id)
        .map(|a| a.class_code.as_str())
        .or(target.summary.class_code.as_deref())
        .unwrap_or("");

    let subject_norm = subject_opt
        .as_deref()
        .map(normalize_subject_key)
        .filter(|s| !s.is_empty());

    let detail = build_classroom_comparison_from_dataset(
        &dataset,
        my_class_code,
        &class_code,
        bimester_opt,
        subject_norm.as_deref(),
    )
    .ok_or_else(|| {
        ServerError::with_user_message(
            nsac_core::error::ErrorCode::NotFound,
            "Turma não disponível para comparação.",
            format!("Classroom comparison unavailable for {my_class_code} vs {class_code}"),
        )
    })?;

    let etag = compute_strong_etag(&detail)?;
    etag_or_304_response(&headers, detail, &etag, Some(COMPARE_POLL_HINT_SECS))
}
