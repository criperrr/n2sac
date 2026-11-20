pub mod extractor;
pub mod scopes;

pub use extractor::{
    DualAuth, Principal, ServicePrincipal, TargetAccount, UserPrincipal, resolve_target_account,
};
pub use scopes::{SCOPE_ACCOUNT_LINK, SCOPE_NSAC_READ, SCOPE_NSAC_WRITE, has_scope};

use crate::error::ServerError;
use crate::state::AppState;

pub async fn seed_service_clients(state: &AppState) -> Result<usize, ServerError> {
    state
        .auth()
        .seed_service_clients(&state.config.service_api_keys)
        .await
}
