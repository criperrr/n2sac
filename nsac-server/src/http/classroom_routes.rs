//! HTTP & SSE routes for Class Rooms (Announcements, Class Chat, Off-Topic, Representative Election).

use std::convert::Infallible;
use std::time::Duration;

use axum::body::Body;
use axum::extract::{Path, RawQuery, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use chrono::NaiveDate;
use futures_util::stream::{self, StreamExt};
use nsac_core::error::ErrorCode;
use serde::Deserialize;

use crate::auth::{DualAuth, Principal};
use crate::db::classrooms::{
    ClassroomChannelKind, ClassroomMessageQuery, normalize_announcement_tag,
};
use crate::error::ServerError;
use crate::fcm::FcmDeliveryOutcome;
use crate::http::envelope::ok_json;
use crate::http::extract::{AppJson, parse_query_pairs};
use crate::media::{ZippedSanitizedImage, sanitize_and_zip_channel_image};
use crate::realtime::ClassroomStreamEvent;
use crate::state::AppState;

fn user_account_id(principal: &Principal) -> Result<i64, ServerError> {
    match principal {
        Principal::User(u) => Ok(u.account_id),
        Principal::Service(_) => Err(ServerError::unauthorized("User session required")),
    }
}

fn query_i64(pairs: &[(String, String)], key: &str) -> Option<i64> {
    pairs
        .iter()
        .find(|(k, _)| k == key)
        .and_then(|(_, v)| v.trim().parse::<i64>().ok())
}

fn query_str<'a>(pairs: &'a [(String, String)], key: &str) -> Option<&'a str> {
    pairs
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.trim())
        .filter(|v| !v.is_empty())
}

fn parse_channel_kind(raw: &str) -> Result<ClassroomChannelKind, ServerError> {
    ClassroomChannelKind::parse(raw).ok_or_else(|| {
        ServerError::with_user_message(
            ErrorCode::InvalidParams,
            "Canal inválido. Use 'chat' ou 'off_topic'.",
            "Invalid room channel kind",
        )
    })
}

fn jpeg_response(bytes: Vec<u8>) -> Response {
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, HeaderValue::from_static("image/jpeg"))
        .header(
            header::CACHE_CONTROL,
            HeaderValue::from_static("private, max-age=86400"),
        )
        .body(Body::from(bytes))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

fn format_preview(raw: &str, has_attachment: bool) -> String {
    if let Some(label) = crate::db::social::gif_message_label(raw) {
        return label.to_string();
    }
    let collapsed = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        return if has_attachment {
            "📷 Enviou uma imagem".to_string()
        } else {
            "Nova mensagem no canal da turma.".to_string()
        };
    }
    let mut chars = collapsed.chars();
    let preview: String = chars.by_ref().take(100).collect();
    if chars.next().is_some() {
        format!("{preview}…")
    } else {
        preview
    }
}

/// GET /api/v1/classrooms/me
pub async fn get_my_room(
    State(state): State<AppState>,
    DualAuth(principal): DualAuth,
) -> Result<Response, ServerError> {
    let viewer = user_account_id(&principal)?;
    let overview = state.classrooms().get_classroom_overview(viewer, &state.crypto).await?;
    state.realtime.record_rev(
        overview.room.id,
        "announcements",
        overview.next_announcements_rev,
    );
    Ok(ok_json(overview))
}

/// GET /api/v1/classrooms/me/stream (SSE realtime updates for the user's room)
pub async fn stream_my_room(
    State(state): State<AppState>,
    DualAuth(principal): DualAuth,
) -> Result<Response, ServerError> {
    let viewer = user_account_id(&principal)?;
    let room = state.classrooms().resolve_user_classroom(viewer).await?;
    let rx = state.realtime.subscribe(room.id);
    let event_stream = async_stream_from_broadcast(rx);
    let sse = Sse::new(event_stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(20))
            .text("ping"),
    );

    let mut resp = sse.into_response();
    resp.headers_mut().insert(
        header::HeaderName::from_static("x-accel-buffering"),
        HeaderValue::from_static("no"),
    );
    resp.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-cache, no-transform"),
    );
    Ok(resp)
}

