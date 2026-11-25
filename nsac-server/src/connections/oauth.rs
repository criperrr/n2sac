use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::RngExt;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use url::Url;
use zeroize::Zeroizing;

use crate::connections::{ConnectionProvider, VerificationSource};
use crate::error::ServerError;

const OAUTH_HTTP_TIMEOUT_SECS: u64 = 12;

#[derive(Debug, Clone)]
pub struct VerifiedExternalAccount {
    pub provider: ConnectionProvider,
    pub external_id: String,
    pub handle: String,
    pub display_name: Option<String>,
    pub profile_url: Option<String>,
    pub verification_source: VerificationSource,
}

#[derive(Debug, Clone)]
pub struct OAuthCallbackResult {
    pub primary: VerifiedExternalAccount,
    pub imported: Vec<VerifiedExternalAccount>,
}

fn env_non_empty(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

pub fn public_base_url() -> Option<String> {
    env_non_empty("OAUTH_PUBLIC_BASE_URL").map(|u| u.trim_end_matches('/').to_string())
}

pub fn is_oauth_configured(provider: ConnectionProvider) -> bool {
    if public_base_url().is_none() {
        return false;
    }
    match provider {
        ConnectionProvider::Discord => {
            env_non_empty("DISCORD_APPLICATION_ID").is_some()
                && env_non_empty("DISCORD_OAUTH_CLIENT_SECRET").is_some()
        }
        ConnectionProvider::Spotify => {
            env_non_empty("SPOTIFY_CLIENT_ID").is_some()
                && env_non_empty("SPOTIFY_CLIENT_SECRET").is_some()
        }
        ConnectionProvider::Github => {
            env_non_empty("GITHUB_CLIENT_ID").is_some()
                && env_non_empty("GITHUB_CLIENT_SECRET").is_some()
        }
        ConnectionProvider::Twitch => {
            env_non_empty("TWITCH_CLIENT_ID").is_some()
                && env_non_empty("TWITCH_CLIENT_SECRET").is_some()
        }
        ConnectionProvider::Steam => env_non_empty("STEAM_WEB_API_KEY").is_some(),
        _ => false,
    }
}

pub fn generate_random_token(num_bytes: usize) -> String {
    let mut bytes = vec![0u8; num_bytes];
    rand::rng().fill(bytes.as_mut_slice());
    URL_SAFE_NO_PAD.encode(bytes)
}

pub fn pkce_challenge_s256(verifier: &str) -> String {
    let digest = Sha256::digest(verifier.as_bytes());
    URL_SAFE_NO_PAD.encode(digest)
}

pub fn callback_url_for(provider: ConnectionProvider) -> Result<String, ServerError> {
    let base = public_base_url().ok_or_else(|| {
        ServerError::invalid_params("OAUTH_PUBLIC_BASE_URL não configurado no servidor.")
    })?;
    Ok(format!(
        "{base}/api/v1/connections/oauth/{}/callback",
        provider.as_str()
    ))
}

pub fn build_authorize_url(
    provider: ConnectionProvider,
    state: &str,
    code_challenge: Option<&str>,
) -> Result<String, ServerError> {
    let redirect_uri = callback_url_for(provider)?;
    match provider {
        ConnectionProvider::Discord => {
            let client_id = env_non_empty("DISCORD_APPLICATION_ID")
                .ok_or_else(|| ServerError::invalid_params("Discord OAuth não configurado."))?;
            let mut url = Url::parse("https://discord.com/oauth2/authorize")
                .map_err(|_| ServerError::internal("Invalid Discord authorize URL"))?;
            {
                let mut q = url.query_pairs_mut();
                q.append_pair("client_id", &client_id);
                q.append_pair("redirect_uri", &redirect_uri);
                q.append_pair("response_type", "code");
                q.append_pair("scope", "identify connections");
                q.append_pair("state", state);
                q.append_pair("prompt", "consent");
                if let Some(ch) = code_challenge {
                    q.append_pair("code_challenge", ch);
                    q.append_pair("code_challenge_method", "S256");
                }
            }
            Ok(url.into())
        }
        ConnectionProvider::Spotify => {
            let client_id = env_non_empty("SPOTIFY_CLIENT_ID")
                .ok_or_else(|| ServerError::invalid_params("Spotify OAuth não configurado."))?;
            let mut url = Url::parse("https://accounts.spotify.com/authorize")
                .map_err(|_| ServerError::internal("Invalid Spotify authorize URL"))?;
            {
                let mut q = url.query_pairs_mut();
                q.append_pair("client_id", &client_id);
                q.append_pair("redirect_uri", &redirect_uri);
                q.append_pair("response_type", "code");
                q.append_pair("scope", "user-read-private user-read-currently-playing");
                q.append_pair("state", state);
                if let Some(ch) = code_challenge {
                    q.append_pair("code_challenge", ch);
                    q.append_pair("code_challenge_method", "S256");
                }
            }
            Ok(url.into())
        }
        ConnectionProvider::Github => {
            let client_id = env_non_empty("GITHUB_CLIENT_ID")
                .ok_or_else(|| ServerError::invalid_params("GitHub OAuth não configurado."))?;
            let mut url = Url::parse("https://github.com/login/oauth/authorize")
                .map_err(|_| ServerError::internal("Invalid GitHub authorize URL"))?;
            {
                let mut q = url.query_pairs_mut();
                q.append_pair("client_id", &client_id);
                q.append_pair("redirect_uri", &redirect_uri);
                q.append_pair("scope", "read:user");
                q.append_pair("state", state);
            }
            Ok(url.into())
        }
        ConnectionProvider::Twitch => {
            let client_id = env_non_empty("TWITCH_CLIENT_ID")
                .ok_or_else(|| ServerError::invalid_params("Twitch OAuth não configurado."))?;
            let mut url = Url::parse("https://id.twitch.tv/oauth2/authorize")
                .map_err(|_| ServerError::internal("Invalid Twitch authorize URL"))?;
            {
                let mut q = url.query_pairs_mut();
                q.append_pair("client_id", &client_id);
                q.append_pair("redirect_uri", &redirect_uri);
                q.append_pair("response_type", "code");
                q.append_pair("state", state);
                if let Some(ch) = code_challenge {
                    q.append_pair("code_challenge", ch);
                    q.append_pair("code_challenge_method", "S256");
                }
            }
            Ok(url.into())
        }
        ConnectionProvider::Steam => {
            let base = public_base_url()
                .ok_or_else(|| ServerError::invalid_params("OAUTH_PUBLIC_BASE_URL não configurado."))?;
            let return_to = format!("{redirect_uri}?state={state}");
            let mut url = Url::parse("https://steamcommunity.com/openid/login")
                .map_err(|_| ServerError::internal("Invalid Steam OpenID URL"))?;
            {
                let mut q = url.query_pairs_mut();
                q.append_pair("openid.ns", "http://specs.openid.net/auth/2.0");
                q.append_pair("openid.mode", "checkid_setup");
                q.append_pair("openid.return_to", &return_to);
                q.append_pair("openid.realm", &base);
                q.append_pair(
                    "openid.identity",
                    "http://specs.openid.net/auth/2.0/identifier_select",
                );
                q.append_pair(
                    "openid.claimed_id",
                    "http://specs.openid.net/auth/2.0/identifier_select",
                );
            }
            Ok(url.into())
        }
        _ => Err(ServerError::invalid_params(
            "Este provedor não suporta verificação OAuth direta.",
        )),
    }
}

fn build_http_client() -> Result<reqwest::Client, ServerError> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(OAUTH_HTTP_TIMEOUT_SECS))
        .user_agent("n2sac-server/1.5")
        .build()
        .map_err(|_| ServerError::internal("Failed to construct OAuth HTTP client"))
}

