use axum::Json;
use axum::body::Body;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Serialize;

use crate::error::ServerError;

#[derive(Serialize)]
pub struct ApiSuccess<T: Serialize> {
    pub success: bool,
    pub data: T,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_poll_hint_secs: Option<u64>,
}

pub fn ok_json<T: Serialize>(data: T) -> Response {
    (
        StatusCode::OK,
        Json(ApiSuccess {
            success: true,
            data,
            next_poll_hint_secs: None,
        }),
    )
        .into_response()
}

pub fn etag_matches_if_none_match(header_value: Option<&str>, current_etag: &str) -> bool {
    let Some(raw) = header_value else {
        return false;
    };

    let current_unquoted = current_etag.trim().trim_matches('"').trim();
    if current_unquoted.is_empty() {
        return false;
    }

    for segment in raw.split(',') {
        let token = segment.trim();
        if token.is_empty() {
            continue;
        }
        if token == "*" {
            return true;
        }

        let without_weak = token
            .strip_prefix("W/")
            .or_else(|| token.strip_prefix("w/"))
            .unwrap_or(token)
            .trim();

        let candidate = without_weak
            .strip_prefix('"')
            .and_then(|s| s.strip_suffix('"'))
            .unwrap_or(without_weak)
            .trim();

        if !candidate.is_empty() && candidate == current_unquoted {
            return true;
        }
    }

    false
}

pub fn etag_or_304_response<T: Serialize>(
    headers: &HeaderMap,
    data: T,
    etag: &str,
    next_poll_hint_secs: Option<u64>,
) -> Result<Response, ServerError> {
    let etag_header = HeaderValue::from_str(etag)
        .map_err(|_| ServerError::internal("Failed to construct ETag header value"))?;

    let if_none_match = headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok());

    if etag_matches_if_none_match(if_none_match, etag) {
        return Ok((
            StatusCode::NOT_MODIFIED,
            [(header::ETAG, etag_header)],
            Body::empty(),
        )
            .into_response());
    }

    Ok((
        StatusCode::OK,
        [(header::ETAG, etag_header)],
        Json(ApiSuccess {
            success: true,
            data,
            next_poll_hint_secs,
        }),
    )
        .into_response())
}