fn async_stream_from_broadcast(
    rx: tokio::sync::broadcast::Receiver<ClassroomStreamEvent>,
) -> impl futures_util::Stream<Item = Result<Event, Infallible>> {
    stream::unfold(rx, |mut rx| async move {
        match rx.recv().await {
            Ok(ev) => {
                let data = serde_json::to_string(&ev).unwrap_or_else(|_| "{}".to_string());
                Some((Ok(Event::default().event("room_event").data(data)), rx))
            }
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                let resync = serde_json::json!({ "kind": "resync" }).to_string();
                Some((Ok(Event::default().event("room_event").data(resync)), rx))
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => None,
        }
    })
}

/// GET /api/v1/classrooms/me/election
pub async fn get_election(
    State(state): State<AppState>,
    DualAuth(principal): DualAuth,
) -> Result<Response, ServerError> {
    let viewer = user_account_id(&principal)?;
    let payload = state.classrooms().get_election_state(viewer).await?;
    Ok(ok_json(payload))
}

/// POST /api/v1/classrooms/me/representatives/votes/{candidate_id}
pub async fn post_representative_vote(
    State(state): State<AppState>,
    DualAuth(principal): DualAuth,
    Path(candidate_id): Path<i64>,
) -> Result<Response, ServerError> {
    let viewer = user_account_id(&principal)?;
    let payload = state.classrooms().cast_vote(viewer, candidate_id).await?;
    state.realtime.publish(
        payload.room.id,
        ClassroomStreamEvent::RepresentativesChanged {
            room_id: payload.room.id,
        },
    );
    Ok(ok_json(payload))
}

/// DELETE /api/v1/classrooms/me/representatives/votes/{candidate_id}
pub async fn delete_representative_vote(
    State(state): State<AppState>,
    DualAuth(principal): DualAuth,
    Path(candidate_id): Path<i64>,
) -> Result<Response, ServerError> {
    let viewer = user_account_id(&principal)?;
    let payload = state.classrooms().revoke_vote(viewer, candidate_id).await?;
    state.realtime.publish(
        payload.room.id,
        ClassroomStreamEvent::RepresentativesChanged {
            room_id: payload.room.id,
        },
    );
    Ok(ok_json(payload))
}

#[derive(Deserialize)]
pub struct UpdateRoomAppearanceRequest {
    #[serde(default)]
    pub avatar_data_url: Option<String>,
    #[serde(default)]
    pub banner_data_url: Option<String>,
    #[serde(default)]
    pub clear_avatar: bool,
    #[serde(default)]
    pub clear_banner: bool,
}

/// PUT /api/v1/classrooms/me/appearance (Representatives only; stored zipped in PostgreSQL)
pub async fn put_classroom_appearance(
    State(state): State<AppState>,
    DualAuth(principal): DualAuth,
    AppJson(body): AppJson<UpdateRoomAppearanceRequest>,
) -> Result<Response, ServerError> {
    let viewer = user_account_id(&principal)?;
    let room = state.classrooms().resolve_user_classroom(viewer).await?;
    if !state.classrooms().is_representative(room.id, viewer).await? {
        return Err(ServerError::with_user_message(
            ErrorCode::Forbidden,
            "Apenas representantes de classe podem alterar a foto e o banner da turma.",
            "Non-representative tried to edit room appearance",
        ));
    }

    let avatar_update: Option<Option<Vec<u8>>> = if body.clear_avatar {
        Some(None)
    } else if let Some(raw) = body.avatar_data_url {
        let zipped = tokio::task::spawn_blocking(move || {
            sanitize_and_zip_channel_image(&raw, "Foto do canal", 320, 320, true, 80)
        })
        .await
        .map_err(|_| ServerError::internal("Image processing task panicked"))??;
        Some(Some(zipped.zip_bytes))
    } else {
        None
    };

    let banner_update: Option<Option<Vec<u8>>> = if body.clear_banner {
        Some(None)
    } else if let Some(raw) = body.banner_data_url {
        let zipped = tokio::task::spawn_blocking(move || {
            sanitize_and_zip_channel_image(&raw, "Banner do canal", 1280, 640, false, 74)
        })
        .await
        .map_err(|_| ServerError::internal("Image processing task panicked"))??;
        Some(Some(zipped.zip_bytes))
    } else {
        None
    };

    let info = state
        .classrooms()
        .update_classroom_appearance(viewer, avatar_update, banner_update)
        .await?;
    state.realtime.publish(
        info.id,
        ClassroomStreamEvent::AppearanceChanged { room_id: info.id },
    );
    Ok(ok_json(info))
}

