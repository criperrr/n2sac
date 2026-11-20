use std::future::Future;
use std::sync::Arc;

use axum::extract::{Path, RawQuery, State};
use axum::http::HeaderMap;
use axum::response::Response;
use nsac_core::error::{AppError, ErrorCode};
use nsac_core::filters::{QueryFilter, filter_query};
use nsac_core::models::{AllYearsResponse, NsacSessionCookies};
use serde::Serialize;

use crate::auth::{DualAuth, SCOPE_NSAC_READ, resolve_target_account};
use crate::db::accounts::{CTI_CLASSES, normalize_class_code};
use crate::db::snapshots::compute_strong_etag;
use crate::error::ServerError;
use crate::http::envelope::etag_or_304_response;
use crate::http::extract::{parse_ficha_id, parse_query_pairs, parse_target_query};
use crate::state::{AppState, CachedBoletim, CachedSnapshot, RouteKey};

pub fn canonicalize_boletim(boletim: &mut AllYearsResponse) {
    boletim
        .data
        .sort_by(|a, b| a.year.cmp(&b.year).then_with(|| a.title.cmp(&b.title)));
    for year in &mut boletim.data {
        year.grades
            .sort_by(|a, b| a.subject_name.cmp(&b.subject_name));
        for subject in &mut year.grades {
            subject.bimesters.sort_by_key(|b| b.bimester);
        }
    }
    boletim.user_current_year = boletim.data.len();
}

async fn execute_nsac_with_refresh<T, F, Fut>(
    state: &AppState,
    account_id: i64,
    mut op: F,
) -> Result<T, ServerError>
where
    F: FnMut(NsacSessionCookies) -> Fut,
    Fut: Future<Output = Result<(T, Option<NsacSessionCookies>), AppError>>,
{
    let secrets = state
        .accounts()
        .get_decrypted_credentials(&state.crypto, account_id)
        .await?
        .ok_or_else(|| ServerError::user_not_linked("Conta não encontrada."))?;

    if !secrets.active {
        return Err(ServerError::forbidden("A conta está desativada."));
    }

    if let Some(cookies) = secrets.cookies {
        let attempt = {
            let _permit = state.semaphore.acquire().await?;
            op(cookies).await
        };

        match attempt {
            Ok((value, maybe_rotated)) => {
                if let Some(ref rotated) = maybe_rotated {
                    state
                        .accounts()
                        .update_cookies(&state.crypto, account_id, rotated, state.clock.now())
                        .await?;
                }
                return Ok(value);
            }
            Err(err) if err.code == ErrorCode::NsacAuthExpired => {}
            Err(err) => return Err(ServerError::from(err)),
        }
    }

    let fresh_cookies = {
        let _permit = state.semaphore.acquire().await?;
        state
            .nsac
            .login(&secrets.nsac_email, secrets.password.as_str())
            .await?
    };

    state
        .accounts()
        .update_cookies(
            &state.crypto,
            account_id,
            &fresh_cookies,
            state.clock.now(),
        )
        .await?;

    let retry = {
        let _permit = state.semaphore.acquire().await?;
        op(fresh_cookies).await
    };

    match retry {
        Ok((value, maybe_rotated)) => {
            if let Some(ref rotated) = maybe_rotated {
                state
                    .accounts()
                    .update_cookies(&state.crypto, account_id, rotated, state.clock.now())
                    .await?;
            }
            Ok(value)
        }
        Err(err) => Err(ServerError::from(err)),
    }
}

