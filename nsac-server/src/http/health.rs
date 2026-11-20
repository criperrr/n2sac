use axum::extract::State;
use axum::response::Response;

use crate::error::ServerError;
use crate::http::envelope::ok_json;
use crate::state::AppState;

pub async fn health(State(state): State<AppState>) -> Result<Response, ServerError> {
    let _: i32 = sqlx::query_scalar("SELECT 1").fetch_one(&state.db).await?;
    Ok(ok_json(serde_json::json!({ "status": "ok" })))
}
