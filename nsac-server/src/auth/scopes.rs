pub const SCOPE_NSAC_READ: &str = "nsac:read";
pub const SCOPE_NSAC_WRITE: &str = "nsac:write";
pub const SCOPE_ACCOUNT_LINK: &str = "account:link";
pub const SCOPE_FEEDBACK_READ: &str = "feedback:read";
pub const SCOPE_FEEDBACK_WRITE: &str = "feedback:write";

pub fn has_scope(scopes: &[String], required: &str) -> bool {
    scopes.iter().any(|s| s == required)
}
