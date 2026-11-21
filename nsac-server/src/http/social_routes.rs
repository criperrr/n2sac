use axum::extract::{Path, RawQuery, State};
use axum::response::Response;
use serde::Deserialize;

use crate::auth::{DualAuth, Principal};
use crate::db::social::FollowEdgeStatus;
use crate::error::ServerError;
use crate::fcm::FcmDeliveryOutcome;
use crate::http::envelope::ok_json;
use crate::http::extract::{AppJson, parse_query_pairs};
use crate::state::AppState;

fn user_account_id(principal: &Principal) -> Result<i64, ServerError> {
    match principal {
        Principal::User(u) => Ok(u.account_id),
        Principal::Service(_) => Err(ServerError::unauthorized("User session required")),
    }
}

fn format_message_preview(raw: &str) -> String {
    let collapsed = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        return "Enviou uma nova mensagem no n²sac.".to_string();
    }
    let mut chars = collapsed.chars();
    let preview: String = chars.by_ref().take(90).collect();
    if chars.next().is_some() {
        format!("{preview}…")
    } else {
        preview
    }
}

fn spawn_social_push(
    state: AppState,
    recipient_id: i64,
    actor_id: i64,
    actor_label: String,
    title: String,
    body: String,
    kind: &'static str,
) {
    if !state.fcm.is_enabled() {
        return;
    }
    tokio::spawn(async move {
        let Ok(Some(token)) = state.social().account_fcm_token(recipient_id).await else {
            return;
        };
        match state
            .fcm
            .send_social_notification(&token, &title, &body, kind, actor_id, &actor_label)
            .await
        {
            Ok(FcmDeliveryOutcome::Delivered { .. }) => {}
            Ok(FcmDeliveryOutcome::Unregistered) => {
                tracing::info!(
                    recipient_id = recipient_id,
                    kind = kind,
                    "Clearing unregistered FCM token after social push 404"
                );
                let _ = state
                    .accounts()
                    .update_fcm_token(recipient_id, None, state.clock.now())
                    .await;
            }
            Ok(FcmDeliveryOutcome::TransientFailure { status, detail }) => {
                tracing::warn!(
                    recipient_id = recipient_id,
                    kind = kind,
                    status = status,
                    detail = %detail,
                    "Social FCM push transient failure"
                );
            }
            Ok(FcmDeliveryOutcome::Disabled) => {}
            Err(err) => {
                tracing::warn!(
                    recipient_id = recipient_id,
                    kind = kind,
                    error = %err,
                    "Social FCM push failed"
                );
            }
        }
    });
}

/// GET /api/v1/social/overview
pub async fn get_overview(
    State(state): State<AppState>,
    DualAuth(principal): DualAuth,
) -> Result<Response, ServerError> {
    let viewer = user_account_id(&principal)?;
    let overview = state.social().overview(viewer, &state.crypto).await?;
    Ok(ok_json(overview))
}

/// GET /api/v1/social/relationship/{account_id}
pub async fn get_relationship(
    State(state): State<AppState>,
    DualAuth(principal): DualAuth,
    Path(account_id): Path<i64>,
) -> Result<Response, ServerError> {
    let viewer = user_account_id(&principal)?;
    let rel = state.social().relationship(viewer, account_id).await?;
    Ok(ok_json(rel))
}

/// POST /api/v1/social/follows/{account_id}
pub async fn post_follow(
    State(state): State<AppState>,
    DualAuth(principal): DualAuth,
    Path(account_id): Path<i64>,
) -> Result<Response, ServerError> {
    let viewer = user_account_id(&principal)?;
    let result = state.social().follow(viewer, account_id).await?;

    let actor_name = state
        .social()
        .account_label(viewer)
        .await
        .unwrap_or_else(|_| "Um colega".to_string());

    if result.became_friends {
        spawn_social_push(
            state.clone(),
            account_id,
            viewer,
            actor_name.clone(),
            "Amizade conquistada!".to_string(),
            format!("{actor_name} também seguiu você — agora vocês são amigos no n²sac!"),
            "friendship",
        );
    } else if result.status == FollowEdgeStatus::Pending {
        spawn_social_push(
            state.clone(),
            account_id,
            viewer,
            actor_name.clone(),
            "Novo pedido para seguir".to_string(),
            format!("{actor_name} quer seguir você no n²sac. Toque para aprovar."),
            "follow_request",
        );
    } else {
        spawn_social_push(
            state.clone(),
            account_id,
            viewer,
            actor_name.clone(),
            "Novo seguidor no n²sac".to_string(),
            format!("{actor_name} começou a seguir você!"),
            "follow",
        );
    }

    Ok(ok_json(result))
}

/// DELETE /api/v1/social/follows/{account_id}
pub async fn delete_follow(
    State(state): State<AppState>,
    DualAuth(principal): DualAuth,
    Path(account_id): Path<i64>,
) -> Result<Response, ServerError> {
    let viewer = user_account_id(&principal)?;
    let relationship = state.social().unfollow(viewer, account_id).await?;
    Ok(ok_json(relationship))
}