/// GET /api/v1/classrooms/me/avatar
pub async fn get_classroom_avatar(
    State(state): State<AppState>,
    DualAuth(principal): DualAuth,
) -> Result<Response, ServerError> {
    let viewer = user_account_id(&principal)?;
    let bytes = state.classrooms().get_classroom_media_jpeg(viewer, false).await?;
    Ok(jpeg_response(bytes))
}

/// GET /api/v1/classrooms/me/banner
pub async fn get_classroom_banner(
    State(state): State<AppState>,
    DualAuth(principal): DualAuth,
) -> Result<Response, ServerError> {
    let viewer = user_account_id(&principal)?;
    let bytes = state.classrooms().get_classroom_media_jpeg(viewer, true).await?;
    Ok(jpeg_response(bytes))
}

/// GET /api/v1/classrooms/me/announcements
pub async fn get_announcements(
    State(state): State<AppState>,
    DualAuth(principal): DualAuth,
    RawQuery(raw_query): RawQuery,
) -> Result<Response, ServerError> {
    let viewer = user_account_id(&principal)?;
    let pairs = parse_query_pairs(raw_query.as_deref());
    let category = match query_str(&pairs, "tag").or_else(|| query_str(&pairs, "category")) {
        Some(raw) if !raw.trim().is_empty() => Some(normalize_announcement_tag(raw)?),
        _ => None,
    };
    let before_id = query_i64(&pairs, "before_id");
    let after_rev = query_i64(&pairs, "after_rev");
    let limit = query_i64(&pairs, "limit").unwrap_or(30);

    let (items, next_rev) = state
        .classrooms()
        .list_announcements(
            viewer,
            category.as_deref(),
            before_id,
            after_rev,
            limit,
            &state.crypto,
        )
        .await?;
    Ok(ok_json(serde_json::json!({
        "announcements": items,
        "next_rev": next_rev,
    })))
}

#[derive(Deserialize)]
pub struct CreateAnnouncementRequest {
    #[serde(default, alias = "tag")]
    pub category: Option<String>,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub event_date: Option<String>,
    #[serde(default)]
    pub pinned: bool,
    #[serde(default = "default_true")]
    pub notify_push: bool,
    #[serde(default)]
    pub image_data_url: Option<String>,
}

fn default_true() -> bool {
    true
}

/// POST /api/v1/classrooms/me/announcements (Representatives only)
pub async fn post_announcement(
    State(state): State<AppState>,
    DualAuth(principal): DualAuth,
    AppJson(body): AppJson<CreateAnnouncementRequest>,
) -> Result<Response, ServerError> {
    let viewer = user_account_id(&principal)?;
    let category = match body.category.as_deref() {
        Some(raw) => normalize_announcement_tag(raw)?,
        None => "Geral".to_string(),
    };

    let event_date = match body.event_date.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(raw) => Some(NaiveDate::parse_from_str(raw, "%Y-%m-%d").map_err(|_| {
            ServerError::with_user_message(
                ErrorCode::InvalidParams,
                "Data inválida. Use o formato AAAA-MM-DD.",
                "Invalid event_date",
            )
        })?),
        None => None,
    };

    let attachment: Option<ZippedSanitizedImage> = if let Some(raw_img) = body.image_data_url {
        let zipped = tokio::task::spawn_blocking(move || {
            sanitize_and_zip_channel_image(&raw_img, "Imagem do anúncio", 1280, 1280, false, 78)
        })
        .await
        .map_err(|_| ServerError::internal("Image processing task panicked"))??;
        Some(zipped)
    } else {
        None
    };

    let discord_jpeg = attachment.as_ref().map(|a| a.raw_jpeg_bytes.clone());

    let (room, item) = state
        .classrooms()
        .create_announcement(
            viewer,
            &category,
            &body.title,
            &body.body,
            event_date,
            body.pinned,
            body.notify_push,
            attachment,
            &state.crypto,
        )
        .await?;

    state
        .realtime
        .bump_and_notify(room.id, "announcements", item.rev);

    // Spawn high-priority FCM push fan-out to classmates (if notify_push = true).
    if item.notify_push && state.fcm.is_enabled() {
        let st = state.clone();
        let class_code = room.class_code.clone();
        let ann_id = item.id;
        let author_label = item.author_label.clone();
        let cat = item.category.clone();
        let title = item.title.clone();
        let preview = format_preview(&item.body, item.attachment_url.is_some());
        tokio::spawn(async move {
            let Ok(targets) = st
                .classrooms()
                .list_announcement_push_targets(&class_code, viewer)
                .await
            else {
                return;
            };
            stream::iter(targets)
                .for_each_concurrent(8, |(target_id, token)| {
                    let st = st.clone();
                    let class_code = class_code.clone();
                    let author_label = author_label.clone();
                    let cat = cat.clone();
                    let title = title.clone();
                    let preview = preview.clone();
                    async move {
                        if let Ok(FcmDeliveryOutcome::Unregistered) = st
                            .fcm
                            .send_announcement_notification(
                                &token,
                                &class_code,
                                ann_id,
                                &author_label,
                                &cat,
                                &title,
                                &preview,
                            )
                            .await
                        {
                            let _ = st
                                .accounts()
                                .update_fcm_token(target_id, None, st.clock.now())
                                .await;
                        }
                    }
                })
                .await;
        });
    }

    // Optional opt-in mirroring to Discord channels configured with mirror_announcements = TRUE.
    {
        let db = state.db.clone();
        let class_code = room.class_code.clone();
        let title = item.title.clone();
        let body_text = item.body.clone();
        let author_label = item.author_label.clone();
        let cat = item.category.clone();
        let ev_date = item.event_date.clone();
        tokio::spawn(async move {
            crate::discord::broadcast_discord_class_announcement(
                &db,
                &class_code,
                &author_label,
                &cat,
                &title,
                &body_text,
                ev_date.as_deref(),
                discord_jpeg,
            )
            .await;
        });
    }

    Ok(ok_json(item))
}

