#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

use std::error::Error;

use nsac_server::auth::seed_service_clients;
use nsac_server::config::Config;
use nsac_server::discord::spawn_discord_bot;
use nsac_server::fcm::FcmClient;
use nsac_server::http::build_router;
use nsac_server::run_migrations;
use nsac_server::sentinel::spawn_sentinel_worker;
use nsac_server::state::AppState;
use sqlx::postgres::PgPoolOptions;
use tokio::net::TcpListener;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let config = Config::from_env()?;

    let env_filter = EnvFilter::try_new(&config.rust_log)
        .unwrap_or_else(|_| EnvFilter::new("info,nsac_server=info"));
    tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .with_target(true)
        .init();

    let pool = PgPoolOptions::new()
        .max_connections(16)
        .connect(config.database_url.as_str())
        .await?;

    run_migrations(&pool).await?;

    let bind_addr = config.http_bind;
    let fcm_client = FcmClient::new(config.fcm.clone())?;
    let state = AppState::with_default_ports(config, pool.clone())?;
    let seeded = seed_service_clients(&state).await?;
    let sentinel_handle = spawn_sentinel_worker(state.clone(), fcm_client.clone());
    let discord_handle = spawn_discord_bot(state.clone(), fcm_client);
    tracing::info!(
        bind_addr = %bind_addr,
        service_clients_seeded = seeded,
        discord_bot_active = discord_handle.is_some(),
        "Starting nsac-server HTTP API + Sentinel + Discord Bot"
    );

    let router = build_router(state);
    let listener = TcpListener::bind(bind_addr).await?;

    axum::serve(listener, router)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    sentinel_handle.abort();
    if let Some(h) = discord_handle {
        h.abort();
    }
    pool.close().await;
    tracing::info!("nsac-server shut down cleanly");
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(err) = tokio::signal::ctrl_c().await {
            tracing::warn!(error = %err, "Failed to install Ctrl+C signal handler");
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut sigterm) => {
                let _ = sigterm.recv().await;
            }
            Err(err) => {
                tracing::warn!(error = %err, "Failed to install SIGTERM signal handler");
                std::future::pending::<()>().await;
            }
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {
            tracing::info!("Received SIGINT, initiating graceful shutdown");
        }
        () = terminate => {
            tracing::info!("Received SIGTERM, initiating graceful shutdown");
        }
    }
}
