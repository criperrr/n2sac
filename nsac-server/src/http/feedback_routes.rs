//! User feedback, suggestions, and bug report endpoints + service-scoped admin triage routes.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use nsac_core::error::ErrorCode;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::auth::{DualAuth, Principal, SCOPE_FEEDBACK_READ, SCOPE_FEEDBACK_WRITE, has_scope};
use crate::db::feedback::{
    AdminFeedbackFilter, BugFrequency, BugSeverity, FeedbackKind, FeedbackStatus,
    MAX_BUG_ATTACHMENTS, NewFeedbackSubmission, clean_multiline, collapse_whitespace,
    compute_content_hash, is_valid_area, validate_structured_text,
};
use crate::error::ServerError;
use crate::http::envelope::ApiSuccess;
use crate::http::extract::AppJson;
use crate::media::sanitize_feedback_screenshot;
use crate::state::AppState;

const MAX_CONTEXT_JSON_BYTES: usize = 24_576;
const MAX_DEV_NOTE_CHARS: usize = 600;

fn require_user(principal: &Principal) -> Result<i64, ServerError> {
    match principal {
        Principal::User(u) => Ok(u.account_id),
        Principal::Service(_) => Err(ServerError::unauthorized("User session required")),
    }
}

fn require_service_scope(principal: &Principal, required_scope: &str) -> Result<(), ServerError> {
    match principal {
        Principal::Service(svc) => {
            if has_scope(&svc.scopes, required_scope) {
                Ok(())
            } else {
                Err(ServerError::forbidden(format!(
                    "Service key missing required scope '{required_scope}'"
                )))
            }
        }
        Principal::User(_) => Err(ServerError::forbidden(
            "Admin service API key required for feedback triage",
        )),
    }
}

#[derive(Debug, Deserialize)]
pub struct CreateFeedbackRequest {
    pub kind: String,
    pub area: String,
    pub title: String,
    // Bug fields
    pub severity: Option<String>,
    pub frequency: Option<String>,
    pub steps_to_reproduce: Option<String>,
    pub actual_behavior: Option<String>,
    pub expected_behavior: Option<String>,
    // Suggestion fields
    pub problem: Option<String>,
    pub proposal: Option<String>,
    // Feedback fields
    pub rating: Option<i16>,
    pub message: Option<String>,
    // Bug-only screenshots (base64 data URLs)
    #[serde(default)]
    pub attachments: Vec<String>,
    // Optional transparent technical context JSON
    pub context: Option<Value>,
}

