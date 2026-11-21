#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

//! NSAC Unified Production HTTP Server (M1 persistence, cache, config, and ports).

pub mod auth;
pub mod config;
pub mod db;
pub mod error;
pub mod fcm;
pub mod http;
pub mod ports;
pub mod sentinel;
pub mod state;

use sqlx::PgPool;

use crate::error::ServerError;

pub async fn run_migrations(pool: &PgPool) -> Result<(), ServerError> {
    sqlx::migrate!("../migrations").run(pool).await?;
    Ok(())
}

pub fn core_crate_available() -> bool {
    let _ = core::any::type_name::<nsac_core::error::AppError>();
    true
}