#[derive(Deserialize)]
struct OAuthTokenResponse {
    access_token: String,
}

#[derive(Deserialize)]
struct DiscordUserMe {
    id: String,
    username: String,
    global_name: Option<String>,
}

#[derive(Deserialize)]
struct DiscordConnectionObj {
    id: String,
    name: String,
    #[serde(rename = "type")]
    conn_type: String,
    #[serde(default)]
    verified: bool,
    #[serde(default)]
    revoked: bool,
}

#[derive(Deserialize)]
struct GithubUser {
    id: i64,
    login: String,
    name: Option<String>,
    html_url: Option<String>,
}

#[derive(Deserialize)]
struct TwitchUsersResponse {
    data: Vec<TwitchUser>,
}

#[derive(Deserialize)]
struct TwitchUser {
    id: String,
    login: String,
    display_name: String,
}

#[derive(Deserialize)]
struct SteamSummaryResponse {
    response: SteamSummaryInner,
}

#[derive(Deserialize)]
struct SteamSummaryInner {
    players: Vec<SteamPlayer>,
}

#[derive(Deserialize)]
struct SteamPlayer {
    personaname: String,
    profileurl: Option<String>,
}

pub async fn complete_oauth_callback(
    provider: ConnectionProvider,
    code: &str,
    pkce_verifier: Option<&str>,
) -> Result<OAuthCallbackResult, ServerError> {
    let client = build_http_client()?;
    let redirect_uri = callback_url_for(provider)?;

    match provider {
        ConnectionProvider::Discord => {
            let client_id = env_non_empty("DISCORD_APPLICATION_ID")
                .ok_or_else(|| ServerError::invalid_params("Discord OAuth não configurado."))?;
            let client_secret = Zeroizing::new(
                env_non_empty("DISCORD_OAUTH_CLIENT_SECRET")
                    .ok_or_else(|| ServerError::invalid_params("Discord OAuth não configurado."))?,
            );

            let mut form = vec![
                ("client_id", client_id.as_str()),
                ("client_secret", client_secret.as_str()),
                ("grant_type", "authorization_code"),
                ("code", code),
                ("redirect_uri", redirect_uri.as_str()),
            ];
            if let Some(v) = pkce_verifier {
                form.push(("code_verifier", v));
            }

            let token_resp: OAuthTokenResponse = client
                .post("https://discord.com/api/v10/oauth2/token")
                .form(&form)
                .send()
                .await
                .map_err(|_| ServerError::internal("Falha ao contatar Discord OAuth."))?
                .error_for_status()
                .map_err(|_| ServerError::invalid_params("Código OAuth do Discord inválido ou expirado."))?
                .json()
                .await
                .map_err(|_| ServerError::internal("Resposta inválida do Discord OAuth."))?;
            let access_token = Zeroizing::new(token_resp.access_token);

            let user: DiscordUserMe = client
                .get("https://discord.com/api/v10/users/@me")
                .bearer_auth(access_token.as_str())
                .send()
                .await
                .map_err(|_| ServerError::internal("Falha ao buscar perfil do Discord."))?
                .error_for_status()
                .map_err(|_| ServerError::internal("Discord rejeitou a leitura do perfil."))?
                .json()
                .await
                .map_err(|_| ServerError::internal("Perfil do Discord inválido."))?;

            let mut imported = Vec::new();
            if let Ok(resp) = client
                .get("https://discord.com/api/v10/users/@me/connections")
                .bearer_auth(access_token.as_str())
                .send()
                .await
                && let Ok(ok_resp) = resp.error_for_status()
                && let Ok(conns) = ok_resp.json::<Vec<DiscordConnectionObj>>().await
            {
                for c in conns {
                    if c.revoked || !c.verified {
                        continue;
                    }
                    let Some(mapped) = ConnectionProvider::from_discord_type(&c.conn_type) else {
                        continue;
                    };
                    let profile_url = mapped.build_profile_url(Some(&c.id), &c.name);
                    imported.push(VerifiedExternalAccount {
                        provider: mapped,
                        external_id: c.id,
                        handle: c.name.clone(),
                        display_name: Some(c.name),
                        profile_url,
                        verification_source: VerificationSource::DiscordImport,
                    });
                }
            }

            let primary = VerifiedExternalAccount {
                provider: ConnectionProvider::Discord,
                external_id: user.id,
                handle: format!("@{}", user.username),
                display_name: user.global_name.filter(|s| !s.trim().is_empty()),
                profile_url: None,
                verification_source: VerificationSource::Oauth,
            };

            Ok(OAuthCallbackResult { primary, imported })
        }
        ConnectionProvider::Spotify => {
            #[derive(Deserialize)]
            struct SpotifyExternalUrls {
                spotify: Option<String>,
            }
            #[derive(Deserialize)]
            struct SpotifyUserMe {
                id: String,
                display_name: Option<String>,
                external_urls: Option<SpotifyExternalUrls>,
            }

            let client_id = env_non_empty("SPOTIFY_CLIENT_ID")
                .ok_or_else(|| ServerError::invalid_params("Spotify OAuth não configurado."))?;
            let client_secret = Zeroizing::new(
                env_non_empty("SPOTIFY_CLIENT_SECRET")
                    .ok_or_else(|| ServerError::invalid_params("Spotify OAuth não configurado."))?,
            );

            let mut form = vec![
                ("grant_type", "authorization_code"),
                ("code", code),
                ("redirect_uri", redirect_uri.as_str()),
            ];
            if let Some(v) = pkce_verifier {
                form.push(("code_verifier", v));
            }

            let token_resp: OAuthTokenResponse = client
                .post("https://accounts.spotify.com/api/token")
                .basic_auth(client_id.as_str(), Some(client_secret.as_str()))
                .form(&form)
                .send()
                .await
                .map_err(|_| ServerError::internal("Falha ao contatar Spotify OAuth."))?
                .error_for_status()
                .map_err(|_| ServerError::invalid_params("Código OAuth do Spotify inválido ou expirado."))?
                .json()
                .await
                .map_err(|_| ServerError::internal("Resposta inválida do Spotify OAuth."))?;
            let access_token = Zeroizing::new(token_resp.access_token);

            let user: SpotifyUserMe = client
                .get("https://api.spotify.com/v1/me")
                .bearer_auth(access_token.as_str())
                .send()
                .await
                .map_err(|_| ServerError::internal("Falha ao buscar perfil do Spotify."))?
                .error_for_status()
                .map_err(|_| ServerError::internal("Spotify rejeitou a leitura do perfil."))?
                .json()
                .await
                .map_err(|_| ServerError::internal("Perfil do Spotify inválido."))?;

            let profile_url = user
                .external_urls
                .and_then(|u| u.spotify)
                .filter(|s| s.starts_with("https://open.spotify.com/"))
                .or_else(|| Some(format!("https://open.spotify.com/user/{}", user.id)));

            let primary = VerifiedExternalAccount {
                provider: ConnectionProvider::Spotify,
                external_id: user.id.clone(),
                handle: user.id,
                display_name: user.display_name.filter(|s| !s.trim().is_empty()),
                profile_url,
                verification_source: VerificationSource::Oauth,
            };

            Ok(OAuthCallbackResult {
                primary,
                imported: Vec::new(),
            })
        }
        ConnectionProvider::Github => {
            let client_id = env_non_empty("GITHUB_CLIENT_ID")
                .ok_or_else(|| ServerError::invalid_params("GitHub OAuth não configurado."))?;
            let client_secret = Zeroizing::new(
                env_non_empty("GITHUB_CLIENT_SECRET")
                    .ok_or_else(|| ServerError::invalid_params("GitHub OAuth não configurado."))?,
            );

            let token_resp: OAuthTokenResponse = client
                .post("https://github.com/login/oauth/access_token")
                .header(reqwest::header::ACCEPT, "application/json")
                .form(&[
                    ("client_id", client_id.as_str()),
                    ("client_secret", client_secret.as_str()),
                    ("code", code),
                    ("redirect_uri", redirect_uri.as_str()),
                ])
                .send()
                .await
                .map_err(|_| ServerError::internal("Falha ao contatar GitHub OAuth."))?
                .error_for_status()
                .map_err(|_| ServerError::invalid_params("Código OAuth do GitHub inválido ou expirado."))?
                .json()
                .await
                .map_err(|_| ServerError::internal("Resposta inválida do GitHub OAuth."))?;
            let access_token = Zeroizing::new(token_resp.access_token);

            let user: GithubUser = client
                .get("https://api.github.com/user")
                .bearer_auth(access_token.as_str())
                .send()
                .await
                .map_err(|_| ServerError::internal("Falha ao buscar perfil do GitHub."))?
                .error_for_status()
                .map_err(|_| ServerError::internal("GitHub rejeitou a leitura do perfil."))?
                .json()
                .await
                .map_err(|_| ServerError::internal("Perfil do GitHub inválido."))?;

            Ok(OAuthCallbackResult {
                primary: VerifiedExternalAccount {
                    provider: ConnectionProvider::Github,
                    external_id: user.id.to_string(),
                    handle: user.login.clone(),
                    display_name: user.name.filter(|s| !s.trim().is_empty()),
                    profile_url: user
                        .html_url
                        .or_else(|| Some(format!("https://github.com/{}", user.login))),
                    verification_source: VerificationSource::Oauth,
                },
                imported: Vec::new(),
            })
        }
        ConnectionProvider::Twitch => {
            let client_id = env_non_empty("TWITCH_CLIENT_ID")
                .ok_or_else(|| ServerError::invalid_params("Twitch OAuth não configurado."))?;
            let client_secret = Zeroizing::new(
                env_non_empty("TWITCH_CLIENT_SECRET")
                    .ok_or_else(|| ServerError::invalid_params("Twitch OAuth não configurado."))?,
            );

            let mut form = vec![
                ("client_id", client_id.as_str()),
                ("client_secret", client_secret.as_str()),
                ("code", code),
                ("grant_type", "authorization_code"),
                ("redirect_uri", redirect_uri.as_str()),
            ];
            if let Some(v) = pkce_verifier {
                form.push(("code_verifier", v));
            }

            let token_resp: OAuthTokenResponse = client
                .post("https://id.twitch.tv/oauth2/token")
                .form(&form)
                .send()
                .await
                .map_err(|_| ServerError::internal("Falha ao contatar Twitch OAuth."))?
                .error_for_status()
                .map_err(|_| ServerError::invalid_params("Código OAuth da Twitch inválido ou expirado."))?
                .json()
                .await
                .map_err(|_| ServerError::internal("Resposta inválida da Twitch OAuth."))?;
            let access_token = Zeroizing::new(token_resp.access_token);

            let users: TwitchUsersResponse = client
                .get("https://api.twitch.tv/helix/users")
                .bearer_auth(access_token.as_str())
                .header("Client-Id", client_id.as_str())
                .send()
                .await
                .map_err(|_| ServerError::internal("Falha ao buscar perfil da Twitch."))?
                .error_for_status()
                .map_err(|_| ServerError::internal("Twitch rejeitou a leitura do perfil."))?
                .json()
                .await
                .map_err(|_| ServerError::internal("Perfil da Twitch inválido."))?;

            let user = users
                .data
                .into_iter()
                .next()
                .ok_or_else(|| ServerError::internal("Twitch não retornou o usuário."))?;

            Ok(OAuthCallbackResult {
                primary: VerifiedExternalAccount {
                    provider: ConnectionProvider::Twitch,
                    external_id: user.id,
                    handle: user.login.clone(),
                    display_name: Some(user.display_name),
                    profile_url: Some(format!("https://www.twitch.tv/{}", user.login)),
                    verification_source: VerificationSource::Oauth,
                },
                imported: Vec::new(),
            })
        }
        _ => Err(ServerError::invalid_params("Provedor OAuth não suportado.")),
    }
}