/// POST /api/v1/feedback — authenticated student submission (never anonymous).
pub async fn post_feedback(
    State(state): State<AppState>,
    DualAuth(principal): DualAuth,
    AppJson(req): AppJson<CreateFeedbackRequest>,
) -> Result<Response, ServerError> {
    let account_id = require_user(&principal)?;

    let kind = FeedbackKind::parse(&req.kind).ok_or_else(|| {
        ServerError::with_user_message(
            ErrorCode::InvalidParams,
            "Categoria inválida. Escolha Bug, Sugestão ou Feedback.",
            "Invalid feedback kind",
        )
    })?;

    let area = req.area.trim().to_string();
    if !is_valid_area(kind, &area) {
        return Err(ServerError::with_user_message(
            ErrorCode::InvalidParams,
            "Selecione uma área válida do aplicativo.",
            format!("Invalid feedback area: {area}"),
        ));
    }

    let title_min = if kind == FeedbackKind::Feedback { 8 } else { 10 };
    let title = collapse_whitespace(&validate_structured_text(
        "Título",
        &req.title,
        title_min,
        100,
        2,
        5,
    )?);

    // Screenshots are strictly allowed on bug reports only.
    if kind != FeedbackKind::Bug && !req.attachments.is_empty() {
        return Err(ServerError::with_user_message(
            ErrorCode::InvalidParams,
            "O envio de prints é permitido apenas ao reportar bugs.",
            "Attachments sent for non-bug feedback",
        ));
    }
    if req.attachments.len() > MAX_BUG_ATTACHMENTS {
        return Err(ServerError::with_user_message(
            ErrorCode::InvalidParams,
            format!("Envie no máximo {MAX_BUG_ATTACHMENTS} imagens por bug."),
            "Too many feedback attachments",
        ));
    }

    let (severity, frequency, rating, body_json) = match kind {
        FeedbackKind::Bug => {
            let severity = req
                .severity
                .as_deref()
                .and_then(BugSeverity::parse)
                .ok_or_else(|| {
                    ServerError::with_user_message(
                        ErrorCode::InvalidParams,
                        "Selecione a gravidade do erro.",
                        "Missing/invalid bug severity",
                    )
                })?;
            let frequency = req
                .frequency
                .as_deref()
                .and_then(BugFrequency::parse)
                .ok_or_else(|| {
                    ServerError::with_user_message(
                        ErrorCode::InvalidParams,
                        "Selecione a frequência em que o erro acontece.",
                        "Missing/invalid bug frequency",
                    )
                })?;
            let steps = validate_structured_text(
                "Passo a passo",
                req.steps_to_reproduce.as_deref().unwrap_or(""),
                20,
                1_200,
                4,
                8,
            )?;
            let actual = validate_structured_text(
                "O que aconteceu de errado",
                req.actual_behavior.as_deref().unwrap_or(""),
                15,
                800,
                4,
                8,
            )?;
            let expected_raw = req.expected_behavior.as_deref().unwrap_or("");
            let expected = clean_multiline(expected_raw);
            if expected.chars().count() > 500 {
                return Err(ServerError::with_user_message(
                    ErrorCode::InvalidParams,
                    "Resultado esperado: limite máximo de 500 caracteres.",
                    "expected_behavior too long",
                ));
            }
            let body = if expected.is_empty() {
                json!({
                    "steps_to_reproduce": steps,
                    "actual_behavior": actual,
                })
            } else {
                json!({
                    "steps_to_reproduce": steps,
                    "actual_behavior": actual,
                    "expected_behavior": expected,
                })
            };
            (Some(severity), Some(frequency), None, body)
        }
        FeedbackKind::Suggestion => {
            let problem = validate_structured_text(
                "O que falta ou incomoda hoje",
                req.problem.as_deref().unwrap_or(""),
                20,
                800,
                4,
                8,
            )?;
            let proposal = validate_structured_text(
                "Como você imagina funcionando",
                req.proposal.as_deref().unwrap_or(""),
                20,
                1_200,
                4,
                8,
            )?;
            (
                None,
                None,
                None,
                json!({
                    "problem": problem,
                    "proposal": proposal,
                }),
            )
        }
        FeedbackKind::Feedback => {
            let rating = req.rating.ok_or_else(|| {
                ServerError::with_user_message(
                    ErrorCode::InvalidParams,
                    "Escolha uma nota de 1 a 5 estrelas.",
                    "Missing feedback rating",
                )
            })?;
            if !(1..=5).contains(&rating) {
                return Err(ServerError::with_user_message(
                    ErrorCode::InvalidParams,
                    "A nota deve estar entre 1 e 5.",
                    "Out-of-range feedback rating",
                ));
            }
            let message = validate_structured_text(
                "Comentário",
                req.message.as_deref().unwrap_or(""),
                25,
                1_500,
                4,
                8,
            )?;
            (None, None, Some(rating), json!({ "message": message }))
        }
    };

    // Bound technical context JSON size and ensure it is an object.
    let context_json = match req.context {
        Some(v) if v.is_object() => {
            let serialized = serde_json::to_vec(&v)?;
            if serialized.len() > MAX_CONTEXT_JSON_BYTES {
                return Err(ServerError::with_user_message(
                    ErrorCode::InvalidParams,
                    "Metadados técnicos excedem o tamanho permitido.",
                    "Feedback context_json too large",
                ));
            }
            v
        }
        Some(_) => {
            return Err(ServerError::with_user_message(
                ErrorCode::InvalidParams,
                "Formato de metadados técnicos inválido.",
                "Feedback context must be JSON object",
            ));
        }
        None => json!({}),
    };

    // Re-encode bug screenshots safely from raw pixels in a blocking pool thread.
    let raw_attachments = req.attachments;
    let sanitized_attachments = if raw_attachments.is_empty() {
        Vec::new()
    } else {
        tokio::task::spawn_blocking(move || {
            raw_attachments
                .iter()
                .map(|url| sanitize_feedback_screenshot(url))
                .collect::<Result<Vec<_>, ServerError>>()
        })
        .await
        .map_err(|_| ServerError::internal("Feedback screenshot sanitize task failed"))??
    };

    let content_hash = compute_content_hash(account_id, kind, &title, &body_json);
    let created = state
        .feedback()
        .create(NewFeedbackSubmission {
            account_id,
            kind,
            area,
            severity,
            frequency,
            rating,
            title,
            body_json,
            context_json,
            content_hash,
            attachments: sanitized_attachments,
        })
        .await?;

    Ok((
        StatusCode::CREATED,
        Json(ApiSuccess {
            success: true,
            data: created,
            next_poll_hint_secs: None,
        }),
    )
        .into_response())
}

