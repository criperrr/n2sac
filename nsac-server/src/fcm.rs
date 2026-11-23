//! FCM HTTP v1 push notification client (M3 + High-Priority Grades & Class Announcements).

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

/// Versioned Android NotificationChannel IDs.
/// Android permanently caches a channel's sound & importance by ID; bumping IDs ensures
/// existing installs migrate cleanly off legacy channels created with a broken sound URI.
pub const CHANNEL_GRADES: &str = "n2sac-grades-v2";
pub const CHANNEL_ANNOUNCEMENTS: &str = "n2sac-announcements-v1";
pub const CHANNEL_SOCIAL_MESSAGES: &str = "n2sac-social-messages-v2";
pub const CHANNEL_SOCIAL_FRIENDS: &str = "n2sac-social-friends-v2";
pub const CHANNEL_SOCIAL_FOLLOWS: &str = "n2sac-social-follows-v2";
pub const CHANNEL_ROOM_CHAT: &str = "n2sac-room-chat-v1";
pub const CHANNEL_ROOM_OFFTOPIC: &str = "n2sac-room-offtopic-v1";

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
struct FcmGenericSendRequest<'a, D: Serialize> {
    message: FcmGenericMessage<'a, D>,
}

#[derive(Debug, Serialize)]
struct FcmGenericMessage<'a, D: Serialize> {
    token: &'a str,
    notification: FcmNotification,
    data: D,
    android: FcmAndroidConfig,
}

#[derive(Debug, Serialize)]
struct FcmNotification {
    title: String,
    body: String,
}

#[derive(Debug, Serialize)]
struct FcmAndroidConfig {
    priority: &'static str,
    ttl: &'static str,
    notification: FcmAndroidNotification,
}

#[derive(Debug, Serialize)]
struct FcmAndroidNotification {
    channel_id: &'static str,
    notification_priority: &'static str,
    visibility: &'static str,
    default_sound: bool,
    default_vibrate_timings: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    tag: Option<String>,
}

#[derive(Debug, Serialize)]
struct FcmDataOnlySendRequest<'a, D: Serialize> {
    message: FcmDataOnlyMessage<'a, D>,
}

#[derive(Debug, Serialize)]
struct FcmDataOnlyMessage<'a, D: Serialize> {
    token: &'a str,
    data: D,
    android: FcmDataOnlyAndroidConfig,
}

#[derive(Debug, Serialize)]
struct FcmDataOnlyAndroidConfig {
    priority: &'static str,
    ttl: &'static str,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StudentGradeHighlight {
    pub subject: String,
    pub bimester: u8,
    pub year: i32,
    pub personal_grade: Option<f64>,
    pub class_average: Option<f64>,
    pub previous_grade: Option<f64>,
    pub recovery: bool,
}

#[derive(Debug, Serialize)]
struct FcmGradeData<'a> {
    kind: &'static str,
    channel_id: &'static str,
    title: String,
    body: String,
    class_code: &'a str,
    subjects: String,
    bimesters: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    primary_subject: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    primary_bimester: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    primary_year: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    primary_grade: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    primary_class_avg: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    highlights_json: String,
}

/// Up to 3 new grades show inline previews; at 4+ the notification switches to a clean
/// "Múltiplas novas notas saíram" summary prompt and the tap modal shows everything.
const MAX_INLINE_GRADE_PREVIEW: usize = 3;
const MAX_HIGHLIGHTS_IN_FCM_DATA: usize = 16;

fn format_grade_br(value: f64) -> String {
    format!("{value:.1}").replace('.', ",")
}

fn format_signed_grade_br(delta: f64) -> String {
    let sign = if delta >= 0.0 { "+" } else { "-" };
    format!("{sign}{:.1}", delta.abs()).replace('.', ",")
}

#[derive(Debug, Serialize)]
struct FcmSocialData<'a> {
    kind: &'a str,
    actor_id: String,
    actor_label: &'a str,
}

