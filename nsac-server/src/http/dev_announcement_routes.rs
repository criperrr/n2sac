//! Global developer announcements endpoints.
//! Read-only for all authenticated users; write/delete strictly restricted to `platform_role = 'dev'`.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use nsac_core::error::ErrorCode;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::auth::{DualAuth, Principal};
use crate::db::{DevAnnouncementRepo, DevAnnouncementRow};
use crate::error::ServerError;
use crate::http::envelope::ok_json;
use crate::state::AppState;

fn require_user_account_id(principal: &Principal) -> Result<i64, ServerError> {
    match principal {
        Principal::User(u) => Ok(u.account_id),
        Principal::Service(_) => Err(ServerError::unauthorized("User session required")),
    }
}

#[derive(Debug, Deserialize)]
pub struct ListDevAnnouncementsQuery {
    #[serde(default = "default_limit")]
    pub limit: i64,
}

fn default_limit() -> i64 {
    50
}

#[derive(Debug, Serialize)]
pub struct DevAnnouncementsResponse {
    pub items: Vec<DevAnnouncementRow>,
    pub unread_count: i64,
    pub can_publish: bool,
}

#[derive(Debug, Deserialize)]
pub struct CreateDevAnnouncementRequest {
    pub title: String,
    pub body: String,
    #[serde(default = "default_tag")]
    pub tag: String,
    #[serde(default)]
    pub pinned: bool,
    #[serde(default = "default_send_push")]
    pub send_push: bool,
    #[serde(default)]
    pub image_data_url: Option<String>,
    #[serde(default)]
    pub poll_options: Vec<String>,
}

fn default_tag() -> String {
    "novidade".to_string()
}

fn default_send_push() -> bool {
    true
}

/// GET /api/v1/dev-announcements
pub async fn list_dev_announcements(
    State(state): State<AppState>,
    DualAuth(principal): DualAuth,
    Query(query): Query<ListDevAnnouncementsQuery>,
) -> Result<impl IntoResponse, ServerError> {
    let account_id = require_user_account_id(&principal)?;
    let repo = DevAnnouncementRepo::new(state.db.clone());

    let (items, unread_count, role) = tokio::try_join!(
        repo.list_for_account(account_id, query.limit),
        repo.count_unread(account_id),
        repo.get_platform_role(account_id),
    )?;

    Ok(ok_json(DevAnnouncementsResponse {
        items,
        unread_count,
        can_publish: role == "dev",
    }))
}

/// POST /api/v1/dev-announcements
pub async fn create_dev_announcement(
    State(state): State<AppState>,
    DualAuth(principal): DualAuth,
    Json(req): Json<CreateDevAnnouncementRequest>,
) -> Result<impl IntoResponse, ServerError> {
    let account_id = require_user_account_id(&principal)?;
    let repo = DevAnnouncementRepo::new(state.db.clone());

    let role = repo.get_platform_role(account_id).await?;
    if role != "dev" {
        return Err(ServerError::forbidden(
            "Apenas desenvolvedores podem publicar anúncios de desenvolvimento.",
        ));
    }

    let title = req.title.trim();
    let body = req.body.trim();
    let tag = req.tag.trim();
    if title.is_empty() || title.len() > 140 {
        return Err(ServerError::with_user_message(
            ErrorCode::InvalidParams,
            "Título deve ter entre 1 e 140 caracteres.",
            "invalid dev announcement title",
        ));
    }
    if body.is_empty() || body.len() > 4000 {
        return Err(ServerError::with_user_message(
            ErrorCode::InvalidParams,
            "Conteúdo deve ter entre 1 e 4000 caracteres.",
            "invalid dev announcement body",
        ));
    }
    let clean_tag = if tag.is_empty() { "novidade" } else { tag };
    if clean_tag.len() > 32 {
        return Err(ServerError::with_user_message(
            ErrorCode::InvalidParams,
            "Tag deve ter no máximo 32 caracteres.",
            "invalid dev announcement tag",
        ));
    }

    let poll_options: Vec<String> = req
        .poll_options
        .into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if !poll_options.is_empty() && (poll_options.len() < 2 || poll_options.len() > 6) {
        return Err(ServerError::with_user_message(
            ErrorCode::InvalidParams,
            "A enquete deve ter entre 2 e 6 opções.",
            "invalid poll options count",
        ));
    }

    let clean_image = req
        .image_data_url
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    if let Some(img) = clean_image
        && (!img.starts_with("data:image/") || img.len() > 5_000_000)
    {
        return Err(ServerError::with_user_message(
            ErrorCode::InvalidParams,
            "Imagem inválida ou muito grande (máximo ~3.5 MB).",
            "invalid dev announcement image",
        ));
    }

    let created = repo
        .create(
            account_id,
            title,
            body,
            clean_tag,
            req.pinned,
            clean_image,
            &poll_options,
        )
        .await?;

    info!(
        account_id,
        announcement_id = created.id,
        "Published developer announcement"
    );

    if req.send_push {
        let fcm = state.fcm.clone();
        let repo_bg = repo.clone();
        let ann_id = created.id;
        let author = created.author_name.clone();
        let category = created.tag.clone();
        let push_title = created.title.clone();
        let preview: String = created.body.chars().take(160).collect();

        tokio::spawn(async move {
            match repo_bg.list_push_tokens_for_dev_announcements(account_id).await {
                Ok(tokens) => {
                    for (_target_account_id, token) in tokens {
                        if let Err(err) = fcm
                            .send_announcement_notification(
                                &token,
                                "n²sac Dev",
                                ann_id,
                                &author,
                                &category,
                                &push_title,
                                &preview,
                            )
                            .await
                        {
                            warn!(?err, "Failed to send dev announcement FCM push");
                        }
                    }
                }
                Err(err) => {
                    warn!(?err, "Failed to list push tokens for dev announcement");
                }
            }
        });
    }

    Ok((StatusCode::CREATED, ok_json(created)))
}

