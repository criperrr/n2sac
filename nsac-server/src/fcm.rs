//! FCM HTTP v1 push notification client (M3).

use std::fmt;
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::Utc;
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use crate::config::FcmConfig;
use crate::error::ServerError;

const DEFAULT_FCM_TIMEOUT_SECS: u64 = 10;
const DEFAULT_TOKEN_TTL_SECS: u64 = 3600;
const TOKEN_REFRESH_MARGIN_SECS: u64 = 30;
const FCM_MESSAGING_SCOPE: &str = "https://www.googleapis.com/auth/firebase.messaging";
const JWT_BEARER_GRANT_TYPE: &str = "urn:ietf:params:oauth:grant-type:jwt-bearer";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FcmDeliveryOutcome {
    Delivered { message_id: String },
    Unregistered,
    TransientFailure { status: u16, detail: String },
    Disabled,
}

#[derive(Clone)]
pub struct FcmClient {
    config: FcmConfig,
    http: reqwest::Client,
    token_cache: Arc<RwLock<Option<(String, Instant)>>>,
}

impl fmt::Debug for FcmClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FcmClient")
            .field("config", &self.config)
            .finish()
    }
}

#[derive(Debug, Deserialize)]
struct ServiceAccountCredentials {
    client_email: String,
    private_key: String,
    #[serde(default)]
    token_uri: Option<String>,
}

#[derive(Debug, Serialize)]
struct ServiceAccountClaims<'a> {
    iss: &'a str,
    scope: &'a str,
    aud: &'a str,
    iat: i64,
    exp: i64,
}

#[derive(Debug, Deserialize)]
struct OAuthTokenResponse {
    access_token: String,
    #[serde(default)]
    expires_in: Option<u64>,
}

#[derive(Debug, Serialize)]
struct FcmSendRequest<'a> {
    message: FcmMessage<'a>,
}

#[derive(Debug, Serialize)]
struct FcmMessage<'a> {
    token: &'a str,
    notification: FcmNotification,
    data: FcmData<'a>,
}

#[derive(Debug, Serialize)]
struct FcmNotification {
    title: String,
    body: String,
}

#[derive(Debug, Serialize)]
struct FcmData<'a> {
    class_code: &'a str,
    subjects: String,
    bimesters: String,
}