/// POST /api/v1/social/requests/{requester_id}/accept
pub async fn accept_follow_request(
    State(state): State<AppState>,
    DualAuth(principal): DualAuth,
    Path(requester_id): Path<i64>,
) -> Result<Response, ServerError> {
    let viewer = user_account_id(&principal)?;
    let result = state.social().accept_request(viewer, requester_id).await?;

    let actor_name = state
        .social()
        .account_label(viewer)
        .await
        .unwrap_or_else(|_| "Um colega".to_string());

    if result.became_friends {
        spawn_social_push(
            state.clone(),
            requester_id,
            viewer,
            actor_name.clone(),
            "Amizade conquistada!".to_string(),
            format!("{actor_name} aceitou seu pedido — agora vocês são amigos no n²sac!"),
            "friendship",
        );
    } else {
        spawn_social_push(
            state.clone(),
            requester_id,
            viewer,
            actor_name.clone(),
            "Pedido aceito".to_string(),
            format!("{actor_name} aceitou sua solicitação para seguir."),
            "follow_accepted",
        );
    }

    Ok(ok_json(result))
}

/// POST /api/v1/social/requests/{requester_id}/decline
pub async fn decline_follow_request(
    State(state): State<AppState>,
    DualAuth(principal): DualAuth,
    Path(requester_id): Path<i64>,
) -> Result<Response, ServerError> {
    let viewer = user_account_id(&principal)?;
    let relationship = state.social().decline_request(viewer, requester_id).await?;
    Ok(ok_json(relationship))
}

/// POST /api/v1/social/blocks/{account_id}
pub async fn post_block(
    State(state): State<AppState>,
    DualAuth(principal): DualAuth,
    Path(account_id): Path<i64>,
) -> Result<Response, ServerError> {
    let viewer = user_account_id(&principal)?;
    let relationship = state.social().block(viewer, account_id).await?;
    Ok(ok_json(relationship))
}

/// DELETE /api/v1/social/blocks/{account_id}
pub async fn delete_block(
    State(state): State<AppState>,
    DualAuth(principal): DualAuth,
    Path(account_id): Path<i64>,
) -> Result<Response, ServerError> {
    let viewer = user_account_id(&principal)?;
    let relationship = state.social().unblock(viewer, account_id).await?;
    Ok(ok_json(relationship))
}

/// GET /api/v1/social/conversations/{peer_id}/messages?after_id=&limit=
pub async fn get_conversation_messages(
    State(state): State<AppState>,
    DualAuth(principal): DualAuth,
    Path(peer_id): Path<i64>,
    RawQuery(raw_query): RawQuery,
) -> Result<Response, ServerError> {
    let viewer = user_account_id(&principal)?;
    let pairs = parse_query_pairs(raw_query.as_deref());
    let after_id = pairs
        .iter()
        .find(|(k, _)| k == "after_id")
        .and_then(|(_, v)| v.trim().parse::<i64>().ok());
    let limit = pairs
        .iter()
        .find(|(k, _)| k == "limit")
        .and_then(|(_, v)| v.trim().parse::<i64>().ok())
        .unwrap_or(60);

    let payload = state
        .social()
        .get_messages(viewer, peer_id, after_id, limit, &state.crypto)
        .await?;
    Ok(ok_json(payload))
}

#[derive(Deserialize)]
pub struct SendMessageRequest {
    pub text: String,
}

/// POST /api/v1/social/conversations/{peer_id}/messages
pub async fn post_conversation_message(
    State(state): State<AppState>,
    DualAuth(principal): DualAuth,
    Path(peer_id): Path<i64>,
    AppJson(body): AppJson<SendMessageRequest>,
) -> Result<Response, ServerError> {
    let viewer = user_account_id(&principal)?;
    let payload = state
        .social()
        .send_message(viewer, peer_id, &body.text, &state.crypto)
        .await?;

    let actor_name = state
        .social()
        .account_label(viewer)
        .await
        .unwrap_or_else(|_| "Um amigo".to_string());
    let preview = format_message_preview(&body.text);
    spawn_social_push(
        state.clone(),
        peer_id,
        viewer,
        actor_name.clone(),
        actor_name,
        preview,
        "chat_message",
    );

    Ok(ok_json(payload))
}

/// DELETE /api/v1/social/conversations/{peer_id}
pub async fn delete_conversation(
    State(state): State<AppState>,
    DualAuth(principal): DualAuth,
    Path(peer_id): Path<i64>,
) -> Result<Response, ServerError> {
    let viewer = user_account_id(&principal)?;
    state.social().clear_conversation(viewer, peer_id).await?;
    Ok(ok_json(serde_json::json!({ "cleared": true })))
}