pub async fn complete_steam_openid_callback(
    query_pairs: &[(String, String)],
) -> Result<VerifiedExternalAccount, ServerError> {
    let api_key = Zeroizing::new(
        env_non_empty("STEAM_WEB_API_KEY")
            .ok_or_else(|| ServerError::invalid_params("Steam OpenID não configurado."))?,
    );
    let client = build_http_client()?;

    let mut verify_form: Vec<(String, String)> = query_pairs
        .iter()
        .filter(|(k, _)| k.starts_with("openid."))
        .map(|(k, v)| {
            if k == "openid.mode" {
                (k.clone(), "check_authentication".to_string())
            } else {
                (k.clone(), v.clone())
            }
        })
        .collect();
    if !verify_form.iter().any(|(k, _)| k == "openid.mode") {
        verify_form.push(("openid.mode".to_string(), "check_authentication".to_string()));
    }

    let body = client
        .post("https://steamcommunity.com/openid/login")
        .form(&verify_form)
        .send()
        .await
        .map_err(|_| ServerError::internal("Falha ao validar login Steam."))?
        .text()
        .await
        .map_err(|_| ServerError::internal("Resposta inválida da Steam."))?;

    if !body.contains("is_valid:true") {
        return Err(ServerError::invalid_params(
            "Assinatura OpenID da Steam inválida ou expirada.",
        ));
    }

    let claimed_id = query_pairs
        .iter()
        .find(|(k, _)| k == "openid.claimed_id")
        .map(|(_, v)| v.as_str())
        .ok_or_else(|| ServerError::invalid_params("SteamID ausente na resposta."))?;

    let steam_id = claimed_id
        .rsplit('/')
        .next()
        .filter(|s| s.chars().all(|c| c.is_ascii_digit()) && s.len() >= 15)
        .ok_or_else(|| ServerError::invalid_params("SteamID64 inválido."))?
        .to_string();

    let summary_url = format!(
        "https://api.steampowered.com/ISteamUser/GetPlayerSummaries/v0002/?key={}&steamids={}",
        api_key.as_str(),
        steam_id
    );
    let summary: SteamSummaryResponse = client
        .get(&summary_url)
        .send()
        .await
        .map_err(|_| ServerError::internal("Falha ao consultar perfil Steam."))?
        .error_for_status()
        .map_err(|_| ServerError::internal("Steam Web API rejeitou a consulta."))?
        .json()
        .await
        .map_err(|_| ServerError::internal("Resposta de perfil Steam inválida."))?;

    let player = summary.response.players.into_iter().next();
    let persona = player
        .as_ref()
        .map(|p| p.personaname.clone())
        .unwrap_or_else(|| steam_id.clone());
    let profile_url = player
        .and_then(|p| p.profileurl)
        .or_else(|| Some(format!("https://steamcommunity.com/profiles/{steam_id}")));

    Ok(VerifiedExternalAccount {
        provider: ConnectionProvider::Steam,
        external_id: steam_id,
        handle: persona.clone(),
        display_name: Some(persona),
        profile_url,
        verification_source: VerificationSource::Oauth,
    })
}
