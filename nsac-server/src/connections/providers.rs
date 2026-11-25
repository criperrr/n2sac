use serde::Serialize;

use crate::error::ServerError;

pub const MAX_CONNECTIONS_PER_ACCOUNT: usize = 12;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ConnectionProvider {
    Discord,
    Spotify,
    Github,
    Steam,
    Twitch,
    Instagram,
    Tiktok,
    Youtube,
    X,
    Roblox,
    Linkedin,
    Letterboxd,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProviderDescriptor {
    pub id: &'static str,
    pub label: &'static str,
    pub placeholder: &'static str,
    pub supports_oauth: bool,
    pub oauth_enabled: bool,
    pub supports_discord_import: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedConnection {
    pub provider: ConnectionProvider,
    pub handle: String,
    pub display_name: Option<String>,
    pub profile_url: Option<String>,
}

impl ConnectionProvider {
    pub const ALL: [Self; 2] = [
        Self::Spotify,
        Self::Discord,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Discord => "discord",
            Self::Spotify => "spotify",
            Self::Github => "github",
            Self::Steam => "steam",
            Self::Twitch => "twitch",
            Self::Instagram => "instagram",
            Self::Tiktok => "tiktok",
            Self::Youtube => "youtube",
            Self::X => "x",
            Self::Roblox => "roblox",
            Self::Linkedin => "linkedin",
            Self::Letterboxd => "letterboxd",
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Discord => "Discord",
            Self::Spotify => "Spotify",
            Self::Github => "GitHub",
            Self::Steam => "Steam",
            Self::Twitch => "Twitch",
            Self::Instagram => "Instagram",
            Self::Tiktok => "TikTok",
            Self::Youtube => "YouTube",
            Self::X => "X (Twitter)",
            Self::Roblox => "Roblox",
            Self::Linkedin => "LinkedIn",
            Self::Letterboxd => "Letterboxd",
        }
    }

    pub const fn placeholder(self) -> &'static str {
        match self {
            Self::Discord => "@seu_usuario",
            Self::Spotify => "open.spotify.com/user/... ou @usuario",
            Self::Github => "usuario ou github.com/usuario",
            Self::Steam => "steamcommunity.com/id/... ou ID",
            Self::Twitch => "usuario ou twitch.tv/usuario",
            Self::Instagram => "@usuario ou instagram.com/usuario",
            Self::Tiktok => "@usuario ou tiktok.com/@usuario",
            Self::Youtube => "@canal ou youtube.com/@canal",
            Self::X => "@usuario ou x.com/usuario",
            Self::Roblox => "usuario ou roblox.com/users/...",
            Self::Linkedin => "linkedin.com/in/usuario",
            Self::Letterboxd => "usuario ou letterboxd.com/usuario",
        }
    }

    pub const fn supports_oauth(self) -> bool {
        matches!(self, Self::Discord | Self::Spotify)
    }

    pub const fn supports_discord_import(self) -> bool {
        matches!(
            self,
            Self::Spotify
                | Self::Github
                | Self::Steam
                | Self::Twitch
                | Self::Instagram
                | Self::Tiktok
                | Self::Youtube
                | Self::X
                | Self::Roblox
        )
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "discord" => Some(Self::Discord),
            "spotify" => Some(Self::Spotify),
            "github" => Some(Self::Github),
            "steam" => Some(Self::Steam),
            "twitch" => Some(Self::Twitch),
            "instagram" => Some(Self::Instagram),
            "tiktok" => Some(Self::Tiktok),
            "youtube" => Some(Self::Youtube),
            "x" | "twitter" => Some(Self::X),
            "roblox" => Some(Self::Roblox),
            "linkedin" => Some(Self::Linkedin),
            "letterboxd" => Some(Self::Letterboxd),
            _ => None,
        }
    }

    /// Map a Discord `/users/@me/connections` `type` string to our provider enum.
    pub fn from_discord_type(discord_type: &str) -> Option<Self> {
        match discord_type.trim().to_ascii_lowercase().as_str() {
            "spotify" => Some(Self::Spotify),
            "github" => Some(Self::Github),
            "steam" => Some(Self::Steam),
            "twitch" => Some(Self::Twitch),
            "instagram" => Some(Self::Instagram),
            "tiktok" => Some(Self::Tiktok),
            "youtube" => Some(Self::Youtube),
            "twitter" | "x" => Some(Self::X),
            "roblox" => Some(Self::Roblox),
            _ => None,
        }
    }

    pub fn build_profile_url(self, external_id: Option<&str>, handle: &str) -> Option<String> {
        let clean_handle = handle.trim().trim_start_matches('@');
        if clean_handle.is_empty() {
            return None;
        }
        match self {
            Self::Discord => None,
            Self::Spotify => {
                let id = external_id.unwrap_or(clean_handle).trim();
                if id.is_empty() {
                    None
                } else {
                    Some(format!("https://open.spotify.com/user/{id}"))
                }
            }
            Self::Github => Some(format!("https://github.com/{clean_handle}")),
            Self::Steam => {
                if let Some(id) = external_id.map(str::trim).filter(|s| s.chars().all(|c| c.is_ascii_digit()) && s.len() >= 15) {
                    Some(format!("https://steamcommunity.com/profiles/{id}"))
                } else {
                    Some(format!("https://steamcommunity.com/id/{clean_handle}"))
                }
            }
            Self::Twitch => Some(format!("https://www.twitch.tv/{clean_handle}")),
            Self::Instagram => Some(format!("https://www.instagram.com/{clean_handle}/")),
            Self::Tiktok => Some(format!("https://www.tiktok.com/@{clean_handle}")),
            Self::Youtube => {
                if let Some(id) = external_id.map(str::trim).filter(|s| s.starts_with("UC") && s.len() >= 10) {
                    Some(format!("https://www.youtube.com/channel/{id}"))
                } else {
                    Some(format!("https://www.youtube.com/@{clean_handle}"))
                }
            }
            Self::X => Some(format!("https://x.com/{clean_handle}")),
            Self::Roblox => {
                if let Some(id) = external_id.map(str::trim).filter(|s| s.chars().all(|c| c.is_ascii_digit()) && !s.is_empty()) {
                    Some(format!("https://www.roblox.com/users/{id}/profile"))
                } else {
                    Some(format!("https://www.roblox.com/search/users?keyword={clean_handle}"))
                }
            }
            Self::Linkedin => Some(format!("https://www.linkedin.com/in/{clean_handle}")),
            Self::Letterboxd => Some(format!("https://letterboxd.com/{clean_handle}/")),
        }
    }
}