async fn fetch_and_persist_boletim(
    state: &AppState,
    account_id: i64,
) -> Result<CachedBoletim, ServerError> {
    let nsac = state.nsac.clone();
    let mut live_boletim =
        execute_nsac_with_refresh(state, account_id, move |cookies| {
            let client = nsac.clone();
            async move { client.fetch_boletim(&cookies).await }
        })
        .await?;

    canonicalize_boletim(&mut live_boletim);

    let now = state.clock.now();
    if let Some(code) = live_boletim
        .current_class_code()
        .and_then(normalize_class_code)
    {
        if !CTI_CLASSES.contains(&code.as_str()) {
            tracing::warn!(
                class_code = %code,
                "Derived NSAC class_code is outside known CTI_CLASSES"
            );
        }
        state
            .accounts()
            .update_class_code(account_id, &code, now)
            .await?;
    }

    state
        .boletim()
        .upsert_boletim(account_id, &live_boletim)
        .await?;

    let canonical = state
        .boletim()
        .get_boletim(account_id)
        .await?
        .unwrap_or(live_boletim);
    let etag = compute_strong_etag(&canonical)?;
    let cached = CachedBoletim {
        data: Arc::new(canonical),
        etag,
        cached_at: now,
    };
    state
        .caches
        .boletim
        .insert(account_id, cached.clone())
        .await;
    Ok(cached)
}

pub async fn get_boletim(
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

    let filter_pairs = pairs
        .into_iter()
        .filter(|(k, _)| !matches!(k.trim(), "account_id" | "discord_id" | "refresh"));
    let query_filter = QueryFilter::from_query_pairs(filter_pairs)?;

    let cached = if target_query.refresh {
        fetch_and_persist_boletim(&state, target.account_id).await?
    } else if let Some(hit) = state.caches.boletim.get(&target.account_id).await {
        hit
    } else if let Some(mut db_boletim) = state.boletim().get_boletim(target.account_id).await? {
        canonicalize_boletim(&mut db_boletim);
        let etag = compute_strong_etag(&db_boletim)?;
        let entry = CachedBoletim {
            data: Arc::new(db_boletim),
            etag,
            cached_at: state.clock.now(),
        };
        state
            .caches
            .boletim
            .insert(target.account_id, entry.clone())
            .await;
        entry
    } else {
        fetch_and_persist_boletim(&state, target.account_id).await?
    };

    let now = state.clock.now();
    let class_code = target
        .class_code
        .as_deref()
        .or_else(|| cached.data.current_class_code());
    let next_poll_hint_secs = match class_code.and_then(normalize_class_code) {
        Some(ref code) => state.heuristic.record_api_activity(code, now).await?,
        None => state.heuristic.get_poll_hint_secs(None, now).await,
    };

    if query_filter == QueryFilter::default() {
        etag_or_304_response(
            &headers,
            cached.data.as_ref(),
            &cached.etag,
            Some(next_poll_hint_secs),
        )
    } else {
        let filtered = filter_query(cached.data.as_ref(), &query_filter)?;
        let post_filter_etag = compute_strong_etag(&filtered)?;
        etag_or_304_response(
            &headers,
            &filtered,
            &post_filter_etag,
            Some(next_poll_hint_secs),
        )
    }
}

