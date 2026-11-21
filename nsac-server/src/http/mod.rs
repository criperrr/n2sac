pub mod account_routes;
pub mod auth_routes;
pub mod compare_routes;
pub mod envelope;
pub mod extract;
pub mod health;
pub mod nsac_routes;
pub mod profile_routes;
pub mod social_routes;
pub mod telemetry_routes;

use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::routing::{delete, get, post, put};
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;

use crate::state::AppState;

const TELEMETRY_MAX_BODY_BYTES: usize = 16 * 1024;

pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/api/v1/health", get(health::health))
        .route(
            "/api/v1/telemetry",
            post(telemetry_routes::ingest_telemetry)
                .layer(DefaultBodyLimit::max(TELEMETRY_MAX_BODY_BYTES)),
        )
        .route("/api/v1/auth/login", post(auth_routes::login))
        .route("/api/v1/auth/logout", post(auth_routes::logout))
        .route("/api/v1/account", get(account_routes::get_account))
        .route(
            "/api/v1/account/profile",
            get(profile_routes::get_profile).put(profile_routes::put_profile),
        )
        .route(
            "/api/v1/account/fcm-token",
            put(account_routes::put_fcm_token),
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
        .route("/api/v1/students/search", get(profile_routes::search_students))
        .route("/api/v1/account/privacy", put(profile_routes::put_privacy))
        .route("/api/v1/profiles/{account_id}", get(profile_routes::get_public_profile))
        .route("/api/v1/profiles/{account_id}/avatar", get(profile_routes::get_avatar))
        .route("/api/v1/profiles/{account_id}/banner", get(profile_routes::get_banner))
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
            "/api/v1/social/conversations/{peer_id}",
            delete(social_routes::delete_conversation),
        )
        .route("/api/v1/nsac/home", get(nsac_routes::get_home))
        .route("/api/v1/nsac/boletim", get(nsac_routes::get_boletim))
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