/// GET /api/v1/feedback/mine — last 10 submissions of the authenticated student with status & dev note.
pub async fn get_my_feedback(
    State(state): State<AppState>,
    DualAuth(principal): DualAuth,
) -> Result<Json<ApiSuccess<Vec<crate::db::feedback::FeedbackMineItem>>>, ServerError> {
    let account_id = require_user(&principal)?;
    let items = state.feedback().list_mine(account_id, 10).await?;
    Ok(Json(ApiSuccess {
        success: true,
        data: items,
        next_poll_hint_secs: None,
    }))
}

#[derive(Debug, Deserialize)]
pub struct AdminListQuery {
    pub status: Option<String>,
    pub kind: Option<String>,
    pub area: Option<String>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

/// GET /api/v1/admin/feedback — service-key triage list (paginated, zero TOAST blobs).
pub async fn admin_list_feedback(
    State(state): State<AppState>,
    DualAuth(principal): DualAuth,
    Query(query): Query<AdminListQuery>,
) -> Result<Json<ApiSuccess<crate::db::feedback::AdminFeedbackList>>, ServerError> {
    require_service_scope(&principal, SCOPE_FEEDBACK_READ)?;

    let status = match query.status.as_deref() {
        Some(raw) if !raw.trim().is_empty() => Some(FeedbackStatus::parse(raw).ok_or_else(|| {
            ServerError::invalid_params("Invalid status filter (open|triaged|resolved|dismissed)")
        })?),
        _ => None,
    };

    let kind = match query.kind.as_deref() {
        Some(raw) if !raw.trim().is_empty() => Some(FeedbackKind::parse(raw).ok_or_else(|| {
            ServerError::invalid_params("Invalid kind filter (bug|suggestion|feedback)")
        })?),
        _ => None,
    };

    let area = query
        .area
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned);

    let list = state
        .feedback()
        .list_admin(AdminFeedbackFilter {
            status,
            kind,
            area,
            limit: query.limit.unwrap_or(50),
            offset: query.offset.unwrap_or(0),
        })
        .await?;

    Ok(Json(ApiSuccess {
        success: true,
        data: list,
        next_poll_hint_secs: None,
    }))
}