pub fn all_provider_descriptors(
    oauth_enabled_fn: impl Fn(ConnectionProvider) -> bool,
) -> Vec<ProviderDescriptor> {
    ConnectionProvider::ALL
        .iter()
        .map(|&p| ProviderDescriptor {
            id: p.as_str(),
            label: p.label(),
            placeholder: p.placeholder(),
            supports_oauth: p.supports_oauth(),
            oauth_enabled: p.supports_oauth() && oauth_enabled_fn(p),
            supports_discord_import: p.supports_discord_import(),
        })
        .collect()
}

fn is_safe_handle_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.')
}

fn strip_url_prefix<'a>(raw: &'a str, prefixes: &[&str]) -> Option<&'a str> {
    let lower = raw.to_ascii_lowercase();
    for prefix in prefixes {
        if lower.starts_with(prefix) {
            return Some(&raw[prefix.len()..]);
        }
    }
    None
}

fn first_path_segment(rest: &str) -> &str {
    rest.split(['?', '#', '/'])
        .next()
        .unwrap_or("")
        .trim()
}

/// Strictly validates and normalizes a user-supplied handle or profile URL for `provider`.
/// Rejects arbitrary URLs, `javascript:` schemes, spaces, and control characters.
pub fn normalize_manual_connection(
    provider: ConnectionProvider,
    raw_input: &str,
    raw_display_name: Option<&str>,
) -> Result<NormalizedConnection, ServerError> {
    let trimmed = raw_input.trim();
    if trimmed.is_empty() || trimmed.len() > 200 {
        return Err(ServerError::invalid_params(
            "Informe um usuário ou link válido (até 200 caracteres).",
        ));
    }
    if trimmed.chars().any(|c| c.is_control()) {
        return Err(ServerError::invalid_params("Caracteres inválidos no link ou usuário."));
    }
    let lower = trimmed.to_ascii_lowercase();
    if lower.starts_with("javascript:") || lower.starts_with("data:") || lower.starts_with("vbscript:") {
        return Err(ServerError::invalid_params("Esquema de URL não permitido."));
    }

    let cleaned_display = raw_display_name
        .map(|s| s.trim().chars().take(48).collect::<String>())
        .filter(|s| !s.is_empty());

    let (handle, profile_url) = match provider {
        ConnectionProvider::Discord => {
            let h = trimmed.trim_start_matches('@');
            validate_handle(h, 2, 32, "Discord")?;
            (format!("@{h}"), None)
        }
        ConnectionProvider::Spotify => {
            if let Some(rest) = strip_url_prefix(
                trimmed,
                &[
                    "https://open.spotify.com/user/",
                    "http://open.spotify.com/user/",
                    "open.spotify.com/user/",
                ],
            ) {
                let seg = first_path_segment(rest);
                validate_handle(seg, 1, 64, "Spotify")?;
                (seg.to_string(), Some(format!("https://open.spotify.com/user/{seg}")))
            } else {
                reject_unknown_url(trimmed, "Spotify")?;
                let h = trimmed.trim_start_matches('@');
                validate_handle(h, 1, 64, "Spotify")?;
                (h.to_string(), Some(format!("https://open.spotify.com/user/{h}")))
            }
        }
        ConnectionProvider::Github => {
            let seg = strip_url_prefix(
                trimmed,
                &["https://github.com/", "http://github.com/", "https://www.github.com/", "github.com/"],
            )
            .map(first_path_segment)
            .unwrap_or_else(|| trimmed.trim_start_matches('@'));
            if seg == trimmed {
                reject_unknown_url(trimmed, "GitHub")?;
            }
            validate_handle(seg, 1, 39, "GitHub")?;
            (seg.to_string(), Some(format!("https://github.com/{seg}")))
        }
        ConnectionProvider::Steam => {
            if let Some(rest) = strip_url_prefix(
                trimmed,
                &[
                    "https://steamcommunity.com/profiles/",
                    "http://steamcommunity.com/profiles/",
                    "steamcommunity.com/profiles/",
                ],
            ) {
                let seg = first_path_segment(rest);
                if !seg.chars().all(|c| c.is_ascii_digit()) || seg.len() < 15 || seg.len() > 22 {
                    return Err(ServerError::invalid_params("SteamID64 inválido."));
                }
                (seg.to_string(), Some(format!("https://steamcommunity.com/profiles/{seg}")))
            } else if let Some(rest) = strip_url_prefix(
                trimmed,
                &[
                    "https://steamcommunity.com/id/",
                    "http://steamcommunity.com/id/",
                    "steamcommunity.com/id/",
                ],
            ) {
                let seg = first_path_segment(rest);
                validate_handle(seg, 2, 32, "Steam")?;
                (seg.to_string(), Some(format!("https://steamcommunity.com/id/{seg}")))
            } else {
                reject_unknown_url(trimmed, "Steam")?;
                let h = trimmed.trim_start_matches('@');
                validate_handle(h, 2, 32, "Steam")?;
                if h.chars().all(|c| c.is_ascii_digit()) && h.len() >= 15 {
                    (h.to_string(), Some(format!("https://steamcommunity.com/profiles/{h}")))
                } else {
                    (h.to_string(), Some(format!("https://steamcommunity.com/id/{h}")))
                }
            }
        }
        ConnectionProvider::Twitch => {
            let seg = strip_url_prefix(
                trimmed,
                &["https://www.twitch.tv/", "https://twitch.tv/", "http://twitch.tv/", "twitch.tv/"],
            )
            .map(first_path_segment)
            .unwrap_or_else(|| trimmed.trim_start_matches('@'));
            if seg == trimmed {
                reject_unknown_url(trimmed, "Twitch")?;
            }
            validate_handle(seg, 2, 25, "Twitch")?;
            (seg.to_string(), Some(format!("https://www.twitch.tv/{seg}")))
        }
        ConnectionProvider::Instagram => {
            let seg = strip_url_prefix(
                trimmed,
                &[
                    "https://www.instagram.com/",
                    "https://instagram.com/",
                    "http://instagram.com/",
                    "instagram.com/",
                ],
            )
            .map(first_path_segment)
            .unwrap_or_else(|| trimmed.trim_start_matches('@'));
            if seg == trimmed {
                reject_unknown_url(trimmed, "Instagram")?;
            }
            validate_handle(seg, 1, 30, "Instagram")?;
            (format!("@{seg}"), Some(format!("https://www.instagram.com/{seg}/")))
        }
        ConnectionProvider::Tiktok => {
            let seg = strip_url_prefix(
                trimmed,
                &[
                    "https://www.tiktok.com/@",
                    "https://tiktok.com/@",
                    "http://tiktok.com/@",
                    "tiktok.com/@",
                ],
            )
            .map(first_path_segment)
            .unwrap_or_else(|| trimmed.trim_start_matches('@'));
            if seg == trimmed {
                reject_unknown_url(trimmed, "TikTok")?;
            }
            validate_handle(seg, 2, 24, "TikTok")?;
            (format!("@{seg}"), Some(format!("https://www.tiktok.com/@{seg}")))
        }
        ConnectionProvider::Youtube => {
            if let Some(rest) = strip_url_prefix(
                trimmed,
                &[
                    "https://www.youtube.com/channel/",
                    "https://youtube.com/channel/",
                    "youtube.com/channel/",
                ],
            ) {
                let seg = first_path_segment(rest);
                validate_handle(seg, 5, 40, "YouTube")?;
                (seg.to_string(), Some(format!("https://www.youtube.com/channel/{seg}")))
            } else {
                let seg = strip_url_prefix(
                    trimmed,
                    &[
                        "https://www.youtube.com/@",
                        "https://youtube.com/@",
                        "http://youtube.com/@",
                        "youtube.com/@",
                    ],
                )
                .map(first_path_segment)
                .unwrap_or_else(|| trimmed.trim_start_matches('@'));
                if seg == trimmed {
                    reject_unknown_url(trimmed, "YouTube")?;
                }
                validate_handle(seg, 2, 30, "YouTube")?;
                (format!("@{seg}"), Some(format!("https://www.youtube.com/@{seg}")))
            }
        }
        ConnectionProvider::X => {
            let seg = strip_url_prefix(
                trimmed,
                &[
                    "https://x.com/",
                    "http://x.com/",
                    "https://www.x.com/",
                    "x.com/",
                    "https://twitter.com/",
                    "https://www.twitter.com/",
                    "twitter.com/",
                ],
            )
            .map(first_path_segment)
            .unwrap_or_else(|| trimmed.trim_start_matches('@'));
            if seg == trimmed {
                reject_unknown_url(trimmed, "X")?;
            }
            validate_handle(seg, 1, 15, "X")?;
            (format!("@{seg}"), Some(format!("https://x.com/{seg}")))
        }
        ConnectionProvider::Roblox => {
            if let Some(rest) = strip_url_prefix(
                trimmed,
                &[
                    "https://www.roblox.com/users/",
                    "https://roblox.com/users/",
                    "roblox.com/users/",
                ],
            ) {
                let seg = first_path_segment(rest);
                if !seg.chars().all(|c| c.is_ascii_digit()) || seg.is_empty() || seg.len() > 20 {
                    return Err(ServerError::invalid_params("ID de usuário Roblox inválido."));
                }
                (seg.to_string(), Some(format!("https://www.roblox.com/users/{seg}/profile")))
            } else {
                reject_unknown_url(trimmed, "Roblox")?;
                let h = trimmed.trim_start_matches('@');
                validate_handle(h, 3, 20, "Roblox")?;
                (h.to_string(), Some(format!("https://www.roblox.com/search/users?keyword={h}")))
            }
        }
        ConnectionProvider::Linkedin => {
            let seg = strip_url_prefix(
                trimmed,
                &[
                    "https://www.linkedin.com/in/",
                    "https://linkedin.com/in/",
                    "http://linkedin.com/in/",
                    "linkedin.com/in/",
                ],
            )
            .map(first_path_segment)
            .unwrap_or_else(|| trimmed.trim_start_matches('@'));
            if seg == trimmed {
                reject_unknown_url(trimmed, "LinkedIn")?;
            }
            validate_handle(seg, 2, 64, "LinkedIn")?;
            (seg.to_string(), Some(format!("https://www.linkedin.com/in/{seg}")))
        }
        ConnectionProvider::Letterboxd => {
            let seg = strip_url_prefix(
                trimmed,
                &[
                    "https://letterboxd.com/",
                    "https://www.letterboxd.com/",
                    "http://letterboxd.com/",
                    "letterboxd.com/",
                ],
            )
            .map(first_path_segment)
            .unwrap_or_else(|| trimmed.trim_start_matches('@'));
            if seg == trimmed {
                reject_unknown_url(trimmed, "Letterboxd")?;
            }
            validate_handle(seg, 2, 24, "Letterboxd")?;
            (seg.to_string(), Some(format!("https://letterboxd.com/{seg}/")))
        }
    };

    Ok(NormalizedConnection {
        provider,
        handle,
        display_name: cleaned_display,
        profile_url,
    })
}