#[derive(Debug, Serialize)]
struct FcmAnnouncementData<'a> {
    kind: &'static str,
    class_code: &'a str,
    announcement_id: String,
    author_label: &'a str,
    category: &'a str,
}

#[derive(Debug, Serialize)]
struct FcmRoomChatData<'a> {
    kind: &'static str,
    class_code: &'a str,
    channel_kind: &'a str,
    message_id: String,
    sender_label: &'a str,
}

fn social_android_channel_and_tag(kind: &str, actor_id: i64) -> (&'static str, String) {
    match kind {
        "chat_message" => (CHANNEL_SOCIAL_MESSAGES, format!("chat-{actor_id}")),
        "friendship" => (CHANNEL_SOCIAL_FRIENDS, format!("friend-{actor_id}")),
        _ => (CHANNEL_SOCIAL_FOLLOWS, format!("follow-{actor_id}")),
    }
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

    async fn dispatch_payload<P: Serialize>(
        &self,
        payload: &P,
        log_context: &str,
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

        let response = match self
            .http
            .post(endpoint_url)
            .bearer_auth(&access_token)
            .json(payload)
            .send()
            .await
        {
            Ok(resp) => resp,
            Err(err) => {
                let detail = format!("FCM HTTP request error: {err}");
                tracing::warn!(
                    context = log_context,
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
                    context = log_context,
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
            context = log_context,
            status = status,
            detail = %detail,
            "FCM push delivery returned transient error"
        );
        Ok(FcmDeliveryOutcome::TransientFailure { status, detail })
    }

    /// Dispatches a high-priority (PRIORITY_MAX + IMPORTANCE_HIGH) grade alert.
    pub async fn send_grade_notification(
        &self,
        fcm_token: &str,
        class_code: &str,
        subjects: &[String],
        bimesters: &[u8],
    ) -> Result<FcmDeliveryOutcome, ServerError> {
        self.send_grade_notification_detailed(fcm_token, class_code, subjects, bimesters, &[])
            .await
    }

    /// Dispatches a personalized high-priority grade alert including the student's actual
    /// posted grade(s), class average comparison, evolution vs previous bimester, and
    /// structured deep-link metadata so tapping the notification opens the subject view directly.
    pub async fn send_grade_notification_detailed(
        &self,
        fcm_token: &str,
        class_code: &str,
        subjects: &[String],
        bimesters: &[u8],
        highlights: &[StudentGradeHighlight],
    ) -> Result<FcmDeliveryOutcome, ServerError> {
        let subjects_joined = if subjects.is_empty() {
            highlights
                .first()
                .map(|h| h.subject.clone())
                .unwrap_or_else(|| "Boletim".to_string())
        } else {
            subjects.join(", ")
        };

        let bimesters_joined = if bimesters.is_empty() {
            highlights
                .first()
                .map(|h| h.bimester.to_string())
                .unwrap_or_else(|| "1".to_string())
        } else {
            bimesters
                .iter()
                .map(u8::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        };

        let (notif_title, notif_body) = match highlights {
            [single] => {
                let title = match single.personal_grade {
                    Some(g) => format!("Nova nota em {}: {}", single.subject, format_grade_br(g)),
                    None => format!("Nova nota em {} ({}º Bim)", single.subject, single.bimester),
                };
                let mut parts = vec![format!("{}º Bim", single.bimester)];
                if let (Some(g), Some(avg)) = (single.personal_grade, single.class_average) {
                    parts.push(format!(
                        "Sala: {} ({})",
                        format_grade_br(avg),
                        format_signed_grade_br(g - avg)
                    ));
                } else if let Some(avg) = single.class_average {
                    parts.push(format!("Média sala: {}", format_grade_br(avg)));
                }
                if let (Some(g), Some(prev)) = (single.personal_grade, single.previous_grade) {
                    parts.push(format!("Evolução: {}", format_signed_grade_br(g - prev)));
                }
                if parts.len() == 1 {
                    parts.push(format!("Turma {class_code} • Toque para ver detalhes"));
                }
                (title, parts.join(" • "))
            }
            many if many.len() >= 2 && many.len() <= MAX_INLINE_GRADE_PREVIEW => {
                let title = format!("{} novas notas lançadas ({class_code})", many.len());
                let preview: Vec<String> = many
                    .iter()
                    .map(|h| match h.personal_grade {
                        Some(g) => format!("{}: {} ({}ºB)", h.subject, format_grade_br(g), h.bimester),
                        None => format!("{} ({}ºB)", h.subject, h.bimester),
                    })
                    .collect();
                (title, preview.join(" • "))
            }
            many if many.len() > MAX_INLINE_GRADE_PREVIEW => (
                format!("Múltiplas novas notas saíram ({})", many.len()),
                format!(
                    "Múltiplas notas novas saíram no boletim ({class_code}). Toque para ver o resumo completo."
                ),
            ),
            _ if subjects.len() > MAX_INLINE_GRADE_PREVIEW => (
                format!("Múltiplas novas notas saíram ({})", subjects.len()),
                format!(
                    "Múltiplas notas novas saíram no boletim ({class_code}). Toque para ver o resumo completo."
                ),
            ),
            _ => (
                format!("Novas notas no NSAC ({class_code})"),
                format!("Notas atualizadas: {subjects_joined} ({bimesters_joined}º bimestre)"),
            ),
        };

        let primary = highlights.first();
        let primary_subject = primary
            .map(|h| h.subject.clone())
            .or_else(|| subjects.first().cloned())
            .unwrap_or_default();
        let primary_bimester = primary
            .map(|h| h.bimester.to_string())
            .or_else(|| bimesters.first().map(u8::to_string))
            .unwrap_or_default();
        let primary_year = primary.map(|h| h.year.to_string()).unwrap_or_default();
        let primary_grade = primary
            .and_then(|h| h.personal_grade)
            .map(|g| format!("{g:.2}"))
            .unwrap_or_default();
        let primary_class_avg = primary
            .and_then(|h| h.class_average)
            .map(|g| format!("{g:.2}"))
            .unwrap_or_default();

        let capped_highlights: Vec<&StudentGradeHighlight> = highlights
            .iter()
            .take(MAX_HIGHLIGHTS_IN_FCM_DATA)
            .collect();
        let highlights_json = if capped_highlights.is_empty() {
            String::new()
        } else {
            serde_json::to_string(&capped_highlights).unwrap_or_default()
        };

        // High-priority data-only FCM message so Android's `NsacMessagingService` ALWAYS runs
        // (foreground, background, or killed) and natively accumulates unread grade notifications
        // into a single smart summary notification on `CHANNEL_GRADES`.
        let payload = FcmDataOnlySendRequest {
            message: FcmDataOnlyMessage {
                token: fcm_token,
                data: FcmGradeData {
                    kind: "grade_update",
                    channel_id: CHANNEL_GRADES,
                    title: notif_title,
                    body: notif_body,
                    class_code,
                    subjects: subjects_joined,
                    bimesters: bimesters_joined,
                    primary_subject,
                    primary_bimester,
                    primary_year,
                    primary_grade,
                    primary_class_avg,
                    highlights_json,
                },
                android: FcmDataOnlyAndroidConfig {
                    priority: "high",
                    ttl: "86400s",
                },
            },
        };

        self.dispatch_payload(&payload, class_code).await
    }

    /// Dispatches a high-priority (PRIORITY_MAX + IMPORTANCE_HIGH) class announcement alert.
    #[allow(clippy::too_many_arguments)]
    pub async fn send_announcement_notification(
        &self,
        fcm_token: &str,
        class_code: &str,
        announcement_id: i64,
        author_label: &str,
        category: &str,
        title: &str,
        body_preview: &str,
    ) -> Result<FcmDeliveryOutcome, ServerError> {
        let notif_title = if title.trim().is_empty() {
            format!("Anúncio • {class_code}")
        } else {
            format!("{class_code}: {}", title.trim())
        };
        let notif_body = format!("{author_label}: {body_preview}");

        let payload = FcmGenericSendRequest {
            message: FcmGenericMessage {
                token: fcm_token,
                notification: FcmNotification {
                    title: notif_title,
                    body: notif_body,
                },
                data: FcmAnnouncementData {
                    kind: "class_announcement",
                    class_code,
                    announcement_id: announcement_id.to_string(),
                    author_label,
                    category,
                },
                android: FcmAndroidConfig {
                    priority: "high",
                    ttl: "86400s",
                    notification: FcmAndroidNotification {
                        channel_id: CHANNEL_ANNOUNCEMENTS,
                        notification_priority: "PRIORITY_MAX",
                        visibility: "PUBLIC",
                        default_sound: true,
                        default_vibrate_timings: true,
                        tag: Some(format!("announcement-{announcement_id}")),
                    },
                },
            },
        };

        self.dispatch_payload(&payload, "class_announcement").await
    }

    /// Dispatches a class group chat / off-topic notification (respecting channel priority).
    #[allow(clippy::too_many_arguments)]
    pub async fn send_classroom_chat_notification(
        &self,
        fcm_token: &str,
        class_code: &str,
        channel_kind: &str,
        message_id: i64,
        sender_label: &str,
        preview: &str,
    ) -> Result<FcmDeliveryOutcome, ServerError> {
        let is_off_topic = channel_kind == "off_topic";
        let (channel_id, kind, priority_label, notif_title) = if is_off_topic {
            (
                CHANNEL_ROOM_OFFTOPIC,
                "room_offtopic",
                "PRIORITY_DEFAULT",
                format!("Off-Topic • {class_code}"),
            )
        } else {
            (
                CHANNEL_ROOM_CHAT,
                "room_chat",
                "PRIORITY_HIGH",
                format!("Chat da Turma • {class_code}"),
            )
        };

        let payload = FcmGenericSendRequest {
            message: FcmGenericMessage {
                token: fcm_token,
                notification: FcmNotification {
                    title: notif_title,
                    body: format!("{sender_label}: {preview}"),
                },
                data: FcmRoomChatData {
                    kind,
                    class_code,
                    channel_kind,
                    message_id: message_id.to_string(),
                    sender_label,
                },
                android: FcmAndroidConfig {
                    priority: if is_off_topic { "normal" } else { "high" },
                    ttl: "3600s",
                    notification: FcmAndroidNotification {
                        channel_id,
                        notification_priority: priority_label,
                        visibility: "PUBLIC",
                        default_sound: !is_off_topic,
                        default_vibrate_timings: !is_off_topic,
                        tag: Some(format!("room-{class_code}-{channel_kind}")),
                    },
                },
            },
        };

        self.dispatch_payload(&payload, kind).await
    }

    pub async fn send_social_notification(
        &self,
        fcm_token: &str,
        title: &str,
        body: &str,
        kind: &str,
        actor_id: i64,
        actor_label: &str,
    ) -> Result<FcmDeliveryOutcome, ServerError> {
        let (channel_id, tag) = social_android_channel_and_tag(kind, actor_id);
        let payload = FcmGenericSendRequest {
            message: FcmGenericMessage {
                token: fcm_token,
                notification: FcmNotification {
                    title: title.to_string(),
                    body: body.to_string(),
                },
                data: FcmSocialData {
                    kind,
                    actor_id: actor_id.to_string(),
                    actor_label,
                },
                android: FcmAndroidConfig {
                    priority: "high",
                    ttl: "3600s",
                    notification: FcmAndroidNotification {
                        channel_id,
                        notification_priority: "PRIORITY_HIGH",
                        visibility: "PUBLIC",
                        default_sound: true,
                        default_vibrate_timings: true,
                        tag: Some(tag),
                    },
                },
            },
        };

        self.dispatch_payload(&payload, kind).await
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
