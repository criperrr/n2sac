use axum::extract::{RawQuery, State};
use axum::response::Response;
use serde::Deserialize;

use crate::auth::{DualAuth, SCOPE_NSAC_READ, SCOPE_NSAC_WRITE, resolve_target_account};
use crate::error::ServerError;
use crate::http::envelope::ok_json;
use crate::http::extract::{AppJson, parse_query_pairs, parse_target_query};
use crate::state::AppState;

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

pub async fn get_ranking_preferences(
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

    let prefs = state.ranking().get_preferences(target.account_id).await?;
    Ok(ok_json(prefs))
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

    if body.display_name.as_deref().is_some_and(|n| !n.trim().is_empty())
        && let Some(r) = state
            .moderation()
            .restriction(target.account_id, crate::db::moderation::ReportKind::Text)
            .await?
    {
        let current = state.ranking().get_preferences(target.account_id).await?;
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

    Ok(ok_json(updated))
}

/// GET /api/v1/compare
/// Query params:
/// - `scope`: `class` (default) | `course` | `school`
/// - `year`: optional school year (defaults to student's latest year)
/// - `bimester`: optional `1`..`4`
/// - `subject`: optional subject name substring
/// - `peer_id`: optional opted-in peer `account_id` for head-to-head subject comparison
pub async fn get_compare(
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

    let mut scope = "class".to_string();
    let mut year_opt: Option<i32> = None;
    let mut bimester_opt: Option<i16> = None;
    let mut subject_opt: Option<String> = None;
    let mut peer_id_opt: Option<i64> = None;

    for (k, v) in &pairs {
        match k.as_str() {
            "scope" => {
                let s = v.trim().to_ascii_lowercase();
                if s == "class" || s == "course" || s == "school" {
                    scope = s;
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

    let school_year = match year_opt {
        Some(y) => y,
        None => {
            state
                .ranking()
                .latest_school_year_for_account(target.account_id)
                .await?
        }
    };

    let response = state
        .ranking()
        .fetch_leaderboard(
            target.account_id,
            target.summary.class_code.as_deref(),
            &scope,
            school_year,
            bimester_opt,
            subject_opt.as_deref(),
            peer_id_opt,
        )
        .await?;

    Ok(ok_json(response))
}