impl FcmClient {
    pub fn new(config: FcmConfig) -> Result<Self, ServerError> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(DEFAULT_FCM_TIMEOUT_SECS))
            .build()
            .map_err(|err| {
                ServerError::internal(format!("Failed to build FCM HTTP client: {err}"))
            })?;

        Ok(Self {
            config,
            http,
            token_cache: Arc::new(RwLock::new(None)),
        })
    }

    pub fn is_enabled(&self) -> bool {
        self.config.enabled && self.config.endpoint_url.is_some()
    }

    pub async fn send_grade_notification(
        &self,
        fcm_token: &str,
        class_code: &str,
        subjects: &[String],
        bimesters: &[u8],
    ) -> Result<FcmDeliveryOutcome, ServerError> {
        if !self.config.enabled {
            return Ok(FcmDeliveryOutcome::Disabled);
        }

        let Some(endpoint_url) = self.config.endpoint_url.as_deref() else {
            return Ok(FcmDeliveryOutcome::Disabled);
        };

        let access_token = match self.acquire_access_token().await {
            Ok(token) => token,
            Err(outcome) => return Ok(outcome),
        };

        let subjects_joined = if subjects.is_empty() {
            "Boletim".to_string()
        } else {
            subjects.join(", ")
        };

        let bimesters_joined = if bimesters.is_empty() {
            "1".to_string()
        } else {
            bimesters
                .iter()
                .map(u8::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        };

        let payload = FcmSendRequest {
            message: FcmMessage {
                token: fcm_token,
                notification: FcmNotification {
                    title: format!("Novas notas no NSAC ({class_code})"),
                    body: format!(
                        "Notas atualizadas: {subjects_joined} ({bimesters_joined}º bimestre)"
                    ),
                },
                data: FcmData {
                    class_code,
                    subjects: subjects_joined,
                    bimesters: bimesters_joined,
                },
            },
        };

        let response = match self
            .http
            .post(endpoint_url)
            .bearer_auth(&access_token)
            .json(&payload)
            .send()
            .await
        {
            Ok(resp) => resp,
            Err(err) => {
                let detail = format!("FCM HTTP request error: {err}");
                tracing::warn!(
                    class_code = class_code,
                    error = %err,
                    "FCM HTTP push request failed"
                );
                return Ok(FcmDeliveryOutcome::TransientFailure { status: 0, detail });
            }
        };

        let status = response.status().as_u16();
        let body_text = match response.text().await {
            Ok(text) => text,
            Err(err) => {
                let detail = format!("Failed to read FCM response body: {err}");
                tracing::warn!(
                    class_code = class_code,
                    status = status,
                    error = %err,
                    "Failed to read FCM HTTP response body"
                );
                return Ok(FcmDeliveryOutcome::TransientFailure { status, detail });
            }
        };

        if (200..=299).contains(&status) {
            let message_id = serde_json::from_str::<serde_json::Value>(&body_text)
                .ok()
                .and_then(|v| {
                    v.get("name")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned)
                })
                .unwrap_or_else(|| "ok".to_string());

            return Ok(FcmDeliveryOutcome::Delivered { message_id });
        }

        if status == 404 || is_unregistered_response(&body_text) {
            return Ok(FcmDeliveryOutcome::Unregistered);
        }

        let detail = extract_error_detail(&body_text).unwrap_or(body_text);
        tracing::warn!(
            class_code = class_code,
            status = status,
            detail = %detail,
            "FCM push delivery returned transient error"
        );
        Ok(FcmDeliveryOutcome::TransientFailure { status, detail })
    }

    async fn acquire_access_token(&self) -> Result<String, FcmDeliveryOutcome> {
        {
            let guard = self.token_cache.read().await;
            if let Some((token, expires_at)) = guard.as_ref()
                && Instant::now() < *expires_at
            {
                return Ok(token.clone());
            }
        }

        let mut guard = self.token_cache.write().await;
        if let Some((token, expires_at)) = guard.as_ref()
            && Instant::now() < *expires_at
        {
            return Ok(token.clone());
        }

        let Some(oauth_url) = self.config.oauth_token_url.as_deref() else {
            return Ok("mock-fcm-oauth2-access-token".to_string());
        };

        let form_params = self.build_oauth_form_params(oauth_url);
        let response = match self.http.post(oauth_url).form(&form_params).send().await {
            Ok(resp) => resp,
            Err(err) => {
                let detail = format!("FCM OAuth2 token request error: {err}");
                tracing::warn!(error = %err, "Failed to request FCM OAuth2 access token");
                return Err(FcmDeliveryOutcome::TransientFailure { status: 0, detail });
            }
        };

        let status = response.status().as_u16();
        let body_text = match response.text().await {
            Ok(text) => text,
            Err(err) => {
                let detail = format!("Failed to read FCM OAuth2 token response: {err}");
                tracing::warn!(
                    status = status,
                    error = %err,
                    "Failed to read FCM OAuth2 token response body"
                );
                return Err(FcmDeliveryOutcome::TransientFailure { status, detail });
            }
        };

        if !(200..=299).contains(&status) {
            let detail = format!("FCM OAuth2 token endpoint returned HTTP {status}: {body_text}");
            tracing::warn!(
                status = status,
                "FCM OAuth2 token endpoint returned non-2xx status"
            );
            return Err(FcmDeliveryOutcome::TransientFailure { status, detail });
        }

        let parsed: OAuthTokenResponse = match serde_json::from_str(&body_text) {
            Ok(parsed) => parsed,
            Err(err) => {
                let detail = format!("Invalid FCM OAuth2 token JSON: {err}");
                tracing::warn!(error = %err, "Failed to parse FCM OAuth2 token JSON");
                return Err(FcmDeliveryOutcome::TransientFailure { status, detail });
            }
        };

        let ttl_secs = parsed
            .expires_in
            .unwrap_or(DEFAULT_TOKEN_TTL_SECS)
            .saturating_sub(TOKEN_REFRESH_MARGIN_SECS)
            .max(1);
        let expires_at = Instant::now() + Duration::from_secs(ttl_secs);
        let token = parsed.access_token;
        *guard = Some((token.clone(), expires_at));

        Ok(token)
    }

    fn build_oauth_form_params(&self, oauth_url: &str) -> Vec<(&'static str, String)> {
        if let Some(raw_creds) = self.config.credentials_json.as_ref()
            && let Some(assertion) = build_service_account_assertion(raw_creds.as_str(), oauth_url)
        {
            return vec![
                ("grant_type", JWT_BEARER_GRANT_TYPE.to_string()),
                ("assertion", assertion),
            ];
        }

        vec![("grant_type", "client_credentials".to_string())]
    }
}

fn build_service_account_assertion(raw_json: &str, default_aud: &str) -> Option<String> {
    let creds: ServiceAccountCredentials = serde_json::from_str(raw_json).ok()?;
    let client_email = creds.client_email.trim();
    let private_key = creds.private_key.trim();
    if client_email.is_empty() || private_key.is_empty() {
        return None;
    }

    let encoding_key = EncodingKey::from_rsa_pem(private_key.as_bytes()).ok()?;
    let aud = creds
        .token_uri
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(default_aud);

    let now = Utc::now().timestamp();
    let claims = ServiceAccountClaims {
        iss: client_email,
        scope: FCM_MESSAGING_SCOPE,
        aud,
        iat: now,
        exp: now.saturating_add(3600),
    };

    jsonwebtoken::encode(&Header::new(Algorithm::RS256), &claims, &encoding_key).ok()
}

fn is_unregistered_response(body_text: &str) -> bool {
    body_text.contains("UNREGISTERED") || body_text.contains("NOT_FOUND")
}

fn extract_error_detail(body_text: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(body_text).ok()?;
    let error = value.get("error")?;
    let message = error.get("message").and_then(serde_json::Value::as_str);
    let status = error.get("status").and_then(serde_json::Value::as_str);

    match (status, message) {
        (Some(s), Some(m)) => Some(format!("{s}: {m}")),
        (None, Some(m)) => Some(m.to_owned()),
        (Some(s), None) => Some(s.to_owned()),
        (None, None) => None,
    }
}