/// GET /api/v1/admin/feedback/{id} — single submission detail with author and attachment metadata.
pub async fn admin_get_feedback(
    State(state): State<AppState>,
    DualAuth(principal): DualAuth,
    Path(id): Path<i64>,
) -> Result<Json<ApiSuccess<crate::db::feedback::AdminFeedbackItem>>, ServerError> {
    require_service_scope(&principal, SCOPE_FEEDBACK_READ)?;
    let item = state
        .feedback()
        .get_admin_item(id)
        .await?
        .ok_or_else(|| ServerError::not_found("Feedback submission not found"))?;
    Ok(Json(ApiSuccess {
        success: true,
        data: item,
        next_poll_hint_secs: None,
    }))
}

/// GET /api/v1/admin/feedback/{id}/attachments/{att_id} — streams raw re-encoded JPEG bytes.
pub async fn admin_get_attachment(
    State(state): State<AppState>,
    DualAuth(principal): DualAuth,
    Path((id, att_id)): Path<(i64, i64)>,
) -> Result<Response, ServerError> {
    require_service_scope(&principal, SCOPE_FEEDBACK_READ)?;
    let (mime_type, bytes) = state
        .feedback()
        .get_attachment(id, att_id)
        .await?
        .ok_or_else(|| ServerError::not_found("Feedback attachment not found"))?;

    let content_type = HeaderValue::from_str(&mime_type)
        .unwrap_or_else(|_| HeaderValue::from_static("image/jpeg"));

    Ok((
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, content_type),
            (
                header::X_CONTENT_TYPE_OPTIONS,
                HeaderValue::from_static("nosniff"),
            ),
            (
                header::CACHE_CONTROL,
                HeaderValue::from_static("private, max-age=3600"),
            ),
        ],
        bytes,
    )
        .into_response())
}

#[derive(Debug, Deserialize)]
pub struct AdminPatchFeedbackRequest {
    pub status: Option<String>,
    /// Use `""` or `null` (when key is present) to clear the dev note.
    #[serde(default, deserialize_with = "deserialize_optional_nullable")]
    pub dev_note: Option<Option<String>>,
}

fn deserialize_optional_nullable<'de, D>(
    deserializer: D,
) -> Result<Option<Option<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Some(Option::<String>::deserialize(deserializer)?))
}

/// PATCH /api/v1/admin/feedback/{id} — update triage status and/or developer note.
pub async fn admin_patch_feedback(
    State(state): State<AppState>,
    DualAuth(principal): DualAuth,
    Path(id): Path<i64>,
    AppJson(req): AppJson<AdminPatchFeedbackRequest>,
) -> Result<Json<ApiSuccess<crate::db::feedback::AdminFeedbackItem>>, ServerError> {
    require_service_scope(&principal, SCOPE_FEEDBACK_WRITE)?;

    let status = match req.status.as_deref() {
        Some(raw) => Some(FeedbackStatus::parse(raw).ok_or_else(|| {
            ServerError::invalid_params("Invalid status (open|triaged|resolved|dismissed)")
        })?),
        None => None,
    };

    let dev_note = match req.dev_note {
        None => None,
        Some(None) => Some(None),
        Some(Some(raw)) => {
            let cleaned = clean_multiline(&raw);
            if cleaned.is_empty() {
                Some(None)
            } else if cleaned.chars().count() > MAX_DEV_NOTE_CHARS {
                return Err(ServerError::with_user_message(
                    ErrorCode::InvalidParams,
                    format!("dev_note excede o limite de {MAX_DEV_NOTE_CHARS} caracteres."),
                    "dev_note too long",
                ));
            } else {
                Some(Some(cleaned))
            }
        }
    };

    if status.is_none() && dev_note.is_none() {
        return Err(ServerError::invalid_params(
            "Provide at least 'status' or 'dev_note' to update",
        ));
    }

    let updated = state
        .feedback()
        .update_admin(id, status, dev_note)
        .await?
        .ok_or_else(|| ServerError::not_found("Feedback submission not found"))?;

    Ok(Json(ApiSuccess {
        success: true,
        data: updated,
        next_poll_hint_secs: None,
    }))
}
