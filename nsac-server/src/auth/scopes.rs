pub const SCOPE_NSAC_READ: &str = "nsac:read";
pub const SCOPE_NSAC_WRITE: &str = "nsac:write";
pub const SCOPE_ACCOUNT_LINK: &str = "account:link";

pub fn has_scope(scopes: &[String], required: &str) -> bool {
    scopes.iter().any(|s| s == required)
}
