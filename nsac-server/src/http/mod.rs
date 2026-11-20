pub mod account_routes;
pub mod auth_routes;
pub mod compare_routes;
pub mod envelope;
pub mod extract;
pub mod health;
pub mod nsac_routes;
pub mod profile_routes;
pub mod telemetry_routes;

use axum::Router;
use axum::routing::{get, post, put};
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;

use crate::state::AppState;

pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/api/v1/health", get(health::health))
        .route("/api/v1/telemetry", post(telemetry_routes::ingest_telemetry))
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
        .with_state(state)
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
}