fn reject_unknown_url(raw: &str, label: &str) -> Result<(), ServerError> {
    let lower = raw.to_ascii_lowercase();
    if lower.contains("://") || lower.contains('/') {
        return Err(ServerError::invalid_params(format!(
            "Use apenas o nome de usuário ou um link oficial de {label}."
        )));
    }
    Ok(())
}

fn validate_handle(handle: &str, min_len: usize, max_len: usize, label: &str) -> Result<(), ServerError> {
    if handle.len() < min_len || handle.len() > max_len {
        return Err(ServerError::invalid_params(format!(
            "Usuário de {label} deve ter entre {min_len} e {max_len} caracteres."
        )));
    }
    if !handle.chars().all(is_safe_handle_char) {
        return Err(ServerError::invalid_params(format!(
            "Usuário de {label} contém caracteres não permitidos."
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_valid_handles_and_urls() {
        let sp = normalize_manual_connection(
            ConnectionProvider::Spotify,
            "https://open.spotify.com/user/criper_br?si=123",
            None,
        )
        .unwrap();
        assert_eq!(sp.handle, "criper_br");
        assert_eq!(sp.profile_url.as_deref(), Some("https://open.spotify.com/user/criper_br"));

        let ig = normalize_manual_connection(ConnectionProvider::Instagram, "@fulano.cti", None).unwrap();
        assert_eq!(ig.handle, "@fulano.cti");
        assert_eq!(ig.profile_url.as_deref(), Some("https://www.instagram.com/fulano.cti/"));

        let gh = normalize_manual_connection(ConnectionProvider::Github, "https://github.com/criperrr/", None).unwrap();
        assert_eq!(gh.handle, "criperrr");
        assert_eq!(gh.profile_url.as_deref(), Some("https://github.com/criperrr"));
    }

    #[test]
    fn rejects_phishing_and_xss_urls() {
        assert!(normalize_manual_connection(ConnectionProvider::Spotify, "javascript:alert(1)", None).is_err());
        assert!(normalize_manual_connection(ConnectionProvider::Github, "https://evil.com/criperrr", None).is_err());
        assert!(normalize_manual_connection(ConnectionProvider::Instagram, "https://phish.me/@user", None).is_err());
    }
}
