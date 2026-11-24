pub mod oauth;
pub mod providers;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub use providers::{
    ConnectionProvider, NormalizedConnection, ProviderDescriptor, all_provider_descriptors,
    normalize_manual_connection,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationSource {
    Manual,
    Oauth,
    DiscordImport,
}

impl VerificationSource {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Oauth => "oauth",
            Self::DiscordImport => "discord_import",
        }
    }

    pub fn parse(raw: &str) -> Self {
        match raw {
            "oauth" => Self::Oauth,
            "discord_import" => Self::DiscordImport,
            _ => Self::Manual,
        }
    }
}

/// Full connection record returned to the owning user.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountConnectionDto {
    pub provider: String,
    pub handle: String,
    pub display_name: Option<String>,
    pub profile_url: Option<String>,
    pub verification_source: VerificationSource,
    pub is_verified: bool,
    pub is_visible: bool,
    pub display_order: i32,
    pub updated_at: DateTime<Utc>,
}

/// Public connection record shown on another student's profile.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublicConnectionDto {
    pub provider: String,
    pub handle: String,
    pub display_name: Option<String>,
    pub profile_url: Option<String>,
    pub verification_source: VerificationSource,
    pub is_verified: bool,
}