#[derive(Deserialize)]
pub struct UpdateAnnouncementRequest {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub pinned: Option<bool>,
}

/// PATCH /api/v1/classrooms/me/announcements/{id}
pub async fn patch_announcement(
    State(state): State<AppState>,
    DualAuth(principal): DualAuth,
    Path(id): Path<i64>,
    AppJson(body): AppJson<UpdateAnnouncementRequest>,
) -> Result<Response, ServerError> {
    let viewer = user_account_id(&principal)?;
    let (room, updated) = state
        .classrooms()
        .update_announcement(
            viewer,
            id,
            body.title.as_deref(),
            body.body.as_deref(),
            body.pinned,
            &state.crypto,
        )
        .await?;
    state
        .realtime
        .bump_and_notify(room.id, "announcements", updated.rev);
    Ok(ok_json(updated))
}

/// DELETE /api/v1/classrooms/me/announcements/{id}
pub async fn delete_announcement(
    State(state): State<AppState>,
    DualAuth(principal): DualAuth,
    Path(id): Path<i64>,
) -> Result<Response, ServerError> {
    let viewer = user_account_id(&principal)?;
    let (room, rev) = state.classrooms().delete_announcement(viewer, id).await?;
    state.realtime.bump_and_notify(room.id, "announcements", rev);
    Ok(ok_json(serde_json::json!({ "deleted": true, "id": id })))
}

/// GET /api/v1/classrooms/me/announcements/{id}/attachment
pub async fn get_announcement_attachment(
    State(state): State<AppState>,
    DualAuth(principal): DualAuth,
    Path(id): Path<i64>,
) -> Result<Response, ServerError> {
    let viewer = user_account_id(&principal)?;
    let bytes = state
        .classrooms()
        .get_announcement_attachment_jpeg(viewer, id)
        .await?;
    Ok(jpeg_response(bytes))
}

/// POST /api/v1/classrooms/me/announcements/{id}/read
pub async fn post_announcement_read(
    State(state): State<AppState>,
    DualAuth(principal): DualAuth,
    Path(id): Path<i64>,
) -> Result<Response, ServerError> {
    let viewer = user_account_id(&principal)?;
    let read_count = state.classrooms().mark_announcement_read(viewer, id).await?;
    Ok(ok_json(serde_json::json!({ "read_count": read_count })))
}

#[derive(Deserialize)]
pub struct ReactionBody {
    pub emoji: Option<String>,
}