async fn handle_auxiliary_route<T, F, Fut>(
    state: AppState,
    principal: crate::auth::Principal,
    raw_query: Option<String>,
    headers: HeaderMap,
    route_key: RouteKey,
    fetch_fn: F,
) -> Result<Response, ServerError>
where
    T: Serialize,
    F: FnMut(NsacSessionCookies) -> Fut,
    Fut: Future<Output = Result<(T, Option<NsacSessionCookies>), AppError>>,
{
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

    let route_key_str = route_key.as_str();
    let cache_key = (target.account_id, route_key_str.clone());

    let (payload_json, etag) = if !target_query.refresh
        && let Some(hit) = state.caches.snapshots.get(&cache_key).await
    {
        (hit.payload_json, hit.etag)
    } else {
        match execute_nsac_with_refresh(&state, target.account_id, fetch_fn).await {
            Ok(value) => {
                let payload_json = serde_json::to_value(&value)?;
                let etag = compute_strong_etag(&payload_json)?;
                let now = state.clock.now();

                state
                    .snapshots()
                    .upsert(target.account_id, &route_key_str, &payload_json, &etag, now)
                    .await?;
                state
                    .caches
                    .snapshots
                    .insert(
                        cache_key,
                        CachedSnapshot {
                            payload_json: payload_json.clone(),
                            etag: etag.clone(),
                            updated_at: now,
                        },
                    )
                    .await;

                (payload_json, etag)
            }
            Err(err)
                if err.error_code() == ErrorCode::NsacUnavailable && !target_query.refresh =>
            {
                if let Some(snap) = state
                    .snapshots()
                    .get(target.account_id, &route_key_str)
                    .await?
                {
                    state
                        .caches
                        .snapshots
                        .insert(
                            cache_key,
                            CachedSnapshot {
                                payload_json: snap.payload_json.clone(),
                                etag: snap.etag.clone(),
                                updated_at: snap.updated_at,
                            },
                        )
                        .await;
                    (snap.payload_json, snap.etag)
                } else {
                    return Err(err);
                }
            }
            Err(err) => return Err(err),
        }
    };

    let next_poll_hint_secs = state
        .heuristic
        .get_poll_hint_secs(target.class_code.as_deref(), state.clock.now())
        .await;

    etag_or_304_response(&headers, payload_json, &etag, Some(next_poll_hint_secs))
}

pub async fn get_home(
    State(state): State<AppState>,
    DualAuth(principal): DualAuth,
    RawQuery(raw_query): RawQuery,
    headers: HeaderMap,
) -> Result<Response, ServerError> {
    let nsac = state.nsac.clone();
    handle_auxiliary_route(
        state,
        principal,
        raw_query,
        headers,
        RouteKey::Home,
        move |cookies| {
            let client = nsac.clone();
            async move { client.fetch_home(&cookies).await }
        },
    )
    .await
}

pub async fn get_atrasos(
    State(state): State<AppState>,
    DualAuth(principal): DualAuth,
    RawQuery(raw_query): RawQuery,
    headers: HeaderMap,
) -> Result<Response, ServerError> {
    let nsac = state.nsac.clone();
    handle_auxiliary_route(
        state,
        principal,
        raw_query,
        headers,
        RouteKey::Atrasos,
        move |cookies| {
            let client = nsac.clone();
            async move { client.fetch_atrasos(&cookies).await }
        },
    )
    .await
}

pub async fn get_saidas(
    State(state): State<AppState>,
    DualAuth(principal): DualAuth,
    RawQuery(raw_query): RawQuery,
    headers: HeaderMap,
) -> Result<Response, ServerError> {
    let nsac = state.nsac.clone();
    handle_auxiliary_route(
        state,
        principal,
        raw_query,
        headers,
        RouteKey::Saidas,
        move |cookies| {
            let client = nsac.clone();
            async move { client.fetch_saidas(&cookies).await }
        },
    )
    .await
}

pub async fn get_ficha_list(
    State(state): State<AppState>,
    DualAuth(principal): DualAuth,
    RawQuery(raw_query): RawQuery,
    headers: HeaderMap,
) -> Result<Response, ServerError> {
    let nsac = state.nsac.clone();
    handle_auxiliary_route(
        state,
        principal,
        raw_query,
        headers,
        RouteKey::FichaList,
        move |cookies| {
            let client = nsac.clone();
            async move { client.fetch_ficha_avaliacao(&cookies).await }
        },
    )
    .await
}

pub async fn get_ficha_detail(
    State(state): State<AppState>,
    DualAuth(principal): DualAuth,
    Path(raw_id): Path<String>,
    RawQuery(raw_query): RawQuery,
    headers: HeaderMap,
) -> Result<Response, ServerError> {
    let id = parse_ficha_id(&raw_id)?;
    let nsac = state.nsac.clone();
    handle_auxiliary_route(
        state,
        principal,
        raw_query,
        headers,
        RouteKey::FichaDetail(id),
        move |cookies| {
            let client = nsac.clone();
            async move { client.fetch_ficha_avaliacao_detail(&cookies, id).await }
        },
    )
    .await
}
