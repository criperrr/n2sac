pub mod account_routes;
pub mod auth_routes;
pub mod compare_routes;
pub mod connection_routes;
pub mod dev_announcement_routes;
pub mod envelope;
pub mod extract;
pub mod feedback_routes;
pub mod health;
pub mod nsac_routes;
pub mod profile_routes;
pub mod classroom_routes;
pub mod social_routes;
pub mod telemetry_routes;

use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::handler::Handler;
use axum::routing::{delete, get, patch, post, put};
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;

use crate::state::AppState;

const TELEMETRY_MAX_BODY_BYTES: usize = 16 * 1024;
const FEEDBACK_MAX_BODY_BYTES: usize = 2_600_000;
const ROOM_MEDIA_MAX_BODY_BYTES: usize = 2_200_000;

pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/api/v1/health", get(health::health))
        .route(
            "/api/v1/telemetry",
            post(telemetry_routes::ingest_telemetry)
                .layer(DefaultBodyLimit::max(TELEMETRY_MAX_BODY_BYTES)),
        )
        .route(
            "/api/v1/feedback",
            post(feedback_routes::post_feedback)
                .layer(DefaultBodyLimit::max(FEEDBACK_MAX_BODY_BYTES)),
        )
        .route("/api/v1/feedback/mine", get(feedback_routes::get_my_feedback))
        .route(
            "/api/v1/admin/feedback",
            get(feedback_routes::admin_list_feedback),
        )
        .route(
            "/api/v1/admin/feedback/{id}",
            get(feedback_routes::admin_get_feedback).patch(feedback_routes::admin_patch_feedback),
        )
        .route(
            "/api/v1/admin/feedback/{id}/attachments/{att_id}",
            get(feedback_routes::admin_get_attachment),
        )
        .route("/api/v1/auth/login", post(auth_routes::login))
        .route("/api/v1/auth/logout", post(auth_routes::logout))
        .route("/api/v1/account", get(account_routes::get_account))
        .route(
            "/api/v1/account/profile",
            get(profile_routes::get_profile).put(profile_routes::put_profile),
        )
        .route(
            "/api/v1/account/verify-google-name",
            post(profile_routes::verify_google_name),
        )
        .route(
            "/api/v1/account/verify-google-name/oauth/start",
            post(profile_routes::start_google_name_oauth),
        )
        .route(
            "/api/v1/account/verify-google-name/oauth/callback",
            get(profile_routes::verify_google_name_oauth_callback),
        )
        .route(
            "/api/v1/account/connections",
            get(connection_routes::list_my_connections),
        )
        .route(
            "/api/v1/account/connections/{provider}",
            put(connection_routes::put_my_connection)
                .delete(connection_routes::delete_my_connection),
        )
        .route(
            "/api/v1/account/connections/oauth/{provider}/start",
            post(connection_routes::start_oauth_link),
        )
        .route(
            "/api/v1/connections/oauth/{provider}/callback",
            get(connection_routes::oauth_callback),
        )
        .route(
            "/api/v1/account/fcm-token",
            put(account_routes::put_fcm_token).delete(account_routes::delete_fcm_token),
        )
        .route(
            "/api/v1/account/notification-preferences",
            get(account_routes::get_notification_preferences)
                .put(account_routes::put_notification_preferences),
        )
        .route("/api/v1/classrooms/me", get(classroom_routes::get_my_room))
        .route(
            "/api/v1/dev-announcements",
            get(dev_announcement_routes::list_dev_announcements).post(
                dev_announcement_routes::create_dev_announcement
                    .layer(DefaultBodyLimit::max(6_000_000)),
            ),
        )
        .route(
            "/api/v1/dev-announcements/{id}",
            delete(dev_announcement_routes::delete_dev_announcement),
        )
        .route(
            "/api/v1/dev-announcements/{id}/read",
            post(dev_announcement_routes::mark_dev_announcement_read),
        )
        .route(
            "/api/v1/dev-announcements/{id}/reaction",
            post(dev_announcement_routes::toggle_dev_announcement_reaction),
        )
        .route(
            "/api/v1/dev-announcements/{id}/poll/vote",
            post(dev_announcement_routes::vote_dev_announcement_poll),
        )
        .route(
            "/api/v1/dev-announcements/{id}/comments",
            get(dev_announcement_routes::list_dev_announcement_comments)
                .post(dev_announcement_routes::create_dev_announcement_comment),
        )
        .route(
            "/api/v1/dev-announcements/comments/{comment_id}",
            delete(dev_announcement_routes::delete_dev_announcement_comment),
        )
        .route("/api/v1/classrooms/me/stream", get(classroom_routes::stream_my_room))
        .route("/api/v1/classrooms/me/election", get(classroom_routes::get_election))
        .route(
            "/api/v1/classrooms/me/representatives/votes/{candidate_id}",
            post(classroom_routes::post_representative_vote)
                .delete(classroom_routes::delete_representative_vote),
        )
        .route(
            "/api/v1/classrooms/me/appearance",
            put(classroom_routes::put_classroom_appearance)
                .layer(DefaultBodyLimit::max(ROOM_MEDIA_MAX_BODY_BYTES)),
        )
        .route("/api/v1/classrooms/me/avatar", get(classroom_routes::get_classroom_avatar))
        .route("/api/v1/classrooms/me/banner", get(classroom_routes::get_classroom_banner))
        .route(
            "/api/v1/classrooms/me/announcements",
            get(classroom_routes::get_announcements).post(
                classroom_routes::post_announcement
                    .layer(DefaultBodyLimit::max(ROOM_MEDIA_MAX_BODY_BYTES)),
            ),
        )
        .route(
            "/api/v1/classrooms/me/announcements/{id}",
            patch(classroom_routes::patch_announcement).delete(classroom_routes::delete_announcement),
        )
        .route(
            "/api/v1/classrooms/me/announcements/{id}/attachment",
            get(classroom_routes::get_announcement_attachment),
        )
        .route(
            "/api/v1/classrooms/me/announcements/{id}/read",
            post(classroom_routes::post_announcement_read),
        )
        .route(
            "/api/v1/classrooms/me/announcements/{id}/reaction",
            put(classroom_routes::put_announcement_reaction),
        )
        .route(
            "/api/v1/classrooms/me/channels/{kind}/messages",
            get(classroom_routes::get_channel_messages).post(
                classroom_routes::post_channel_message
                    .layer(DefaultBodyLimit::max(ROOM_MEDIA_MAX_BODY_BYTES)),
            ),
        )
        .route(
            "/api/v1/classrooms/me/channels/{kind}/messages/{id}",
            patch(classroom_routes::patch_channel_message)
                .delete(classroom_routes::delete_channel_message),
        )
        .route(
            "/api/v1/classrooms/me/channels/{kind}/messages/{id}/reaction",
            put(classroom_routes::put_channel_message_reaction),
        )
        .route(
            "/api/v1/classrooms/me/channels/{kind}/messages/{id}/attachment",
            get(classroom_routes::get_channel_message_attachment),
        )
        .route(
            "/api/v1/classrooms/me/channels/{kind}/typing",
            post(classroom_routes::post_channel_typing),
        )
        .route(
            "/api/v1/account/discord-link",
            put(account_routes::put_discord_link),
        )
        .route(
            "/api/v1/account/ranking-preferences",
            get(compare_routes::get_ranking_preferences)
                .put(compare_routes::put_ranking_preferences),
        )
        .route("/api/v1/compare", get(compare_routes::get_compare))
        .route(
            "/api/v1/compare/custom-analytics",
            post(compare_routes::post_custom_analytics),
        )
        .route(
            "/api/v1/compare/peer/{peer_id}",
            get(compare_routes::get_peer_comparison),
        )
        .route(
            "/api/v1/compare/classrooms",
            get(compare_routes::get_classroom_ranking),
        )
        .route(
            "/api/v1/compare/classrooms/{class_code}",
            get(compare_routes::get_classroom_comparison),
        )
        .route("/api/v1/students/search", get(profile_routes::search_students))
        .route("/api/v1/account/privacy", put(profile_routes::put_privacy))
        .route("/api/v1/profiles/{account_id}", get(profile_routes::get_public_profile))
        .route("/api/v1/profiles/{account_id}/avatar", get(profile_routes::get_avatar))
        .route("/api/v1/profiles/{account_id}/banner", get(profile_routes::get_banner))
        .route("/api/v1/profiles/{account_id}/report", post(profile_routes::post_report))
        .route("/api/v1/social/overview", get(social_routes::get_overview))
        .route(
            "/api/v1/social/relationship/{account_id}",
            get(social_routes::get_relationship),
        )
        .route(
            "/api/v1/social/follows/{account_id}",
            post(social_routes::post_follow).delete(social_routes::delete_follow),
        )
        .route(
            "/api/v1/social/requests/{requester_id}/accept",
            post(social_routes::accept_follow_request),
        )
        .route(
            "/api/v1/social/requests/{requester_id}/decline",
            post(social_routes::decline_follow_request),
        )
        .route(
            "/api/v1/social/blocks/{account_id}",
            post(social_routes::post_block).delete(social_routes::delete_block),
        )
        .route(
            "/api/v1/social/conversations/{peer_id}/messages",
            get(social_routes::get_conversation_messages)
                .post(social_routes::post_conversation_message),
        )
        .route(
            "/api/v1/social/conversations/{peer_id}/messages/{message_id}",
            patch(social_routes::patch_conversation_message)
                .delete(social_routes::delete_conversation_message),
        )
        .route(
            "/api/v1/social/conversations/{peer_id}/messages/{message_id}/reaction",
            put(social_routes::put_message_reaction),
        )
        .route(
            "/api/v1/social/conversations/{peer_id}/typing",
            post(social_routes::post_typing),
        )
        .route(
            "/api/v1/social/conversations/{peer_id}",
            delete(social_routes::delete_conversation),
        )
        .route("/api/v1/nsac/home", get(nsac_routes::get_home))
        .route("/api/v1/nsac/boletim", get(nsac_routes::get_boletim))
        .route("/api/v1/nsac/boletim/pdf", get(nsac_routes::get_boletim_pdf))
        .route(
            "/api/v1/nsac/boletim/pdf-link",
            post(nsac_routes::create_boletim_pdf_link),
        )
        .route(
            "/api/v1/nsac/boletim/pdf-download/{token}",
            get(nsac_routes::download_boletim_pdf_by_token),
        )
        .route("/api/v1/nsac/atrasos", get(nsac_routes::get_atrasos))
        .route("/api/v1/nsac/saidas", get(nsac_routes::get_saidas))
        .route(
            "/api/v1/nsac/ficha-avaliacao",
            get(nsac_routes::get_ficha_list),
        )
        .route(
            "/api/v1/nsac/ficha-avaliacao/{id}",
            get(nsac_routes::get_ficha_detail),
        )
        .route(
            "/api/v1/nsac/ficha-avaliacao/{id}/pdf-link",
            post(nsac_routes::create_ficha_pdf_link),
        )
        .route(
            "/api/v1/nsac/ficha-avaliacao/pdf/{token}",
            get(nsac_routes::get_ficha_pdf),
        )
        .with_state(state)
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
}