/// PUT /api/v1/classrooms/me/announcements/{id}/reaction
pub async fn put_announcement_reaction(
    State(state): State<AppState>,
    DualAuth(principal): DualAuth,
    Path(id): Path<i64>,
    AppJson(body): AppJson<ReactionBody>,
) -> Result<Response, ServerError> {
    let viewer = user_account_id(&principal)?;
    let (room, rev) = state
        .classrooms()
        .set_announcement_reaction(viewer, id, body.emoji.as_deref())
        .await?;
    state.realtime.bump_and_notify(room.id, "announcements", rev);
    Ok(ok_json(serde_json::json!({ "ok": true, "rev": rev })))
}

/// GET /api/v1/classrooms/me/channels/{kind}/messages
pub async fn get_channel_messages(
    State(state): State<AppState>,
    DualAuth(principal): DualAuth,
    Path(kind_raw): Path<String>,
    RawQuery(raw_query): RawQuery,
) -> Result<Response, ServerError> {
    let viewer = user_account_id(&principal)?;
    let kind = parse_channel_kind(&kind_raw)?;
    let pairs = parse_query_pairs(raw_query.as_deref());
    let query = ClassroomMessageQuery {
        after_rev: query_i64(&pairs, "after_rev"),
        before_id: query_i64(&pairs, "before_id"),
        limit: query_i64(&pairs, "limit").unwrap_or(60),
    };

    let mut payload = state
        .classrooms()
        .get_channel_messages(viewer, kind, query, &state.crypto)
        .await?;
    state
        .realtime
        .record_rev(payload.room.id, kind.as_str(), payload.next_rev);
    payload.typing_users = state
        .realtime
        .typing_users(payload.room.id, kind.as_str(), viewer)
        .await;
    Ok(ok_json(payload))
}

#[derive(Deserialize)]
pub struct SendRoomMessageRequest {
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub reply_to_id: Option<i64>,
    #[serde(default)]
    pub ref_announcement_id: Option<i64>,
    #[serde(default)]
    pub image_data_url: Option<String>,
}

/// POST /api/v1/classrooms/me/channels/{kind}/messages
pub async fn post_channel_message(
    State(state): State<AppState>,
    DualAuth(principal): DualAuth,
    Path(kind_raw): Path<String>,
    AppJson(body): AppJson<SendRoomMessageRequest>,
) -> Result<Response, ServerError> {
    let viewer = user_account_id(&principal)?;
    let kind = parse_channel_kind(&kind_raw)?;

    let attachment: Option<ZippedSanitizedImage> = if let Some(raw_img) = body.image_data_url {
        let zipped = tokio::task::spawn_blocking(move || {
            sanitize_and_zip_channel_image(&raw_img, "Imagem do canal", 1280, 1280, false, 78)
        })
        .await
        .map_err(|_| ServerError::internal("Image processing task panicked"))??;
        Some(zipped)
    } else {
        None
    };

    let (room, payload, reply_target_sender) = state
        .classrooms()
        .send_channel_message(
            viewer,
            kind,
            &body.text,
            body.reply_to_id,
            body.ref_announcement_id,
            attachment,
            &state.crypto,
        )
        .await?;

    state
        .realtime
        .clear_typing(room.id, kind.as_str(), viewer)
        .await;
    state
        .realtime
        .bump_and_notify(room.id, kind.as_str(), payload.message.rev);

    // Smart FCM push: only notify classmates whose preference matches ('all', or 'mentions' on direct reply).
    if state.fcm.is_enabled() {
        let st = state.clone();
        let class_code = room.class_code.clone();
        let chan_str = kind.as_str().to_string();
        let msg_id = payload.message.id;
        let sender_label = payload.message.sender_label.clone();
        let preview = format_preview(
            &payload.message.text,
            payload.message.attachment_url.is_some(),
        );
        tokio::spawn(async move {
            let Ok(targets) = st
                .classrooms()
                .list_room_chat_push_targets(&class_code, viewer)
                .await
            else {
                return;
            };
            let eligible: Vec<(i64, String)> = targets
                .into_iter()
                .filter(|t| {
                    let mode = if kind == ClassroomChannelKind::OffTopic {
                        t.room_offtopic_mode.as_str()
                    } else {
                        t.room_chat_mode.as_str()
                    };
                    match mode {
                        "all" => true,
                        "mentions" => reply_target_sender == Some(t.account_id),
                        _ => false,
                    }
                })
                .map(|t| (t.account_id, t.fcm_token))
                .collect();

            stream::iter(eligible)
                .for_each_concurrent(8, |(target_id, token)| {
                    let st = st.clone();
                    let class_code = class_code.clone();
                    let chan_str = chan_str.clone();
                    let sender_label = sender_label.clone();
                    let preview = preview.clone();
                    async move {
                        if let Ok(FcmDeliveryOutcome::Unregistered) = st
                            .fcm
                            .send_classroom_chat_notification(
                                &token,
                                &class_code,
                                &chan_str,
                                msg_id,
                                &sender_label,
                                &preview,
                            )
                            .await
                        {
                            let _ = st
                                .accounts()
                                .update_fcm_token(target_id, None, st.clock.now())
                                .await;
                        }
                    }
                })
                .await;
        });
    }

    Ok(ok_json(payload))
}