/// DELETE /api/v1/dev-announcements/{id}
pub async fn delete_dev_announcement(
    State(state): State<AppState>,
    DualAuth(principal): DualAuth,
    Path(id): Path<i64>,
) -> Result<impl IntoResponse, ServerError> {
    let account_id = require_user_account_id(&principal)?;
    let repo = DevAnnouncementRepo::new(state.db.clone());

    let role = repo.get_platform_role(account_id).await?;
    if role != "dev" {
        return Err(ServerError::forbidden(
            "Apenas desenvolvedores podem remover anúncios de desenvolvimento.",
        ));
    }

    let deleted = repo.soft_delete(id).await?;
    if !deleted {
        return Err(ServerError::not_found("Anúncio não encontrado."));
    }

    Ok(ok_json(serde_json::json!({ "deleted": true, "id": id })))
}

/// POST /api/v1/dev-announcements/{id}/read
pub async fn mark_dev_announcement_read(
    State(state): State<AppState>,
    DualAuth(principal): DualAuth,
    Path(id): Path<i64>,
) -> Result<impl IntoResponse, ServerError> {
    let account_id = require_user_account_id(&principal)?;
    let repo = DevAnnouncementRepo::new(state.db.clone());
    repo.mark_read(id, account_id).await?;
    Ok(ok_json(serde_json::json!({ "read": true, "id": id })))
}

#[derive(Debug, Deserialize)]
pub struct ToggleReactionRequest {
    pub emoji: String,
}

/// POST /api/v1/dev-announcements/{id}/reaction
pub async fn toggle_dev_announcement_reaction(
    State(state): State<AppState>,
    DualAuth(principal): DualAuth,
    Path(id): Path<i64>,
    Json(req): Json<ToggleReactionRequest>,
) -> Result<impl IntoResponse, ServerError> {
    let account_id = require_user_account_id(&principal)?;
    let emoji = req.emoji.trim();
    if emoji.is_empty() || emoji.chars().count() > 8 {
        return Err(ServerError::with_user_message(
            ErrorCode::InvalidParams,
            "Emoji inválido.",
            "invalid emoji",
        ));
    }
    let repo = DevAnnouncementRepo::new(state.db.clone());
    let reactions = repo.toggle_reaction(id, account_id, emoji).await?;
    Ok(ok_json(serde_json::json!({ "reactions": reactions })))
}

#[derive(Debug, Deserialize)]
pub struct VotePollRequest {
    pub option_id: i64,
}

/// POST /api/v1/dev-announcements/{id}/poll/vote
pub async fn vote_dev_announcement_poll(
    State(state): State<AppState>,
    DualAuth(principal): DualAuth,
    Path(id): Path<i64>,
    Json(req): Json<VotePollRequest>,
) -> Result<impl IntoResponse, ServerError> {
    let account_id = require_user_account_id(&principal)?;
    let repo = DevAnnouncementRepo::new(state.db.clone());
    let poll_options = repo.vote_poll(id, req.option_id, account_id).await?;
    Ok(ok_json(serde_json::json!({ "poll_options": poll_options })))
}

/// GET /api/v1/dev-announcements/{id}/comments
pub async fn list_dev_announcement_comments(
    State(state): State<AppState>,
    DualAuth(principal): DualAuth,
    Path(id): Path<i64>,
) -> Result<impl IntoResponse, ServerError> {
    let account_id = require_user_account_id(&principal)?;
    let repo = DevAnnouncementRepo::new(state.db.clone());
    let comments = repo.list_comments(id, account_id).await?;
    Ok(ok_json(serde_json::json!({ "comments": comments })))
}

#[derive(Debug, Deserialize)]
pub struct CreateCommentRequest {
    pub body: String,
}

/// POST /api/v1/dev-announcements/{id}/comments
pub async fn create_dev_announcement_comment(
    State(state): State<AppState>,
    DualAuth(principal): DualAuth,
    Path(id): Path<i64>,
    Json(req): Json<CreateCommentRequest>,
) -> Result<impl IntoResponse, ServerError> {
    let account_id = require_user_account_id(&principal)?;
    let body = req.body.trim();
    if body.is_empty() || body.len() > 1000 {
        return Err(ServerError::with_user_message(
            ErrorCode::InvalidParams,
            "Comentário deve ter entre 1 e 1000 caracteres.",
            "invalid comment body",
        ));
    }
    let repo = DevAnnouncementRepo::new(state.db.clone());
    let comment = repo.create_comment(id, account_id, body).await?;
    Ok((StatusCode::CREATED, ok_json(comment)))
}

/// DELETE /api/v1/dev-announcements/comments/{comment_id}
pub async fn delete_dev_announcement_comment(
    State(state): State<AppState>,
    DualAuth(principal): DualAuth,
    Path(comment_id): Path<i64>,
) -> Result<impl IntoResponse, ServerError> {
    let account_id = require_user_account_id(&principal)?;
    let repo = DevAnnouncementRepo::new(state.db.clone());
    let role = repo.get_platform_role(account_id).await?;
    let deleted = repo
        .delete_comment(comment_id, account_id, role == "dev")
        .await?;
    if !deleted {
        return Err(ServerError::not_found("Comentário não encontrado."));
    }
    Ok(ok_json(serde_json::json!({ "deleted": true, "id": comment_id })))
}