#[derive(Deserialize)]
pub struct EditRoomMessageRequest {
    pub text: String,
}

/// PATCH /api/v1/classrooms/me/channels/{kind}/messages/{id}
pub async fn patch_channel_message(
    State(state): State<AppState>,
    DualAuth(principal): DualAuth,
    Path((kind_raw, id)): Path<(String, i64)>,
    AppJson(body): AppJson<EditRoomMessageRequest>,
) -> Result<Response, ServerError> {
    let viewer = user_account_id(&principal)?;
    let kind = parse_channel_kind(&kind_raw)?;
    let (room, msg) = state
        .classrooms()
        .edit_channel_message(viewer, kind, id, &body.text, &state.crypto)
        .await?;
    state.realtime.bump_and_notify(room.id, kind.as_str(), msg.rev);
    Ok(ok_json(msg))
}

/// DELETE /api/v1/classrooms/me/channels/{kind}/messages/{id}
pub async fn delete_channel_message(
    State(state): State<AppState>,
    DualAuth(principal): DualAuth,
    Path((kind_raw, id)): Path<(String, i64)>,
) -> Result<Response, ServerError> {
    let viewer = user_account_id(&principal)?;
    let kind = parse_channel_kind(&kind_raw)?;
    let (room, msg) = state
        .classrooms()
        .delete_channel_message(viewer, kind, id, &state.crypto)
        .await?;
    state.realtime.bump_and_notify(room.id, kind.as_str(), msg.rev);
    Ok(ok_json(msg))
}

/// PUT /api/v1/classrooms/me/channels/{kind}/messages/{id}/reaction
pub async fn put_channel_message_reaction(
    State(state): State<AppState>,
    DualAuth(principal): DualAuth,
    Path((kind_raw, id)): Path<(String, i64)>,
    AppJson(body): AppJson<ReactionBody>,
) -> Result<Response, ServerError> {
    let viewer = user_account_id(&principal)?;
    let kind = parse_channel_kind(&kind_raw)?;
    let (room, msg) = state
        .classrooms()
        .set_channel_message_reaction(viewer, kind, id, body.emoji.as_deref(), &state.crypto)
        .await?;
    state.realtime.bump_and_notify(room.id, kind.as_str(), msg.rev);
    Ok(ok_json(msg))
}

/// GET /api/v1/classrooms/me/channels/{kind}/messages/{id}/attachment
pub async fn get_channel_message_attachment(
    State(state): State<AppState>,
    DualAuth(principal): DualAuth,
    Path((kind_raw, id)): Path<(String, i64)>,
) -> Result<Response, ServerError> {
    let viewer = user_account_id(&principal)?;
    let kind = parse_channel_kind(&kind_raw)?;
    let bytes = state
        .classrooms()
        .get_channel_message_attachment_jpeg(viewer, kind, id)
        .await?;
    Ok(jpeg_response(bytes))
}

/// POST /api/v1/classrooms/me/channels/{kind}/typing
pub async fn post_channel_typing(
    State(state): State<AppState>,
    DualAuth(principal): DualAuth,
    Path(kind_raw): Path<String>,
) -> Result<Response, ServerError> {
    let viewer = user_account_id(&principal)?;
    let kind = parse_channel_kind(&kind_raw)?;
    let room = state.classrooms().resolve_user_classroom(viewer).await?;
    let label = state
        .social()
        .account_label(viewer)
        .await
        .unwrap_or_else(|_| "Colega".to_string());
    state
        .realtime
        .set_typing(room.id, kind.as_str(), viewer, label)
        .await;
    Ok(ok_json(serde_json::json!({ "ok": true })))
}
