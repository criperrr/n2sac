use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use nsac_core::filters::{
    BooleanFilter, NumberFilter, QueryFilter, StringFilter, filter_query, normalize_text,
};
use nsac_core::models::{AllYearsResponse, RecoveryStatusCode, YearInfo};
use nsac_core::sentinel::compute_class_grade_hash;
use reqwest::Client;
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::{FromRow, PgPool};
use tokio::task::JoinHandle;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;
use zeroize::Zeroizing;

use crate::db::NewGradePostingEvent;
use crate::db::accounts::normalize_class_code;
use crate::db::snapshots::compute_strong_etag;
use crate::error::ServerError;
use crate::fcm::FcmClient;
use crate::http::nsac_routes::canonicalize_boletim;
use crate::sentinel::poll_due_classes_once;
use crate::state::{AppState, CachedBoletim};

const DISCORD_API_BASE: &str = "https://discord.com/api/v10";
const EPHEMERAL_FLAG: u64 = 64;

const CTI_SUBJECT_SUGGESTIONS: &[&str] = &[
    "Matemática",
    "Física",
    "Química",
    "Biologia",
    "História",
    "Geografia",
    "Filosofia",
    "Sociologia",
    "Língua Portuguesa e Literatura",
    "Inglês Técnico",
    "Análise e Projeto de Sistemas",
    "Aplicativos II",
    "Gestão de Negócios II",
    "Linguagem de Programação IV - C#",
    "Linguagem de Programação V - PHP",
    "Linguagem de Programação VI - Java",
    "Programação Orientada a Objetos",
    "Banco de Dados",
    "Redes de Computadores",
    "Sistemas Operacionais",
];

#[derive(Debug, Clone, FromRow)]
pub struct DiscordChannelSubscription {
    pub id: i64,
    pub guild_id: Option<String>,
    pub channel_id: String,
    pub class_code: String,
    pub mention_mode: String,
    pub configured_by_discord_id: String,
    pub mirror_announcements: bool,
}

pub async fn is_dm_notification_enabled(
    pool: &PgPool,
    account_id: i64,
) -> Result<bool, ServerError> {
    let enabled = sqlx::query_scalar::<_, bool>(
        "SELECT enabled FROM discord_dm_preferences WHERE account_id = $1",
    )
    .bind(account_id)
    .fetch_optional(pool)
    .await?;
    Ok(enabled.unwrap_or(false))
}

pub async fn set_dm_notification_enabled(
    pool: &PgPool,
    account_id: i64,
    discord_id: &str,
    enabled: bool,
) -> Result<bool, ServerError> {
    sqlx::query(
        r#"
        INSERT INTO discord_dm_preferences (account_id, discord_id, enabled, updated_at)
        VALUES ($1, $2, $3, NOW())
        ON CONFLICT (account_id) DO UPDATE SET
            discord_id = EXCLUDED.discord_id,
            enabled = EXCLUDED.enabled,
            updated_at = NOW()
        "#,
    )
    .bind(account_id)
    .bind(discord_id)
    .bind(enabled)
    .execute(pool)
    .await?;
    Ok(enabled)
}

pub async fn list_channel_subscriptions(
    pool: &PgPool,
    channel_id: &str,
) -> Result<Vec<DiscordChannelSubscription>, ServerError> {
    let rows = sqlx::query_as::<_, DiscordChannelSubscription>(
        r#"
        SELECT id, guild_id, channel_id, class_code, mention_mode, configured_by_discord_id, mirror_announcements
        FROM discord_channel_subscriptions
        WHERE channel_id = $1
        ORDER BY class_code ASC
        "#,
    )
    .bind(channel_id)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

pub async fn list_subscriptions_for_class(
    pool: &PgPool,
    class_code: &str,
) -> Result<Vec<DiscordChannelSubscription>, ServerError> {
    let rows = sqlx::query_as::<_, DiscordChannelSubscription>(
        r#"
        SELECT id, guild_id, channel_id, class_code, mention_mode, configured_by_discord_id, mirror_announcements
        FROM discord_channel_subscriptions
        WHERE UPPER(class_code) = UPPER($1)
        ORDER BY id ASC
        "#,
    )
    .bind(class_code)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

pub async fn set_channel_mirror_announcements(
    pool: &PgPool,
    channel_id: &str,
    enabled: bool,
) -> Result<u64, ServerError> {
    let res = sqlx::query(
        "UPDATE discord_channel_subscriptions SET mirror_announcements = $2, updated_at = NOW() WHERE channel_id = $1",
    )
    .bind(channel_id)
    .bind(enabled)
    .execute(pool)
    .await?;
    Ok(res.rows_affected())
}

pub async fn upsert_channel_subscription(
    pool: &PgPool,
    guild_id: Option<&str>,
    channel_id: &str,
    class_code: &str,
    mention_mode: &str,
    discord_user_id: &str,
) -> Result<DiscordChannelSubscription, ServerError> {
    // Strict limit: at most 1 announcement channel per Discord server (guild_id).
    // Configuring announcements in a channel replaces any previous channel in that same server.
    if let Some(gid) = guild_id.map(str::trim).filter(|s| !s.is_empty()) {
        sqlx::query(
            "DELETE FROM discord_channel_subscriptions WHERE guild_id = $1 AND channel_id <> $2",
        )
        .bind(gid)
        .bind(channel_id)
        .execute(pool)
        .await?;
    }

    let row = sqlx::query_as::<_, DiscordChannelSubscription>(
        r#"
        INSERT INTO discord_channel_subscriptions (
            guild_id,
            channel_id,
            class_code,
            mention_mode,
            configured_by_discord_id,
            updated_at
        )
        VALUES ($1, $2, UPPER($3), $4, $5, NOW())
        ON CONFLICT (channel_id, class_code) DO UPDATE SET
            guild_id = COALESCE(EXCLUDED.guild_id, discord_channel_subscriptions.guild_id),
            mention_mode = EXCLUDED.mention_mode,
            configured_by_discord_id = EXCLUDED.configured_by_discord_id,
            updated_at = NOW()
        RETURNING id, guild_id, channel_id, class_code, mention_mode, configured_by_discord_id, mirror_announcements
        "#,
    )
    .bind(guild_id)
    .bind(channel_id)
    .bind(class_code.trim())
    .bind(mention_mode)
    .bind(discord_user_id)
    .fetch_one(pool)
    .await?;
    Ok(row)
}

pub async fn remove_channel_subscription(
    pool: &PgPool,
    channel_id: &str,
    class_code: Option<&str>,
) -> Result<u64, ServerError> {
    let res = if let Some(cls) = class_code {
        sqlx::query(
            "DELETE FROM discord_channel_subscriptions WHERE channel_id = $1 AND UPPER(class_code) = UPPER($2)",
        )
        .bind(channel_id)
        .bind(cls)
        .execute(pool)
        .await?
    } else {
        sqlx::query("DELETE FROM discord_channel_subscriptions WHERE channel_id = $1")
            .bind(channel_id)
            .execute(pool)
            .await?
    };
    Ok(res.rows_affected())
}

/// Mirrors an official Class Representative announcement to Discord channels that explicitly
/// opted in (`mirror_announcements = TRUE`) for `class_code`. Never forced on servers.
#[allow(clippy::too_many_arguments)]
pub async fn broadcast_discord_class_announcement(
    pool: &PgPool,
    class_code: &str,
    author_label: &str,
    category: &str,
    title: &str,
    body_text: &str,
    event_date: Option<&str>,
    attachment_jpeg: Option<Vec<u8>>,
) {
    let Some(cfg) = DiscordBotConfig::from_env() else {
        return;
    };
    if !cfg.enabled {
        return;
    }

    let subs: Vec<DiscordChannelSubscription> = match list_subscriptions_for_class(pool, class_code).await {
        Ok(list) => list
            .into_iter()
            .filter(|s| s.mirror_announcements)
            .collect(),
        Err(_) => return,
    };
    if subs.is_empty() {
        return;
    }

    let Ok(http) = Client::builder()
        .timeout(Duration::from_secs(12))
        .user_agent(concat!(
            "DiscordBot (https://github.com/criperrr/ultimate-gemilona, ",
            env!("CARGO_PKG_VERSION"),
            ")"
        ))
        .build()
    else {
        return;
    };

    let auth_header = format!("Bot {}", cfg.bot_token.as_str());
    let category_label = match category {
        "exam" => "Prova",
        "assignment" => "Trabalho",
        "schedule" => "Horário / Sala",
        "event" => "Evento",
        _ => "Aviso Geral",
    };

    let embed_title = if title.trim().is_empty() {
        format!("📢 Anúncio da Turma {class_code} ({category_label})")
    } else {
        format!("📢 [{class_code}] {}", title.trim())
    };
    let mut description = body_text.trim().to_string();
    if let Some(date) = event_date {
        if !description.is_empty() {
            description.push_str("\n\n");
        }
        description.push_str(&format!("📅 **Data:** `{date}`"));
    }

    let mut embed = json!({
        "title": embed_title,
        "description": description,
        "color": 0xF59E0B,
        "footer": {
            "text": format!("Representante de Classe: {author_label} • {category_label} • n²sac")
        }
    });
    if attachment_jpeg.is_some() {
        embed["image"] = json!({ "url": "attachment://anuncio.jpg" });
    }

    for sub in subs {
        let (content_text, allowed_mentions) = match sub.mention_mode.as_str() {
            "everyone" => (
                format!("@everyone Novo anúncio oficial da turma **`{class_code}`**!"),
                json!({ "parse": ["everyone"] }),
            ),
            "here" => (
                format!("@here Novo anúncio oficial da turma **`{class_code}`**!"),
                json!({ "parse": ["everyone"] }),
            ),
            _ => (
                format!("Novo anúncio oficial da turma **`{class_code}`**:"),
                json!({ "parse": [] }),
            ),
        };

        let payload_json = json!({
            "content": content_text,
            "allowed_mentions": allowed_mentions,
            "embeds": [embed],
        });

        let url = format!("{DISCORD_API_BASE}/channels/{}/messages", sub.channel_id);
        if let Some(ref jpeg) = attachment_jpeg {
            let boundary = "----n2sacDiscordAnnBoundary7MA4YWxkTrZu0gW";
            let mut body_bytes = Vec::with_capacity(jpeg.len() + 2048);
            body_bytes.extend_from_slice(
                format!(
                    "--{boundary}\r\nContent-Disposition: form-data; name=\"payload_json\"\r\nContent-Type: application/json\r\n\r\n{}\r\n--{boundary}\r\nContent-Disposition: form-data; name=\"files[0]\"; filename=\"anuncio.jpg\"\r\nContent-Type: image/jpeg\r\n\r\n",
                    payload_json
                )
                .as_bytes(),
            );
            body_bytes.extend_from_slice(jpeg);
            body_bytes.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
            let _ = http
                .post(&url)
                .header("Authorization", &auth_header)
                .header(
                    "Content-Type",
                    format!("multipart/form-data; boundary={boundary}"),
                )
                .body(body_bytes)
                .send()
                .await;
        } else {
            let _ = http
                .post(&url)
                .header("Authorization", &auth_header)
                .json(&payload_json)
                .send()
                .await;
        }
    }
}

/// Broadcasts a new grade announcement to all Discord channels subscribed to `class_code`.
pub async fn broadcast_discord_class_grade_update(
    pool: &PgPool,
    class_code: &str,
    events: &[NewGradePostingEvent],
    latest_boletim: Option<&AllYearsResponse>,
) {
    let Some(cfg) = DiscordBotConfig::from_env() else {
        return;
    };
    if !cfg.enabled {
        return;
    }

    let subs = match list_subscriptions_for_class(pool, class_code).await {
        Ok(s) if !s.is_empty() => s,
        _ => return,
    };

    let Ok(http) = Client::builder()
        .timeout(Duration::from_secs(12))
        .user_agent(concat!("DiscordBot (https://github.com/criperrr/ultimate-gemilona, ", env!("CARGO_PKG_VERSION"), ")"))
        .build()
    else {
        return;
    };

    let auth_header = format!("Bot {}", cfg.bot_token.as_str());
    // PRIVACY INVARIANT: Server announcements ONLY include class-level M.T. (Média da Turma).
    // Never include personal grades (M.B.), student emails, or student identifiers!
    let mut lines = Vec::new();
    for ev in events {
        let mut mt_display = "Média da Turma (M.T.): `lançada`".to_string();
        if let Some(bol) = latest_boletim
            && let Some(yr) = bol.data.iter().find(|y| i32::from(y.year) == ev.school_year)
            && let Some(subj) = yr.grades.iter().find(|g| g.subject_name == ev.subject_name)
            && let Some(bim) = subj
                .bimesters
                .iter()
                .find(|b| i16::from(b.bimester) == ev.bimester)
            && let Some(mt) = bim.class.average_grade.filter(|&v| v > 0.0)
        {
            mt_display = format!("Média da Turma (M.T.): **`{mt:.1}`**");
        }
        lines.push(format!(
            "• **{}** (`{}º Bimestre`) — {}",
            ev.subject_name, ev.bimester, mt_display
        ));
    }
    if lines.is_empty() {
        lines.push(format!("• Médias da turma **`{class_code}`** atualizadas no NSAC."));
    }

    for sub in subs {
        let (content_text, allowed_mentions) = match sub.mention_mode.as_str() {
            "everyone" => (
                format!("@everyone Saiu nota nova no NSAC para a turma **`{class_code}`**!"),
                json!({ "parse": ["everyone"] }),
            ),
            "here" => (
                format!("@here Saiu nota nova no NSAC para a turma **`{class_code}`**!"),
                json!({ "parse": ["everyone"] }),
            ),
            _ => (
                format!("Saiu nota nova no NSAC para a turma **`{class_code}`**!"),
                json!({ "parse": [] }),
            ),
        };

        let payload = json!({
            "content": content_text,
            "allowed_mentions": allowed_mentions,
            "embeds": [{
                "title": format!("NOVAS NOTAS NO NSAC — Turma {} (Média da Sala)", class_code),
                "description": format!(
                    "Novas médias de turma (**`M.T.`**) detectadas para **`{}`**:\n\n{}",
                    class_code,
                    lines.join("\n")
                ),
                "color": 0x2ECC71,
                "footer": {
                    "text": "Apenas médias coletivas da turma (M.T.) são exibidas no servidor • Notas individuais são privadas"
                }
            }],
            "components": [{
                "type": 1,
                "components": [
                    {
                        "type": 2,
                        "style": 3,
                        "label": "Ver Minha Nota Individual (Privado)",
                        "custom_id": "btn_view_boletim_ephemeral"
                    },
                    {
                        "type": 2,
                        "style": 1,
                        "label": format!("Tabela M.T. de {}", class_code),
                        "custom_id": format!("btn_view_class_mt:{class_code}")
                    }
                ]
            }]
        });

        let url = format!("{}/channels/{}/messages", cfg.api_base_url, sub.channel_id);
        let _ = http
            .post(&url)
            .header("Authorization", &auth_header)
            .json(&payload)
            .send()
            .await;
    }
}

/// Sends an individual private Discord DM to an opted-in student with their personal grade (M.B.)
/// for the newly updated subjects/bimesters. Never posts personal data to any server channel.
pub async fn send_discord_personal_grade_dm(
    pool: &PgPool,
    account_id: i64,
    discord_id: &str,
    class_code: &str,
    events: &[NewGradePostingEvent],
    personal_boletim: &AllYearsResponse,
) -> bool {
    let trimmed_discord = discord_id.trim();
    if trimmed_discord.is_empty() {
        return false;
    }
    let Ok(true) = is_dm_notification_enabled(pool, account_id).await else {
        return false;
    };
    let Some(cfg) = DiscordBotConfig::from_env() else {
        return false;
    };
    if !cfg.enabled {
        return false;
    }

    let Ok(http) = Client::builder()
        .timeout(Duration::from_secs(12))
        .user_agent(concat!("DiscordBot (https://github.com/criperrr/ultimate-gemilona, ", env!("CARGO_PKG_VERSION"), ")"))
        .build()
    else {
        return false;
    };

    let auth_header = format!("Bot {}", cfg.bot_token.as_str());

    // 1. Open/fetch private DM channel with this Discord user
    let dm_url = format!("{}/users/@me/channels", cfg.api_base_url);
    let dm_resp = match http
        .post(&dm_url)
        .header("Authorization", &auth_header)
        .json(&json!({ "recipient_id": trimmed_discord }))
        .send()
        .await
    {
        Ok(r) if r.status().is_success() => r,
        _ => return false,
    };
    let Ok(dm_json) = dm_resp.json::<Value>().await else {
        return false;
    };
    let Some(dm_channel_id) = dm_json.get("id").and_then(Value::as_str) else {
        return false;
    };

    // 2. Format the student's personal grade (M.B.) vs class average (M.T.) for each updated subject
    let mut personal_lines = Vec::new();
    for ev in events {
        if let Some(yr) = personal_boletim
            .data
            .iter()
            .find(|y| i32::from(y.year) == ev.school_year)
            && let Some(subj) = yr.grades.iter().find(|g| g.subject_name == ev.subject_name)
            && let Some(bim) = subj
                .bimesters
                .iter()
                .find(|b| i16::from(b.bimester) == ev.bimester)
        {
            let mb = format_cell_plain(bim.personal.grade, bim.personal.recovery_code);
            let mt = format_cell_plain(bim.class.average_grade, None);
            let diff = match (bim.personal.grade, bim.class.average_grade) {
                (Some(p), Some(c)) if p > 0.0 && c > 0.0 => format!(" (`{:+.1}` vs sala)", p - c),
                _ => String::new(),
            };
            let rec_badge = match bim.personal.recovery_code {
                Some(RecoveryStatusCode::Sat) => " • Rec: `SAT`",
                Some(RecoveryStatusCode::Ins) => " • Rec: `INS`",
                Some(RecoveryStatusCode::Nc) => " • Rec: `NC`",
                _ => "",
            };
            personal_lines.push(format!(
                "• **{}** (`{}º Bimestre`): Sua Nota **`{}`**{} • Média da Sala: `{}` • Faltas: `{}`{}",
                ev.subject_name,
                ev.bimester,
                mb,
                diff,
                mt,
                bim.personal.absences,
                rec_badge
            ));
        }
    }

    if personal_lines.is_empty()
        && let Some(yr) = personal_boletim.data.last()
    {
        for subj in yr.grades.iter().take(6) {
            if let Some(bim) = subj
                .bimesters
                .iter()
                .rev()
                .find(|b| b.personal.grade.is_some_and(|g| g > 0.0))
            {
                let mb = format_cell_plain(bim.personal.grade, bim.personal.recovery_code);
                let mt = format_cell_plain(bim.class.average_grade, None);
                personal_lines.push(format!(
                    "• **{}** (`{}ºB`): Sua Nota **`{}`** • Sala: `{}` • Faltas: `{}`",
                    subj.subject_name, bim.bimester, mb, mt, bim.personal.absences
                ));
            }
        }
    }

    let msg_url = format!("{}/channels/{}/messages", cfg.api_base_url, dm_channel_id);
    let payload = json!({
        "embeds": [{
            "title": format!("Sua Nota Individual Saiu no NSAC — Turma {}", class_code),
            "description": format!(
                "Novas notas foram lançadas no NSAC Online. Confira suas notas individuais (**`M.B.`**):\n\n{}",
                personal_lines.join("\n")
            ),
            "color": 0x2ECC71,
            "footer": {
                "text": "Mensagem privada individual • Suas notas nunca são exibidas em canais públicos"
            }
        }],
        "components": [{
            "type": 1,
            "components": [
                {
                    "type": 2,
                    "style": 3,
                    "label": "Abrir Meu Boletim Completo",
                    "custom_id": "btn_view_boletim_ephemeral"
                },
                {
                    "type": 2,
                    "style": 2,
                    "label": "Configurar Alertas DM",
                    "custom_id": "btn_channel_config"
                }
            ]
        }]
    });

    match http
        .post(&msg_url)
        .header("Authorization", &auth_header)
        .json(&payload)
        .send()
        .await
    {
        Ok(r) => r.status().is_success(),
        Err(_) => false,
    }
}

#[derive(Clone)]
pub struct DiscordBotConfig {
    pub enabled: bool,
    pub deploy_commands_on_start: bool,
    pub bot_token: Zeroizing<String>,
    pub application_id: String,
    pub api_base_url: String,
}

impl DiscordBotConfig {
    pub fn from_env() -> Option<Self> {
        let token = std::env::var("DISCORD_BOT_TOKEN")
            .or_else(|_| std::env::var("DISCORD_TOKEN"))
            .ok()?;
        let trimmed_tok = token.trim();
        if trimmed_tok.is_empty() {
            return None;
        }
        let app_id = std::env::var("DISCORD_APPLICATION_ID")
            .or_else(|_| std::env::var("DISCORD_CLIENT_ID"))
            .unwrap_or_default();
        let enabled = std::env::var("DISCORD_ENABLED")
            .map(|v| !matches!(v.trim().to_ascii_lowercase().as_str(), "false" | "0" | "no"))
            .unwrap_or(true);
        let deploy_commands_on_start = std::env::var("DISCORD_DEPLOY_COMMANDS")
            .map(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "true" | "1" | "yes"))
            .unwrap_or(false);
        let api_base_url = std::env::var("DISCORD_API_BASE_URL")
            .unwrap_or_else(|_| DISCORD_API_BASE.to_string());

        Some(Self {
            enabled,
            deploy_commands_on_start,
            bot_token: Zeroizing::new(trimmed_tok.to_string()),
            application_id: app_id.trim().to_string(),
            api_base_url: api_base_url.trim_end_matches('/').to_string(),
        })
    }
}

#[derive(Clone)]
pub struct DiscordBot {
    state: AppState,
    fcm: FcmClient,
    config: DiscordBotConfig,
    http: Client,
}

#[derive(Debug, Deserialize)]
struct GatewayBotResponse {
    url: String,
}

pub fn spawn_discord_bot(state: AppState, fcm: FcmClient) -> Option<JoinHandle<()>> {
    let config = DiscordBotConfig::from_env()?;
    if !config.enabled {
        tracing::info!("Discord bot disabled via DISCORD_ENABLED=false");
        return None;
    }

    let bot = match DiscordBot::new(state, fcm, config) {
        Ok(b) => b,
        Err(err) => {
            tracing::error!(error = %err, "Failed to initialize DiscordBot HTTP client");
            return None;
        }
    };

    Some(tokio::spawn(async move {
        bot.run_forever().await;
    }))
}

impl DiscordBot {
    pub fn new(
        state: AppState,
        fcm: FcmClient,
        config: DiscordBotConfig,
    ) -> Result<Self, ServerError> {
        let http = Client::builder()
            .timeout(Duration::from_secs(15))
            .user_agent(concat!("DiscordBot (https://github.com/criperrr/ultimate-gemilona, ", env!("CARGO_PKG_VERSION"), ")"))
            .build()
            .map_err(|e| ServerError::internal(format!("Failed to build Discord HTTP client: {e}")))?;

        Ok(Self {
            state,
            fcm,
            config,
            http,
        })
    }

    pub fn build_command_definitions() -> Value {
        json!([
            {
                "name": "login",
                "description": "Abre o formulário seguro (Modal) para entrar com e-mail e senha do NSAC Online"
            },
            {
                "name": "notas",
                "description": "Exibe seu boletim NSAC, médias de outras salas e configura anúncios automáticos no canal",
                "options": [
                    {
                        "name": "bimestre",
                        "description": "Ver um bimestre detalhado (Você vs Sala + Faltas + Rec)",
                        "type": 4,
                        "required": false,
                        "choices": [
                            { "name": "1º Bimestre", "value": 1 },
                            { "name": "2º Bimestre", "value": 2 },
                            { "name": "3º Bimestre", "value": 3 },
                            { "name": "4º Bimestre", "value": 4 }
                        ]
                    },
                    {
                        "name": "materia",
                        "description": "Filtrar por disciplina específica (com autocomplete)",
                        "type": 3,
                        "required": false,
                        "autocomplete": true
                    },
                    {
                        "name": "ano",
                        "description": "Ano do boletim (ex: 2026, 2025, 2024)",
                        "type": 4,
                        "required": false
                    },
                    {
                        "name": "visao",
                        "description": "Alternar entre suas notas (M.B.), médias da sala (M.T.) ou recuperações",
                        "type": 3,
                        "required": false,
                        "choices": [
                            { "name": "Minhas Notas (M.B.)", "value": "completa" },
                            { "name": "Médias da Sala (M.T.)", "value": "turma" },
                            { "name": "Apenas Recuperações (SAT / INS / NC)", "value": "recuperacao" }
                        ]
                    },
                    {
                        "name": "publico",
                        "description": "Se true, envia visível para o canal (padrão: false / só para você)",
                        "type": 5,
                        "required": false
                    }
                ]
            },
            {
                "name": "heuristica",
                "description": "Painel da Heurística Sentinel e configuração de anúncios automáticos no canal",
                "options": [
                    {
                        "name": "estimular",
                        "description": "Se true, estimula a heurística e verifica novidades da sua turma agora",
                        "type": 5,
                        "required": false
                    }
                ]
            }
        ])
    }

    pub async fn register_commands(&self) -> Result<(), ServerError> {
        let commands = Self::build_command_definitions();
        let auth_header = format!("Bot {}", self.config.bot_token.as_str());

        let global_url = format!(
            "{}/applications/{}/commands",
            self.config.api_base_url, self.config.application_id
        );
        let global_res = self
            .http
            .put(&global_url)
            .header("Authorization", &auth_header)
            .json(&commands)
            .send()
            .await;

        match global_res {
            Ok(resp) if resp.status().is_success() => {
                tracing::info!("Registered global Discord slash commands (/login, /notas, /heuristica)");
            }
            Ok(resp) => {
                let status = resp.status().as_u16();
                let body = resp.text().await.unwrap_or_default();
                tracing::warn!(status, body = %body, "Non-200 registering global Discord commands");
            }
            Err(err) => {
                tracing::warn!(error = %err, "Failed to register global Discord commands");
            }
        }

        Ok(())
    }

    pub async fn run_forever(&self) {
        if self.config.deploy_commands_on_start {
            let _ = self.register_commands().await;
        }

        loop {
            if let Err(err) = self.connect_and_listen().await {
                tracing::warn!(error = %err, "Discord Gateway disconnected; reconnecting in 5s");
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
    }

    async fn connect_and_listen(&self) -> Result<(), ServerError> {
        let auth_header = format!("Bot {}", self.config.bot_token.as_str());
        let gw_url = format!("{}/gateway/bot", self.config.api_base_url);
        let gw_resp = self
            .http
            .get(&gw_url)
            .header("Authorization", &auth_header)
            .send()
            .await
            .map_err(|e| ServerError::internal(format!("Gateway URL fetch failed: {e}")))?;

        let gw_info: GatewayBotResponse = gw_resp
            .json()
            .await
            .map_err(|e| ServerError::internal(format!("Gateway JSON decode failed: {e}")))?;

        let ws_url = format!("{}/?v=10&encoding=json", gw_info.url.trim_end_matches('/'));
        let (ws_stream, _) = connect_async(&ws_url)
            .await
            .map_err(|e| ServerError::internal(format!("Gateway WebSocket connect failed: {e}")))?;

        let (mut write, mut read) = ws_stream.split();

        let hello_msg = read
            .next()
            .await
            .ok_or_else(|| ServerError::internal("Gateway closed before HELLO"))?
            .map_err(|e| ServerError::internal(format!("Gateway read error: {e}")))?;

        let hello_json: Value = serde_json::from_str(hello_msg.to_text().unwrap_or("{}"))?;
        let heartbeat_ms = hello_json
            .get("d")
            .and_then(|d| d.get("heartbeat_interval"))
            .and_then(Value::as_u64)
            .unwrap_or(41_250);

        let identify = json!({
            "op": 2,
            "d": {
                "token": self.config.bot_token.as_str(),
                "intents": 1,
                "properties": {
                    "os": "linux",
                    "browser": "nsac-server-rust",
                    "device": "nsac-server-rust"
                },
                "presence": {
                    "status": "online",
                    "activities": [{
                        "name": "NSAC Online • /notas | /heuristica",
                        "type": 3
                    }]
                }
            }
        });

        write
            .send(Message::Text(identify.to_string().into()))
            .await
            .map_err(|e| ServerError::internal(format!("Gateway IDENTIFY send failed: {e}")))?;

        let mut heartbeat_timer = tokio::time::interval(Duration::from_millis(heartbeat_ms));
        heartbeat_timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut last_seq: Option<u64> = None;

        loop {
            tokio::select! {
                _ = heartbeat_timer.tick() => {
                    let hb = json!({ "op": 1, "d": last_seq });
                    write
                        .send(Message::Text(hb.to_string().into()))
                        .await
                        .map_err(|e| ServerError::internal(format!("Heartbeat send failed: {e}")))?;
                }
                maybe_msg = read.next() => {
                    let Some(msg_res) = maybe_msg else {
                        return Err(ServerError::internal("Discord Gateway stream ended"));
                    };
                    let msg = msg_res.map_err(|e| ServerError::internal(format!("WS message error: {e}")))?;
                    match msg {
                        Message::Text(text) => {
                            let payload: Value = match serde_json::from_str(&text) {
                                Ok(v) => v,
                                Err(_) => continue,
                            };
                            if let Some(s) = payload.get("s").and_then(Value::as_u64) {
                                last_seq = Some(s);
                            }
                            let op = payload.get("op").and_then(Value::as_u64).unwrap_or(0);
                            if op == 1 {
                                let hb = json!({ "op": 1, "d": last_seq });
                                let _ = write.send(Message::Text(hb.to_string().into())).await;
                            } else if op == 7 || op == 9 {
                                return Err(ServerError::internal("Gateway requested reconnect/invalid session"));
                            } else if op == 0 {
                                let event_type = payload.get("t").and_then(Value::as_str).unwrap_or("");
                                if event_type == "READY" {
                                    tracing::info!("Discord Gateway READY — gemilona bot is live");
                                } else if event_type == "INTERACTION_CREATE"
                                    && let Some(d) = payload.get("d").cloned()
                                {
                                    let bot_clone = self.clone();
                                    tokio::spawn(async move {
                                        if let Err(err) = bot_clone.handle_interaction(d).await {
                                            tracing::warn!(error = %err, "Error handling Discord interaction");
                                        }
                                    });
                                }
                            }
                        }
                        Message::Close(_) => {
                            return Err(ServerError::internal("Discord Gateway closed connection"));
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    pub async fn handle_interaction(&self, interaction: Value) -> Result<(), ServerError> {
        let interaction_id = interaction
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let interaction_token = interaction
            .get("token")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let interaction_type = interaction
            .get("type")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let discord_user_id = extract_discord_user_id(&interaction);
        let channel_id = interaction
            .get("channel_id")
            .and_then(Value::as_str)
            .map(ToString::to_string);
        let guild_id = interaction
            .get("guild_id")
            .and_then(Value::as_str)
            .map(ToString::to_string);

        match interaction_type {
            2 => {
                self.handle_application_command(
                    &interaction_id,
                    &interaction_token,
                    &discord_user_id,
                    channel_id.as_deref(),
                    guild_id.as_deref(),
                    &interaction,
                )
                .await
            }
            3 => {
                self.handle_message_component(
                    &interaction_id,
                    &interaction_token,
                    &discord_user_id,
                    channel_id.as_deref(),
                    guild_id.as_deref(),
                    &interaction,
                )
                .await
            }
            4 => {
                self.handle_autocomplete(
                    &interaction_id,
                    &interaction_token,
                    &discord_user_id,
                    &interaction,
                )
                .await
            }
            5 => {
                self.handle_modal_submit(
                    &interaction_id,
                    &interaction_token,
                    &discord_user_id,
                    channel_id.as_deref(),
                    guild_id.as_deref(),
                    &interaction,
                )
                .await
            }
            _ => Ok(()),
        }
    }

    async fn send_interaction_callback(
        &self,
        interaction_id: &str,
        interaction_token: &str,
        body: &Value,
    ) -> Result<(), ServerError> {
        let url = format!(
            "{}/interactions/{}/{}/callback",
            self.config.api_base_url, interaction_id, interaction_token
        );
        let resp = self.http.post(&url).json(body).send().await.map_err(|e| {
            ServerError::internal(format!("Failed sending interaction callback: {e}"))
        })?;
        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let detail = resp.text().await.unwrap_or_default();
            tracing::warn!(status, detail = %detail, "Discord interaction callback returned non-2xx");
        }
        Ok(())
    }

    async fn edit_original_interaction_response(
        &self,
        interaction_token: &str,
        body: &Value,
    ) -> Result<(), ServerError> {
        let url = format!(
            "{}/webhooks/{}/{}/messages/@original",
            self.config.api_base_url, self.config.application_id, interaction_token
        );
        let resp = self
            .http
            .patch(&url)
            .json(body)
            .send()
            .await
            .map_err(|e| ServerError::internal(format!("Failed editing interaction response: {e}")))?;
        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let detail = resp.text().await.unwrap_or_default();
            tracing::warn!(status, detail = %detail, "Discord edit @original returned non-2xx");
        }
        Ok(())
    }

    pub fn build_login_modal() -> Value {
        json!({
            "type": 9,
            "data": {
                "custom_id": "modal_nsac_login",
                "title": "Entrar no NSAC Online (CTI / UNESP)",
                "components": [
                    {
                        "type": 1,
                        "components": [{
                            "type": 4,
                            "custom_id": "nsac_email",
                            "label": "E-mail Institucional do NSAC",
                            "style": 1,
                            "placeholder": "nome.sobrenome@unesp.br",
                            "required": true,
                            "min_length": 5,
                            "max_length": 120
                        }]
                    },
                    {
                        "type": 1,
                        "components": [{
                            "type": 4,
                            "custom_id": "nsac_password",
                            "label": "Senha do NSAC (Criptografada AES-256-GCM)",
                            "style": 1,
                            "placeholder": "Sua senha do portal NSAC",
                            "required": true,
                            "min_length": 1,
                            "max_length": 120
                        }]
                    }
                ]
            }
        })
    }

    pub fn build_filter_modal() -> Value {
        json!({
            "type": 9,
            "data": {
                "custom_id": "modal_nsac_filter",
                "title": "Filtrar Disciplinas e Notas",
                "components": [
                    {
                        "type": 1,
                        "components": [{
                            "type": 4,
                            "custom_id": "subject_contains",
                            "label": "Disciplina (busca sem acento, opcional)",
                            "style": 1,
                            "placeholder": "Ex: Matematica, Fisica, Java, LP",
                            "required": false,
                            "max_length": 60
                        }]
                    },
                    {
                        "type": 1,
                        "components": [{
                            "type": 4,
                            "custom_id": "bimester",
                            "label": "Bimestre (1 a 4, opcional)",
                            "style": 1,
                            "placeholder": "Ex: 1, 2, 3 ou 4 (vazio = todos)",
                            "required": false,
                            "max_length": 5
                        }]
                    },
                    {
                        "type": 1,
                        "components": [{
                            "type": 4,
                            "custom_id": "min_personal_grade",
                            "label": "Sua Nota M.B. (ex: >=6 ou <6)",
                            "style": 1,
                            "placeholder": "Ex: >=6, <6, 7.5 (vazio = todas)",
                            "required": false,
                            "max_length": 15
                        }]
                    },
                    {
                        "type": 1,
                        "components": [{
                            "type": 4,
                            "custom_id": "min_class_avg",
                            "label": "Média da Sala M.T. (ex: >=7 ou <6)",
                            "style": 1,
                            "placeholder": "Ex: >=7.0 ou <6.0 (vazio = todas)",
                            "required": false,
                            "max_length": 15
                        }]
                    },
                    {
                        "type": 1,
                        "components": [{
                            "type": 4,
                            "custom_id": "recovery_code",
                            "label": "Recuperação (SAT, INS, NC ou SIM)",
                            "style": 1,
                            "placeholder": "Ex: SAT, INS, NC ou SIM",
                            "required": false,
                            "max_length": 20
                        }]
                    }
                ]
            }
        })
    }

    #[allow(clippy::too_many_arguments)]
    async fn handle_application_command(
        &self,
        interaction_id: &str,
        interaction_token: &str,
        discord_user_id: &str,
        channel_id: Option<&str>,
        guild_id: Option<&str>,
        interaction: &Value,
    ) -> Result<(), ServerError> {
        let data = interaction.get("data").cloned().unwrap_or_default();
        let cmd_name = data.get("name").and_then(Value::as_str).unwrap_or("");
        let options = data
            .get("options")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();

        match cmd_name {
            "login" => {
                let modal = Self::build_login_modal();
                self.send_interaction_callback(interaction_id, interaction_token, &modal)
                    .await
            }
            "notas" => {
                let is_public = get_opt_bool(&options, "publico").unwrap_or(false);
                let flags = if is_public { 0 } else { EPHEMERAL_FLAG };

                self.send_interaction_callback(
                    interaction_id,
                    interaction_token,
                    &json!({
                        "type": 5,
                        "data": { "flags": flags }
                    }),
                )
                .await?;

                let bimester = get_opt_i64(&options, "bimestre").and_then(|v| u8::try_from(v).ok());
                let materia = get_opt_str(&options, "materia");
                let ano = get_opt_i64(&options, "ano").and_then(|v| u16::try_from(v).ok());
                let visao = get_opt_str(&options, "visao").unwrap_or_else(|| "completa".to_string());

                let mut qf = QueryFilter::default();
                if let Some(b) = bimester {
                    qf.target_bimester = Some(NumberFilter {
                        eq: Some(vec![f64::from(b)]),
                        ..Default::default()
                    });
                }
                if let Some(ref subj) = materia {
                    qf.subject_name = Some(StringFilter {
                        contains: Some(vec![normalize_text(subj)]),
                        ..Default::default()
                    });
                }
                if let Some(y) = ano
                    && (1..=10).contains(&y)
                {
                    qf.school_year = Some(NumberFilter {
                        eq: Some(vec![f64::from(y)]),
                        ..Default::default()
                    });
                }
                if visao == "recuperacao" {
                    qf.is_recovery = Some(BooleanFilter {
                        eq: Some(true),
                        ..Default::default()
                    });
                }

                let payload = self
                    .render_notas_view(
                        discord_user_id,
                        channel_id,
                        guild_id,
                        &qf,
                        ano.filter(|y| *y >= 2000),
                        bimester,
                        &visao,
                        false,
                        None,
                    )
                    .await;
                self.edit_original_interaction_response(interaction_token, &payload)
                    .await
            }
            "heuristica" => {
                self.send_interaction_callback(
                    interaction_id,
                    interaction_token,
                    &json!({
                        "type": 5,
                        "data": { "flags": EPHEMERAL_FLAG }
                    }),
                )
                .await?;

                let estimular = get_opt_bool(&options, "estimular").unwrap_or(false);
                let payload = self
                    .render_heuristica_view(discord_user_id, channel_id, guild_id, estimular)
                    .await;
                self.edit_original_interaction_response(interaction_token, &payload)
                    .await
            }
            _ => Ok(()),
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn handle_message_component(
        &self,
        interaction_id: &str,
        interaction_token: &str,
        discord_user_id: &str,
        channel_id: Option<&str>,
        guild_id: Option<&str>,
        interaction: &Value,
    ) -> Result<(), ServerError> {
        let data = interaction.get("data").cloned().unwrap_or_default();
        let custom_id = data.get("custom_id").and_then(Value::as_str).unwrap_or("");
        let values: Vec<String> = data
            .get("values")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(Value::as_str)
                    .map(ToString::to_string)
                    .collect()
            })
            .unwrap_or_default();

        if custom_id == "btn_open_login_modal" {
            let modal = Self::build_login_modal();
            return self
                .send_interaction_callback(interaction_id, interaction_token, &modal)
                .await;
        }
        if custom_id == "btn_open_filter_modal" {
            let modal = Self::build_filter_modal();
            return self
                .send_interaction_callback(interaction_id, interaction_token, &modal)
                .await;
        }

        // Buttons clicked on a public channel announcement create a new ephemeral response (type 5)
        let is_new_ephemeral_from_public = custom_id == "btn_view_boletim_ephemeral"
            || custom_id.starts_with("btn_view_class_mt:");

        let callback_type = if is_new_ephemeral_from_public { 5 } else { 6 };
        self.send_interaction_callback(
            interaction_id,
            interaction_token,
            &json!({
                "type": callback_type,
                "data": { "flags": EPHEMERAL_FLAG }
            }),
        )
        .await?;

        if let Some(other_cls) = custom_id.strip_prefix("btn_view_class_mt:") {
            let payload = self
                .render_notas_view(
                    discord_user_id,
                    channel_id,
                    guild_id,
                    &QueryFilter::default(),
                    None,
                    None,
                    "turma",
                    false,
                    Some(other_cls),
                )
                .await;
            return self
                .edit_original_interaction_response(interaction_token, &payload)
                .await;
        }

        if custom_id == "select_other_class_mt" {
            let chosen = values.first().map(String::as_str).unwrap_or("mine");
            let override_cls = if chosen == "mine" { None } else { Some(chosen) };
            let visao = if override_cls.is_some() {
                "turma"
            } else {
                "completa"
            };
            let payload = self
                .render_notas_view(
                    discord_user_id,
                    channel_id,
                    guild_id,
                    &QueryFilter::default(),
                    None,
                    None,
                    visao,
                    false,
                    override_cls,
                )
                .await;
            return self
                .edit_original_interaction_response(interaction_token, &payload)
                .await;
        }

        // Channel announcement & personal DM notification configuration hub!
        if custom_id == "btn_channel_config"
            || custom_id == "btn_subscribe_default"
            || custom_id == "btn_unsubscribe_all_here"
            || custom_id == "btn_test_announcement"
            || custom_id == "btn_toggle_dm_notify"
            || custom_id == "btn_test_dm_notify"
            || custom_id.starts_with("btn_mention_mode:")
            || custom_id == "select_channel_sub_class"
        {
            let payload = self
                .handle_channel_announcement_config(
                    discord_user_id,
                    channel_id,
                    guild_id,
                    custom_id,
                    &values,
                )
                .await;
            return self
                .edit_original_interaction_response(interaction_token, &payload)
                .await;
        }

        if custom_id == "select_bimester" {
            let selected = values.first().map(String::as_str).unwrap_or("all");
            let mut qf = QueryFilter::default();
            let mut bim_opt = None;
            let mut visao = "completa";
            if selected == "rec" {
                qf.is_recovery = Some(BooleanFilter {
                    eq: Some(true),
                    ..Default::default()
                });
                visao = "recuperacao";
            } else if selected == "mt" {
                visao = "turma";
            } else if let Ok(b) = selected.parse::<u8>()
                && (1..=4).contains(&b)
            {
                bim_opt = Some(b);
                qf.target_bimester = Some(NumberFilter {
                    eq: Some(vec![f64::from(b)]),
                    ..Default::default()
                });
            }
            let payload = self
                .render_notas_view(
                    discord_user_id,
                    channel_id,
                    guild_id,
                    &qf,
                    None,
                    bim_opt,
                    visao,
                    false,
                    None,
                )
                .await;
            return self
                .edit_original_interaction_response(interaction_token, &payload)
                .await;
        }

        if custom_id == "select_year" {
            let selected_year = values.first().and_then(|v| v.parse::<u16>().ok());
            let payload = self
                .render_notas_view(
                    discord_user_id,
                    channel_id,
                    guild_id,
                    &QueryFilter::default(),
                    selected_year,
                    None,
                    "completa",
                    false,
                    None,
                )
                .await;
            return self
                .edit_original_interaction_response(interaction_token, &payload)
                .await;
        }

        if custom_id == "btn_force_refresh" {
            let payload = self
                .render_notas_view(
                    discord_user_id,
                    channel_id,
                    guild_id,
                    &QueryFilter::default(),
                    None,
                    None,
                    "completa",
                    true,
                    None,
                )
                .await;
            return self
                .edit_original_interaction_response(interaction_token, &payload)
                .await;
        }

        if custom_id == "btn_heuristic_status" || custom_id == "btn_trigger_sentinel" {
            let payload = self
                .render_heuristica_view(discord_user_id, channel_id, guild_id, true)
                .await;
            return self
                .edit_original_interaction_response(interaction_token, &payload)
                .await;
        }

        let payload = self
            .render_notas_view(
                discord_user_id,
                channel_id,
                guild_id,
                &QueryFilter::default(),
                None,
                None,
                "completa",
                false,
                None,
            )
            .await;
        self.edit_original_interaction_response(interaction_token, &payload)
            .await
    }

    async fn handle_channel_announcement_config(
        &self,
        discord_user_id: &str,
        channel_id: Option<&str>,
        guild_id: Option<&str>,
        action: &str,
        values: &[String],
    ) -> Value {
        let Some(ch_id) = channel_id else {
            return json!({
                "embeds": [{
                    "title": "Canal não identificado",
                    "description": "Execute este comando dentro de um canal de servidor (ou DM) para configurar anúncios automáticos.",
                    "color": 0xE74C3C
                }]
            });
        };

        let user_acct = self
            .state
            .accounts()
            .find_by_discord_id(discord_user_id)
            .await
            .ok()
            .flatten();

        let user_default_class = user_acct
            .as_ref()
            .and_then(|a| a.class_code.clone())
            .unwrap_or_else(|| "INI3".to_string());

        let mut status_note = String::new();

        let is_guild_channel = guild_id.map(str::trim).is_some_and(|g| !g.is_empty());

        // If user is in Private DM (guild_id is None), never register the DM as a server announcement channel!
        // Clean up any accidental DM channel subscription if one was created before.
        if !is_guild_channel {
            let _ = remove_channel_subscription(&self.state.db, ch_id, None).await;
            if action == "btn_channel_config" {
                if let Some(ref acct) = user_acct {
                    let currently_on = is_dm_notification_enabled(&self.state.db, acct.id)
                        .await
                        .unwrap_or(false);
                    if !currently_on {
                        let _ = set_dm_notification_enabled(
                            &self.state.db,
                            acct.id,
                            discord_user_id,
                            true,
                        )
                        .await;
                        status_note = "\n\n**Alerta Individual na DM ATIVADO:** Você receberá sua nota pessoal (`M.B.`) aqui na sua DM assim que sair no NSAC!".to_string();
                    }
                }
            } else if action == "btn_toggle_dm_notify" {
                if let Some(ref acct) = user_acct {
                    let currently_on = is_dm_notification_enabled(&self.state.db, acct.id)
                        .await
                        .unwrap_or(false);
                    let new_state = !currently_on;
                    let _ = set_dm_notification_enabled(
                        &self.state.db,
                        acct.id,
                        discord_user_id,
                        new_state,
                    )
                    .await;
                    status_note = if new_state {
                        "\n\n**Alerta Individual na DM ATIVADO!**".to_string()
                    } else {
                        "\n\n**Alerta Individual na DM DESATIVADO.**".to_string()
                    };
                }
            } else if action == "btn_test_dm_notify"
                && let Some(ref acct) = user_acct
            {
                let _ =
                    set_dm_notification_enabled(&self.state.db, acct.id, discord_user_id, true)
                        .await;
                if let Ok(Some(bol)) = self.state.boletim().get_boletim(acct.id).await {
                    let sent = send_discord_personal_grade_dm(
                        &self.state.db,
                        acct.id,
                        discord_user_id,
                        &user_default_class,
                        &[],
                        &bol,
                    )
                    .await;
                    status_note = if sent {
                        "\n\n**Notificação individual de teste enviada aqui na sua DM!**".to_string()
                    } else {
                        "\n\n**Falha ao enviar DM de teste.**".to_string()
                    };
                }
            }

            let dm_enabled = if let Some(ref acct) = user_acct {
                is_dm_notification_enabled(&self.state.db, acct.id)
                    .await
                    .unwrap_or(false)
            } else {
                false
            };

            return json!({
                "embeds": [{
                    "title": "Alerta Individual na Sua DM (Chat Particular)",
                    "description": format!(
                        "Você está na sua **conversa particular (DM)** com o bot.\n\n\
                        • **Sua Sala Detectada pelo NSAC:** **`{user_default_class}`**\n\
                        • **Receber Minha Nota Individual (`M.B.`) na DM:** **`{}`**\n\n\
                        *(Para configurar anúncios públicos da Média da Turma `M.T.` em um servidor, use o comando `/notas` dentro de um canal do servidor).*{}",
                        if dm_enabled { "ATIVADO" } else { "DESATIVADO" },
                        status_note
                    ),
                    "color": if dm_enabled { 0x2ECC71 } else { 0x5865F2 },
                    "footer": {
                        "text": "Na DM você recebe suas notas individuais (M.B.) com total privacidade"
                    }
                }],
                "components": [{
                    "type": 1,
                    "components": [
                        {
                            "type": 2,
                            "style": if dm_enabled { 4 } else { 3 },
                            "label": if dm_enabled {
                                "Desativar Alerta na Minha DM"
                            } else {
                                "Ativar Alerta na Minha DM"
                            },
                            "custom_id": "btn_toggle_dm_notify"
                        },
                        {
                            "type": 2,
                            "style": 1,
                            "label": "Testar Alerta Individual na DM",
                            "custom_id": "btn_test_dm_notify"
                        },
                        {
                            "type": 2,
                            "style": 2,
                            "label": "Voltar ao Meu Boletim",
                            "custom_id": "btn_view_boletim"
                        }
                    ]
                }]
            });
        }

        // Server channel mode (guild_id is Some):
        let existing_before = list_channel_subscriptions(&self.state.db, ch_id)
            .await
            .unwrap_or_default();

        if (action == "btn_channel_config" && existing_before.is_empty())
            || action == "btn_subscribe_default"
        {
            let _ = upsert_channel_subscription(
                &self.state.db,
                guild_id,
                ch_id,
                &user_default_class,
                "everyone",
                discord_user_id,
            )
            .await;
            status_note = format!(
                "\n\n**Configurado por padrão:** Este canal (`<#{ch_id}>`) agora anuncia automaticamente novas notas da sua sala (**`{user_default_class}`**)!"
            );
        } else if action == "btn_unsubscribe_all_here" {
            let removed = remove_channel_subscription(&self.state.db, ch_id, None)
                .await
                .unwrap_or(0);
            status_note = format!(
                "\n\n**Anúncios desativados neste canal** (`{removed}` inscrição(ões) removida(s))."
            );
        } else if let Some(new_mode) = action.strip_prefix("btn_mention_mode:") {
            let current_subs = list_channel_subscriptions(&self.state.db, ch_id)
                .await
                .unwrap_or_default();
            if current_subs.is_empty() {
                let _ = upsert_channel_subscription(
                    &self.state.db,
                    guild_id,
                    ch_id,
                    &user_default_class,
                    new_mode,
                    discord_user_id,
                )
                .await;
            } else {
                for s in current_subs {
                    let _ = upsert_channel_subscription(
                        &self.state.db,
                        guild_id,
                        ch_id,
                        &s.class_code,
                        new_mode,
                        discord_user_id,
                    )
                    .await;
                }
            }
            status_note = format!(
                "\n\n**Modo de menção atualizado para:** `{}`",
                new_mode.to_ascii_uppercase()
            );
        } else if action == "select_channel_sub_class"
            && let Some(chosen_cls) = values.first()
        {
            let already_has = existing_before
                .iter()
                .any(|s| s.class_code.eq_ignore_ascii_case(chosen_cls));
            if already_has {
                let _ =
                    remove_channel_subscription(&self.state.db, ch_id, Some(chosen_cls)).await;
                status_note = format!(
                    "\n\n**Turma `{chosen_cls}` removida** dos anúncios deste canal."
                );
            } else {
                let mode = existing_before
                    .first()
                    .map(|s| s.mention_mode.as_str())
                    .unwrap_or("everyone");
                let _ = upsert_channel_subscription(
                    &self.state.db,
                    guild_id,
                    ch_id,
                    chosen_cls,
                    mode,
                    discord_user_id,
                )
                .await;
                status_note = format!(
                    "\n\n**Turma `{chosen_cls}` adicionada** aos anúncios automáticos deste canal!"
                );
            }
        } else if action == "btn_test_announcement" {
            let subs = list_channel_subscriptions(&self.state.db, ch_id)
                .await
                .unwrap_or_default();
            let target_cls = subs
                .first()
                .map(|s| s.class_code.clone())
                .unwrap_or_else(|| user_default_class.clone());
            if subs.is_empty() {
                let _ = upsert_channel_subscription(
                    &self.state.db,
                    guild_id,
                    ch_id,
                    &target_cls,
                    "everyone",
                    discord_user_id,
                )
                .await;
            }
            let now = self.state.clock.now();
            let sample_event = NewGradePostingEvent {
                class_code: target_cls.clone(),
                school_year: 2026,
                bimester: 3,
                subject_name: "Matemática (Teste de Anúncio)".to_string(),
                day_of_year: 323,
                month: 11,
                day: 19,
                posted_at: now,
            };
            broadcast_discord_class_grade_update(
                &self.state.db,
                &target_cls,
                &[sample_event],
                None,
            )
            .await;
            status_note = format!(
                "\n\n**Anúncio de teste (M.T.) enviado em `<#{ch_id}>` para a turma `{target_cls}`!**"
            );
        } else if action == "btn_toggle_dm_notify" {
            if let Some(ref acct) = user_acct {
                let currently_on = is_dm_notification_enabled(&self.state.db, acct.id)
                    .await
                    .unwrap_or(false);
                let new_state = !currently_on;
                let _ = set_dm_notification_enabled(
                    &self.state.db,
                    acct.id,
                    discord_user_id,
                    new_state,
                )
                .await;
                status_note = if new_state {
                    "\n\n**Notificação Individual na DM ATIVADA:** Você receberá sua nota pessoal (`M.B.`) em privado na DM assim que sair no NSAC!".to_string()
                } else {
                    "\n\n**Notificação Individual na DM DESATIVADA.**".to_string()
                };
            } else {
                status_note =
                    "\n\n**Vincule sua conta com `/login` primeiro** para ativar alertas individuais na sua DM.".to_string();
            }
        } else if action == "btn_toggle_mirror_announcements" {
            let current_mirror = existing_before.iter().any(|s| s.mirror_announcements);
            let next_mirror = !current_mirror;
            if existing_before.is_empty() {
                let _ = upsert_channel_subscription(
                    &self.state.db,
                    guild_id,
                    ch_id,
                    &user_default_class,
                    "everyone",
                    discord_user_id,
                )
                .await;
            }
            let _ = set_channel_mirror_announcements(&self.state.db, ch_id, next_mirror).await;
            status_note = if next_mirror {
                "\n\n**Espelhamento de Anúncios da Turma ATIVADO:** Avisos publicados pelos Representantes de Classe no app `n²sac` também serão enviados neste canal.".to_string()
            } else {
                "\n\n**Espelhamento de Anúncios da Turma DESATIVADO:** Este canal anunciará apenas notas do NSAC.".to_string()
            };
        } else if action == "btn_test_dm_notify" {
            if let Some(ref acct) = user_acct {
                let _ =
                    set_dm_notification_enabled(&self.state.db, acct.id, discord_user_id, true)
                        .await;
                if let Ok(Some(bol)) = self.state.boletim().get_boletim(acct.id).await {
                    let sent = send_discord_personal_grade_dm(
                        &self.state.db,
                        acct.id,
                        discord_user_id,
                        &user_default_class,
                        &[],
                        &bol,
                    )
                    .await;
                    status_note = if sent {
                        "\n\n**DM individual de teste enviada!** Confira suas mensagens privadas com o bot.".to_string()
                    } else {
                        "\n\n**Não foi possível enviar a DM** (verifique se suas DMs estão abertas no servidor).".to_string()
                    };
                }
            } else {
                status_note = "\n\n**Faça `/login` primeiro** para testar o envio da sua nota individual na DM.".to_string();
            }
        }

        let dm_enabled = if let Some(ref acct) = user_acct {
            is_dm_notification_enabled(&self.state.db, acct.id)
                .await
                .unwrap_or(false)
        } else {
            false
        };

        let current_subs = list_channel_subscriptions(&self.state.db, ch_id)
            .await
            .unwrap_or_default();
        let active_classes_in_db = self
            .state
            .accounts()
            .list_active_classes()
            .await
            .unwrap_or_default();

        let mirror_on = current_subs.iter().any(|s| s.mirror_announcements);
        let subs_summary = if current_subs.is_empty() {
            "Nenhuma turma ativa neste canal no momento.".to_string()
        } else {
            current_subs
                .iter()
                .map(|s| {
                    format!(
                        "• Turma **`{}`** (Menção: `{}` • Anúncios da Sala: `{}`)",
                        s.class_code,
                        s.mention_mode,
                        if s.mirror_announcements { "SIM" } else { "NÃO" }
                    )
                })
                .collect::<Vec<_>>()
                .join("\n")
        };

        let mut selectable_classes = vec![
            "INI1", "INI2", "INI3", "ELI1", "ELI2", "ELI3", "MEI1", "MEI2", "MEI3", "INF1", "INF2",
            "INF3", "ELE1", "ELE2", "ELE3", "MEC1", "MEC2", "MEC3",
        ]
        .into_iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>();
        for c in &active_classes_in_db {
            if !selectable_classes.iter().any(|x| x.eq_ignore_ascii_case(c)) {
                selectable_classes.insert(0, c.clone());
            }
        }

        let options: Vec<Value> = selectable_classes
            .into_iter()
            .take(25)
            .map(|cls| {
                let is_subbed = current_subs
                    .iter()
                    .any(|s| s.class_code.eq_ignore_ascii_case(&cls));
                let has_sentinel = active_classes_in_db
                    .iter()
                    .any(|c| c.eq_ignore_ascii_case(&cls));
                json!({
                    "label": if is_subbed {
                        format!("[ATIVO NESTE CANAL] Turma {cls}")
                    } else {
                        format!("Adicionar / Alternar Turma {cls}")
                    },
                    "value": cls,
                    "description": if has_sentinel {
                        "Sentinela ativa no banco (monitorando NSAC)"
                    } else {
                        "Aguardando 1º aluno desta sala fazer /login"
                    }
                })
            })
            .collect();

        json!({
            "embeds": [{
                "title": "Anúncios do Servidor (1 Canal/Servidor) & Alerta Individual na DM",
                "description": format!(
                    "• **Canal de Anúncios deste Servidor:** `<#{ch_id}>` *(limite estrito de 1 canal por servidor; exibe APENAS a Média da Turma `M.T.`)*\n\
                    • **Sua Sala Detectada pelo NSAC:** **`{user_default_class}`**\n\
                    • **Seu Alerta Individual na DM (Sua Nota `M.B.` Privada):** **`{}`**\n\n\
                    **Turmas anunciadas neste canal:**\n{}{}",
                    if dm_enabled { "ATIVADO (Recebe na DM)" } else { "DESATIVADO" },
                    subs_summary,
                    status_note
                ),
                "color": 0x2ECC71,
                "footer": {
                    "text": "Privacidade total: o servidor só anuncia M.T. da turma • Sua nota M.B. vai apenas para a sua DM se ativado"
                }
            }],
            "components": [
                {
                    "type": 1,
                    "components": [{
                        "type": 3,
                        "custom_id": "select_channel_sub_class",
                        "placeholder": "Adicionar ou Remover Salas anunciadas neste Canal",
                        "options": options
                    }]
                },
                {
                    "type": 1,
                    "components": [
                        {
                            "type": 2,
                            "style": if dm_enabled { 4 } else { 3 },
                            "label": if dm_enabled {
                                "DM Individual: ATIVA (Desativar)"
                            } else {
                                "Ativar Minha Nota na DM"
                            },
                            "custom_id": "btn_toggle_dm_notify"
                        },
                        {
                            "type": 2,
                            "style": 1,
                            "label": "Testar Minha DM",
                            "custom_id": "btn_test_dm_notify"
                        },
                        {
                            "type": 2,
                            "style": 3,
                            "label": format!("Anunciar {} Aqui", user_default_class),
                            "custom_id": "btn_subscribe_default"
                        },
                        {
                            "type": 2,
                            "style": 1,
                            "label": "Testar Canal (M.T.)",
                            "custom_id": "btn_test_announcement"
                        },
                        {
                            "type": 2,
                            "style": if mirror_on { 3 } else { 2 },
                            "label": if mirror_on {
                                "Anúncios da Sala: ATIVO"
                            } else {
                                "Espelhar Anúncios da Sala"
                            },
                            "custom_id": "btn_toggle_mirror_announcements"
                        }
                    ]
                },
                {
                    "type": 1,
                    "components": [
                        {
                            "type": 2,
                            "style": 2,
                            "label": "Menção: @everyone",
                            "custom_id": "btn_mention_mode:everyone"
                        },
                        {
                            "type": 2,
                            "style": 2,
                            "label": "Menção: @here",
                            "custom_id": "btn_mention_mode:here"
                        },
                        {
                            "type": 2,
                            "style": 2,
                            "label": "Sem Menção",
                            "custom_id": "btn_mention_mode:none"
                        },
                        {
                            "type": 2,
                            "style": 4,
                            "label": "Desativar Canal",
                            "custom_id": "btn_unsubscribe_all_here"
                        },
                        {
                            "type": 2,
                            "style": 1,
                            "label": "Voltar às Notas",
                            "custom_id": "btn_view_boletim"
                        }
                    ]
                }
            ]
        })
    }

    async fn handle_autocomplete(
        &self,
        interaction_id: &str,
        interaction_token: &str,
        discord_user_id: &str,
        interaction: &Value,
    ) -> Result<(), ServerError> {
        let data = interaction.get("data").cloned().unwrap_or_default();
        let options = data
            .get("options")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();

        let focused = options
            .iter()
            .find(|o| o.get("focused").and_then(Value::as_bool).unwrap_or(false));

        let Some(focused_opt) = focused else {
            return self
                .send_interaction_callback(
                    interaction_id,
                    interaction_token,
                    &json!({ "type": 8, "data": { "choices": [] } }),
                )
                .await;
        };

        let opt_name = focused_opt.get("name").and_then(Value::as_str).unwrap_or("");
        let typed = focused_opt
            .get("value")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();
        let typed_norm = normalize_text(typed);

        let mut choices = Vec::new();
        if opt_name == "materia" {
            let mut candidate_subjects: Vec<String> = Vec::new();
            if let Ok(Some(acct)) = self.state.accounts().find_by_discord_id(discord_user_id).await
                && let Ok(Some(boletim)) = self.state.boletim().get_boletim(acct.id).await
            {
                for yr in &boletim.data {
                    for subj in &yr.grades {
                        if !candidate_subjects.contains(&subj.subject_name) {
                            candidate_subjects.push(subj.subject_name.clone());
                        }
                    }
                }
            }
            for &fallback in CTI_SUBJECT_SUGGESTIONS {
                if !candidate_subjects.iter().any(|s| s == fallback) {
                    candidate_subjects.push(fallback.to_string());
                }
            }

            for subj in candidate_subjects {
                if typed_norm.is_empty() || normalize_text(&subj).contains(&typed_norm) {
                    choices.push(json!({
                        "name": subj,
                        "value": subj
                    }));
                }
                if choices.len() >= 25 {
                    break;
                }
            }
        }

        self.send_interaction_callback(
            interaction_id,
            interaction_token,
            &json!({
                "type": 8,
                "data": { "choices": choices }
            }),
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn handle_modal_submit(
        &self,
        interaction_id: &str,
        interaction_token: &str,
        discord_user_id: &str,
        channel_id: Option<&str>,
        guild_id: Option<&str>,
        interaction: &Value,
    ) -> Result<(), ServerError> {
        let data = interaction.get("data").cloned().unwrap_or_default();
        let custom_id = data.get("custom_id").and_then(Value::as_str).unwrap_or("");
        let inputs = extract_modal_inputs(&data);

        self.send_interaction_callback(
            interaction_id,
            interaction_token,
            &json!({
                "type": 5,
                "data": { "flags": EPHEMERAL_FLAG }
            }),
        )
        .await?;

        if custom_id == "modal_nsac_login" {
            let email = inputs.get("nsac_email").map(String::as_str).unwrap_or("").trim();
            let password = Zeroizing::new(
                inputs
                    .get("nsac_password")
                    .cloned()
                    .unwrap_or_default(),
            );

            let payload = self
                .execute_discord_login(discord_user_id, channel_id, guild_id, email, password.as_str())
                .await;
            return self
                .edit_original_interaction_response(interaction_token, &payload)
                .await;
        }

        if custom_id == "modal_nsac_filter" {
            let mut qf = QueryFilter::default();
            let mut bimester_opt = None;

            if let Some(subj) = inputs.get("subject_contains").map(|s| s.trim()).filter(|s| !s.is_empty()) {
                qf.subject_name = Some(StringFilter {
                    contains: Some(vec![normalize_text(subj)]),
                    ..Default::default()
                });
            }
            if let Some(bim_raw) = inputs.get("bimester").map(|s| s.trim()).filter(|s| !s.is_empty())
                && let Ok(b) = bim_raw.parse::<u8>()
                && (1..=4).contains(&b)
            {
                bimester_opt = Some(b);
                qf.target_bimester = Some(NumberFilter {
                    eq: Some(vec![f64::from(b)]),
                    ..Default::default()
                });
            }
            if let Some(g_raw) = inputs.get("min_personal_grade").map(|s| s.trim()).filter(|s| !s.is_empty())
                && let Some(nf) = parse_modal_number_expr(g_raw)
            {
                qf.grade = Some(nf);
            }
            if let Some(mt_raw) = inputs.get("min_class_avg").map(|s| s.trim()).filter(|s| !s.is_empty())
                && let Some(nf) = parse_modal_number_expr(mt_raw)
            {
                qf.class_average = Some(nf);
            }
            if let Some(rec_raw) = inputs.get("recovery_code").map(|s| s.trim()).filter(|s| !s.is_empty()) {
                let upper = rec_raw.to_ascii_uppercase();
                if matches!(upper.as_str(), "SIM" | "TRUE" | "REC" | "1") {
                    qf.is_recovery = Some(BooleanFilter {
                        eq: Some(true),
                        ..Default::default()
                    });
                } else {
                    qf.recovery_code = Some(StringFilter {
                        eq: Some(vec![normalize_text(&upper)]),
                        ..Default::default()
                    });
                }
            }

            let payload = self
                .render_notas_view(
                    discord_user_id,
                    channel_id,
                    guild_id,
                    &qf,
                    None,
                    bimester_opt,
                    "completa",
                    false,
                    None,
                )
                .await;
            return self
                .edit_original_interaction_response(interaction_token, &payload)
                .await;
        }

        Ok(())
    }

    async fn execute_discord_login(
        &self,
        discord_user_id: &str,
        channel_id: Option<&str>,
        guild_id: Option<&str>,
        email: &str,
        password: &str,
    ) -> Value {
        if email.is_empty() || password.trim().is_empty() {
            return json!({
                "embeds": [{
                    "title": "Dados Inválidos no Formulário",
                    "description": "E-mail e senha do NSAC são obrigatórios.",
                    "color": 0xE74C3C
                }]
            });
        }

        let cookies = {
            let Ok(_permit) = self.state.semaphore.acquire().await else {
                return json!({ "content": "Erro interno ao adquirir semáforo NSAC." });
            };
            match self.state.nsac.login(email, password).await {
                Ok(c) => c,
                Err(err) => {
                    return json!({
                        "embeds": [{
                            "title": "Falha ao Autenticar no NSAC Online",
                            "description": err.user_message,
                            "color": 0xE74C3C,
                            "footer": { "text": format!("Código: {}", err.code.as_str()) }
                        }],
                        "components": [{
                            "type": 1,
                            "components": [{
                                "type": 2,
                                "style": 1,
                                "label": "Tentar Novamente (Formulário)",
                                "custom_id": "btn_open_login_modal"
                            }]
                        }]
                    });
                }
            }
        };

        let (mut boletim, rotated) = {
            let Ok(_permit) = self.state.semaphore.acquire().await else {
                return json!({ "content": "Erro interno ao adquirir semáforo NSAC." });
            };
            match self.state.nsac.fetch_boletim(&cookies).await {
                Ok(res) => res,
                Err(err) => {
                    return json!({
                        "embeds": [{
                            "title": "Erro ao Extrair Boletim do NSAC",
                            "description": err.user_message,
                            "color": 0xE74C3C
                        }]
                    });
                }
            }
        };
        let effective_cookies = rotated.unwrap_or(cookies);
        canonicalize_boletim(&mut boletim);

        let derived_class = boletim.current_class_code().and_then(normalize_class_code);

        let now = self.state.clock.now();
        let account = match self
            .state
            .accounts()
            .upsert_on_login(
                &self.state.crypto,
                email,
                password,
                &effective_cookies,
                derived_class.as_deref(),
                Some(discord_user_id),
                None,
                now,
            )
            .await
        {
            Ok(a) => a,
            Err(err) => {
                return json!({
                    "embeds": [{
                        "title": "Erro ao Salvar Conta Criptografada",
                        "description": err.user_message(),
                        "color": 0xE74C3C
                    }]
                });
            }
        };

        if let Some(ref authoritative_class) = derived_class {
            let _ = self
                .state
                .accounts()
                .update_class_code(account.id, authoritative_class, now)
                .await;
        }

        let _ = self.state.boletim().upsert_boletim(account.id, &boletim).await;
        for year_info in &boletim.data {
            self.state
                .caches
                .invalidate_ranking_year(i32::from(year_info.year))
                .await;
        }
        if let Ok(etag) = compute_strong_etag(&boletim) {
            self.state
                .caches
                .boletim
                .insert(
                    account.id,
                    CachedBoletim {
                        data: Arc::new(boletim.clone()),
                        etag,
                        cached_at: now,
                    },
                )
                .await;
        }

        let class_hash = compute_class_grade_hash(&boletim);
        let final_class = derived_class
            .as_deref()
            .or(account.class_code.as_deref())
            .unwrap_or("CTI");

        let _ = self
            .state
            .heuristic
            .initialize_class_on_login(final_class, &class_hash, account.id, now)
            .await;
        let _ = self.state.heuristic.record_api_activity(final_class, now).await;

        self.render_notas_view(
            discord_user_id,
            channel_id,
            guild_id,
            &QueryFilter::default(),
            None,
            None,
            "completa",
            false,
            None,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn render_notas_view(
        &self,
        discord_user_id: &str,
        channel_id: Option<&str>,
        guild_id: Option<&str>,
        query_filter: &QueryFilter,
        explicit_year: Option<u16>,
        selected_bimester: Option<u8>,
        visao: &str,
        force_refresh: bool,
        override_other_class_mt: Option<&str>,
    ) -> Value {
        let user_acct = self
            .state
            .accounts()
            .find_by_discord_id(discord_user_id)
            .await
            .ok()
            .flatten();

        // Allow viewing another class's M.T. (Médias da Sala) if override_other_class_mt is selected!
        let (target_acct, is_viewing_other_class_mt) = if let Some(other_cls) = override_other_class_mt {
            let class_accounts = self
                .state
                .accounts()
                .list_active_by_class(other_cls)
                .await
                .unwrap_or_default();
            if let Some(sentinel_acct) = class_accounts.into_iter().next() {
                (sentinel_acct, true)
            } else {
                return json!({
                    "embeds": [{
                        "title": format!("Turma {} ainda sem sentinela cadastrada", other_cls),
                        "description": format!("Nenhum aluno da turma **`{other_cls}`** vinculou o NSAC ainda para espelhar as médias da sala (`M.T.`)."),
                        "color": 0xF39C12
                    }]
                });
            }
        } else if let Some(a) = user_acct.clone() {
            (a, false)
        } else {
            return json!({
                "embeds": [{
                    "title": "Conta NSAC não vinculada",
                    "description": "Clique no botão abaixo para entrar com seu **e-mail e senha do NSAC Online**. A turma, matérias e médias são extraídas automaticamente pelo servidor.",
                    "color": 0x5865F2,
                    "footer": { "text": "AES-256-GCM • Visível apenas para você" }
                }],
                "components": [{
                    "type": 1,
                    "components": [{
                        "type": 2,
                        "style": 1,
                        "label": "Entrar no NSAC Online",
                        "custom_id": "btn_open_login_modal"
                    }]
                }]
            });
        };

        let effective_visao = if is_viewing_other_class_mt {
            "turma"
        } else {
            visao
        };

        let now = self.state.clock.now();
        let mut full_boletim = if !force_refresh
            && let Some(hit) = self.state.caches.boletim.get(&target_acct.id).await
        {
            (*hit.data).clone()
        } else if !force_refresh
            && let Ok(Some(db_bol)) = self.state.boletim().get_boletim(target_acct.id).await
        {
            db_bol
        } else {
            match self.refresh_account_boletim(target_acct.id).await {
                Ok(b) => b,
                Err(err) => {
                    return json!({
                        "embeds": [{
                            "title": "Erro ao Consultar Boletim",
                            "description": err.user_message(),
                            "color": 0xE74C3C
                        }],
                        "components": [{
                            "type": 1,
                            "components": [{
                                "type": 2,
                                "style": 1,
                                "label": "Atualizar Credenciais",
                                "custom_id": "btn_open_login_modal"
                            }]
                        }]
                    });
                }
            }
        };

        canonicalize_boletim(&mut full_boletim);

        let class_code = full_boletim
            .current_class_code()
            .and_then(normalize_class_code)
            .or_else(|| target_acct.class_code.clone())
            .unwrap_or_else(|| "CTI".to_string());

        if !is_viewing_other_class_mt && target_acct.class_code.as_deref() != Some(class_code.as_str()) {
            let _ = self
                .state
                .accounts()
                .update_class_code(target_acct.id, &class_code, now)
                .await;
            let class_hash = compute_class_grade_hash(&full_boletim);
            let _ = self
                .state
                .heuristic
                .initialize_class_on_login(&class_code, &class_hash, target_acct.id, now)
                .await;
        }

        let poll_hint_secs = self
            .state
            .heuristic
            .record_api_activity(&class_code, now)
            .await
            .unwrap_or(self.state.config.sentinel.cold_baseline_secs);

        let sentinel_state = self
            .state
            .sentinel()
            .get_class_state(&class_code)
            .await
            .ok()
            .flatten();

        let filtered = filter_query(&full_boletim, query_filter).unwrap_or_else(|_| full_boletim.clone());

        let target_year_info: Option<&YearInfo> = if let Some(y) = explicit_year {
            filtered.data.iter().find(|yr| yr.year == y)
        } else {
            filtered.data.last()
        };

        let active_classes_in_db = self
            .state
            .accounts()
            .list_active_classes()
            .await
            .unwrap_or_default();

        let is_guild_channel = guild_id.map(str::trim).is_some_and(|g| !g.is_empty());
        let dm_enabled = if let Some(ref ua) = user_acct {
            is_dm_notification_enabled(&self.state.db, ua.id)
                .await
                .unwrap_or(false)
        } else {
            false
        };

        let channel_subbed = if is_guild_channel
            && let Some(ch) = channel_id
        {
            list_channel_subscriptions(&self.state.db, ch)
                .await
                .unwrap_or_default()
                .iter()
                .any(|s| s.class_code.eq_ignore_ascii_case(&class_code))
        } else {
            false
        };

        let Some(year_info) = target_year_info else {
            return json!({
                "embeds": [{
                    "title": format!("Boletim NSAC — {}", class_code),
                    "description": "Nenhuma disciplina encontrada para o filtro selecionado.",
                    "color": 0xF39C12
                }],
                "components": self.build_boletim_components(
                    &full_boletim,
                    None,
                    selected_bimester,
                    effective_visao,
                    &active_classes_in_db,
                    override_other_class_mt,
                    is_guild_channel,
                    channel_subbed,
                    dm_enabled
                )
            });
        };

        let (embed_color, table_description, fields) = format_year_boletim_embed(
            year_info,
            selected_bimester,
            effective_visao,
            is_viewing_other_class_mt,
        );

        let certainty_pct = sentinel_state
            .as_ref()
            .map(|s| s.current_certainty * 100.0)
            .unwrap_or(0.0);
        let hot_badge = if sentinel_state
            .as_ref()
            .and_then(|s| s.hot_until)
            .is_some_and(|h| h > now)
        {
            "MODO QUENTE"
        } else if certainty_pct > 25.0 {
            "JANELA HISTÓRICA (±4d)"
        } else {
            "SENTINELA ATIVA"
        };

        let components = self.build_boletim_components(
            &full_boletim,
            Some(year_info.year),
            selected_bimester,
            effective_visao,
            &active_classes_in_db,
            override_other_class_mt,
            is_guild_channel,
            channel_subbed,
            dm_enabled,
        );

        let view_title = if is_viewing_other_class_mt {
            format!("Médias da Sala {} (M.T.)", class_code)
        } else {
            match selected_bimester {
                Some(b) => format!("{}º Bimestre", b),
                None if effective_visao == "turma" => "Médias da Sala (M.T.)".to_string(),
                None if effective_visao == "recuperacao" => "Recuperações".to_string(),
                None => "Visão Geral (1º ao 4º Bim)".to_string(),
            }
        };

        let context_footer = if is_guild_channel {
            if channel_subbed {
                "Canal do Servidor: Anunciando M.T."
            } else {
                "Canal do Servidor: Sem anúncio"
            }
        } else if dm_enabled {
            "Sua DM: Alerta Individual ATIVO"
        } else {
            "Sua DM: Alerta Individual Inativo"
        };

        json!({
            "embeds": [{
                "title": format!(
                    "Boletim {} / {} — {} ({})",
                    year_info.title, year_info.year, view_title, year_info.status
                ),
                "description": table_description,
                "color": embed_color,
                "fields": fields,
                "footer": {
                    "text": format!(
                        "{} • Certeza {:.0}% • Próx. checagem: {} • {}",
                        hot_badge,
                        certainty_pct,
                        format_duration_human(poll_hint_secs),
                        context_footer
                    )
                }
            }],
            "components": components
        })
    }

    async fn render_heuristica_view(
        &self,
        discord_user_id: &str,
        channel_id: Option<&str>,
        guild_id: Option<&str>,
        estimular: bool,
    ) -> Value {
        let Some(acct) = self
            .state
            .accounts()
            .find_by_discord_id(discord_user_id)
            .await
            .ok()
            .flatten()
        else {
            return json!({
                "embeds": [{
                    "title": "Conta NSAC não vinculada",
                    "description": "Faça `/login` primeiro para o servidor identificar sua turma diretamente pelo NSAC.",
                    "color": 0xE74C3C
                }],
                "components": [{
                    "type": 1,
                    "components": [{
                        "type": 2,
                        "style": 1,
                        "label": "Entrar no NSAC Online",
                        "custom_id": "btn_open_login_modal"
                    }]
                }]
            });
        };

        let class_code = acct.class_code.unwrap_or_else(|| "INI3".to_string());
        let now = self.state.clock.now();
        let mut poll_summary = String::new();

        if estimular {
            let _ = self.state.heuristic.record_api_activity(&class_code, now).await;
            if let Ok(Some(mut st)) = self.state.sentinel().get_class_state(&class_code).await {
                st.next_check_at = now;
                let _ = self.state.sentinel().upsert_class_state(&st).await;
            }
            if let Ok(report) = poll_due_classes_once(&self.state, &self.fcm).await {
                poll_summary = format!(
                    "\n\n**Checagem Sentinela Executada:** `{}` turma(s) verificada(s), `{}` mudança(s) de M.T., `{}` aluno(s) atualizado(s).",
                    report.classes_checked, report.classes_changed, report.fanout_students_updated
                );
            }
        }

        let state_opt = self
            .state
            .sentinel()
            .get_class_state(&class_code)
            .await
            .ok()
            .flatten();
        let events = self
            .state
            .sentinel()
            .list_events_for_class(&class_code)
            .await
            .unwrap_or_default();
        let active_students = self
            .state
            .accounts()
            .list_active_by_class(&class_code)
            .await
            .unwrap_or_default();

        let is_guild_channel = guild_id.map(str::trim).is_some_and(|g| !g.is_empty());
        let dm_enabled = is_dm_notification_enabled(&self.state.db, acct.id)
            .await
            .unwrap_or(false);

        let channel_subs = if is_guild_channel
            && let Some(ch) = channel_id
        {
            list_channel_subscriptions(&self.state.db, ch)
                .await
                .unwrap_or_default()
        } else {
            Vec::new()
        };

        let (certainty, interval_secs, next_check, hot_status, last_hash, err_count) =
            if let Some(ref st) = state_opt {
                let hot = match st.hot_until {
                    Some(h) if h > now => {
                        let rem = (h - now).num_seconds().max(0) as u64;
                        format!("ATIVO ({})", format_duration_human(rem))
                    }
                    _ => "Inativo".to_string(),
                };
                let rem_next = (st.next_check_at - now).num_seconds().max(0) as u64;
                let hash_short = st
                    .last_class_hash
                    .as_deref()
                    .map(|h| &h[..12.min(h.len())])
                    .unwrap_or("nenhum")
                    .to_string();
                (
                    st.current_certainty,
                    st.current_interval_secs.max(1) as u64,
                    format_duration_human(rem_next),
                    hot,
                    hash_short,
                    st.consecutive_errors,
                )
            } else {
                (
                    0.0,
                    self.state.config.sentinel.cold_baseline_secs,
                    "Agendado".to_string(),
                    "Inativo".to_string(),
                    "nenhum".to_string(),
                    0,
                )
            };

        let bar = render_progress_bar(certainty);
        let notify_field_title = if is_guild_channel {
            "Anúncios neste Canal do Servidor"
        } else {
            "Alerta Individual na Sua DM"
        };
        let notify_field_value = if is_guild_channel {
            let sub_txt = if channel_subs.is_empty() {
                "Nenhuma sala configurada".to_string()
            } else {
                channel_subs
                    .iter()
                    .map(|s| s.class_code.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            format!("`{sub_txt}` (`{}` eventos hist.)", events.len())
        } else {
            format!(
                "`{}` (`{}` eventos hist.)",
                if dm_enabled { "ATIVADO" } else { "DESATIVADO" },
                events.len()
            )
        };

        let config_btn_label = if is_guild_channel {
            "Configurar Anúncios do Servidor & DM"
        } else if dm_enabled {
            "Alerta na Minha DM (Ativo)"
        } else {
            "Ativar Alerta na Minha DM"
        };

        json!({
            "embeds": [{
                "title": format!("Heurística Sentinel — Turma {}", class_code),
                "description": format!(
                    "Monitoramento automático espelhado no PostgreSQL com aprendizado estatístico multi-ano (raio `±{} dias`).{}",
                    self.state.config.sentinel.historical_radius_days,
                    poll_summary
                ),
                "color": if certainty >= 0.8 { 0xE67E22 } else if certainty >= 0.3 { 0x2ECC71 } else { 0x5865F2 },
                "fields": [
                    {
                        "name": "Certeza de Lançamento",
                        "value": format!("`{}` **{:.1}%**", bar, certainty * 100.0),
                        "inline": true
                    },
                    {
                        "name": "Intervalo Atual",
                        "value": format!("`{}`", format_duration_human(interval_secs)),
                        "inline": true
                    },
                    {
                        "name": "Próxima Checagem",
                        "value": format!("`{}`", next_check),
                        "inline": true
                    },
                    {
                        "name": "Modo Quente",
                        "value": format!("`{}`", hot_status),
                        "inline": true
                    },
                    {
                        "name": notify_field_title,
                        "value": notify_field_value,
                        "inline": true
                    },
                    {
                        "name": "Alunos / Hash M.T.",
                        "value": format!("`{}` • `{}` (erros: `{}`)", active_students.len(), last_hash, err_count),
                        "inline": true
                    }
                ]
            }],
            "components": [{
                "type": 1,
                "components": [
                    {
                        "type": 2,
                        "style": 3,
                        "label": config_btn_label,
                        "custom_id": "btn_channel_config"
                    },
                    {
                        "type": 2,
                        "style": 1,
                        "label": "Checar Turma Agora",
                        "custom_id": "btn_trigger_sentinel"
                    },
                    {
                        "type": 2,
                        "style": 2,
                        "label": "Voltar ao Boletim",
                        "custom_id": "btn_view_boletim"
                    }
                ]
            }]
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn build_boletim_components(
        &self,
        full_boletim: &AllYearsResponse,
        active_year: Option<u16>,
        active_bimester: Option<u8>,
        visao: &str,
        active_classes_in_db: &[String],
        override_other_class_mt: Option<&str>,
        is_guild_channel: bool,
        channel_subbed: bool,
        dm_enabled: bool,
    ) -> Vec<Value> {
        let mut rows = Vec::new();

        rows.push(json!({
            "type": 1,
            "components": [{
                "type": 3,
                "custom_id": "select_bimester",
                "placeholder": "Selecionar Bimestre ou Comparativo com a Sala",
                "options": [
                    {
                        "label": "Tabela Geral (1º ao 4º Bimestre + Média Final)",
                        "value": "all",
                        "default": active_bimester.is_none() && visao == "completa" && override_other_class_mt.is_none()
                    },
                    {
                        "label": "1º Bimestre (Você vs Média da Sala + Faltas)",
                        "value": "1",
                        "default": active_bimester == Some(1)
                    },
                    {
                        "label": "2º Bimestre (Você vs Média da Sala + Faltas)",
                        "value": "2",
                        "default": active_bimester == Some(2)
                    },
                    {
                        "label": "3º Bimestre (Você vs Média da Sala + Faltas)",
                        "value": "3",
                        "default": active_bimester == Some(3)
                    },
                    {
                        "label": "4º Bimestre (Você vs Média da Sala + Faltas)",
                        "value": "4",
                        "default": active_bimester == Some(4)
                    },
                    {
                        "label": "Médias da Minha Sala (M.T. nos 4 Bimestres)",
                        "value": "mt",
                        "default": visao == "turma" && override_other_class_mt.is_none()
                    },
                    {
                        "label": "Apenas Recuperações (SAT / INS / NC)",
                        "value": "rec",
                        "default": visao == "recuperacao"
                    }
                ]
            }]
        }));

        // Row 2: View M.T. (Class Averages) of ANY other registered class in the DB!
        if !active_classes_in_db.is_empty() {
            let mut class_opts = vec![json!({
                "label": "Meu Boletim Pessoal (Minha Sala)",
                "value": "mine",
                "default": override_other_class_mt.is_none()
            })];
            for cls in active_classes_in_db.iter().take(24) {
                class_opts.push(json!({
                    "label": format!("Ver Médias da Sala {} (M.T.)", cls),
                    "value": cls,
                    "description": "Exibe as médias coletivas (M.T.) espelhadas desta turma",
                    "default": override_other_class_mt.is_some_and(|o| o.eq_ignore_ascii_case(cls))
                }));
            }
            rows.push(json!({
                "type": 1,
                "components": [{
                    "type": 3,
                    "custom_id": "select_other_class_mt",
                    "placeholder": "Espiar Média (M.T.) de Outras Salas Cadastradas",
                    "options": class_opts
                }]
            }));
        }

        // Row 3: Year selector (if multi-year and viewing own boletim)
        if override_other_class_mt.is_none() && full_boletim.data.len() > 1 {
            let year_options: Vec<Value> = full_boletim
                .data
                .iter()
                .rev()
                .take(25)
                .map(|yr| {
                    json!({
                        "label": format!("{} / {} ({})", yr.title, yr.year, yr.status),
                        "value": yr.year.to_string(),
                        "default": active_year == Some(yr.year)
                    })
                })
                .collect();

            rows.push(json!({
                "type": 1,
                "components": [{
                    "type": 3,
                    "custom_id": "select_year",
                    "placeholder": "Trocar Ano Letivo",
                    "options": year_options
                }]
            }));
        }

        let (first_btn_style, first_btn_label) = if is_guild_channel {
            if channel_subbed {
                (2, "Anúncios do Canal (Ativo)")
            } else {
                (3, "Anunciar Sala neste Canal")
            }
        } else if dm_enabled {
            (2, "Alerta na Minha DM (Ativo)")
        } else {
            (3, "Ativar Alerta na Minha DM")
        };

        rows.push(json!({
            "type": 1,
            "components": [
                {
                    "type": 2,
                    "style": first_btn_style,
                    "label": first_btn_label,
                    "custom_id": "btn_channel_config"
                },
                {
                    "type": 2,
                    "style": 1,
                    "label": "Filtrar (Form)",
                    "custom_id": "btn_open_filter_modal"
                },
                {
                    "type": 2,
                    "style": 2,
                    "label": "Atualizar NSAC",
                    "custom_id": "btn_force_refresh"
                },
                {
                    "type": 2,
                    "style": 2,
                    "label": "Heurística",
                    "custom_id": "btn_heuristic_status"
                }
            ]
        }));

        rows
    }

    async fn refresh_account_boletim(
        &self,
        account_id: i64,
    ) -> Result<AllYearsResponse, ServerError> {
        let secrets = self
            .state
            .accounts()
            .get_decrypted_credentials(&self.state.crypto, account_id)
            .await?
            .ok_or_else(|| ServerError::user_not_linked("Conta não encontrada."))?;

        let mut fetched: Option<AllYearsResponse> = None;
        if let Some(cookies) = secrets.cookies {
            let res = {
                let _permit = self.state.semaphore.acquire().await?;
                self.state.nsac.fetch_boletim(&cookies).await
            };
            if let Ok((bol, rotated)) = res {
                if let Some(ref rot) = rotated {
                    let _ = self
                        .state
                        .accounts()
                        .update_cookies(&self.state.crypto, account_id, rot, self.state.clock.now())
                        .await;
                }
                fetched = Some(bol);
            }
        }

        let mut boletim = match fetched {
            Some(b) => b,
            None => {
                let fresh_cookies = {
                    let _permit = self.state.semaphore.acquire().await?;
                    self.state
                        .nsac
                        .login(&secrets.nsac_email, secrets.password.as_str())
                        .await?
                };
                self.state
                    .accounts()
                    .update_cookies(
                        &self.state.crypto,
                        account_id,
                        &fresh_cookies,
                        self.state.clock.now(),
                    )
                    .await?;
                let (bol, _) = {
                    let _permit = self.state.semaphore.acquire().await?;
                    self.state.nsac.fetch_boletim(&fresh_cookies).await?
                };
                bol
            }
        };

        canonicalize_boletim(&mut boletim);
        self.state
            .boletim()
            .upsert_boletim(account_id, &boletim)
            .await?;
        for year_info in &boletim.data {
            self.state
                .caches
                .invalidate_ranking_year(i32::from(year_info.year))
                .await;
        }
        let now = self.state.clock.now();
        if let Ok(etag) = compute_strong_etag(&boletim) {
            self.state
                .caches
                .boletim
                .insert(
                    account_id,
                    CachedBoletim {
                        data: Arc::new(boletim.clone()),
                        etag,
                        cached_at: now,
                    },
                )
                .await;
        }
        Ok(boletim)
    }
}

fn format_year_boletim_embed(
    year_info: &YearInfo,
    selected_bimester: Option<u8>,
    visao: &str,
    only_class_mt: bool,
) -> (u64, String, Vec<Value>) {
    const ANSI_RESET: &str = "\u{1b}[0m";
    const ANSI_BOLD_CYAN: &str = "\u{1b}[1;36m";
    const ANSI_DIM: &str = "\u{1b}[0;37m";

    let mut low_count = 0usize;
    let mut rec_count = 0usize;
    let mut total_year_absences = 0u32;
    let mut table = String::from("```ansi\n");

    if let Some(target_bim) = selected_bimester
        && !only_class_mt
    {
        table.push_str(&format!(
            "{ANSI_BOLD_CYAN}{:<22} {:>5} {:>5} {:>5} {:>3} {:>4}{ANSI_RESET}\n",
            "Disciplina", "Você", "Sala", "Dif", "Flt", "Rec"
        ));
        table.push_str("──────────────────────────────────────────────────\n");

        for subj in &year_info.grades {
            let Some(b) = subj.bimesters.iter().find(|b| b.bimester == target_bim) else {
                continue;
            };
            let short_name = compact_subject_name(&subj.subject_name, 22);
            if b.personal.grade.is_some_and(|g| g > 0.0 && g < 6.0) {
                low_count += 1;
            }
            if b.personal.recovery {
                rec_count += 1;
            }
            total_year_absences += b.personal.absences;

            let mb_str = format_cell_plain(b.personal.grade, None);
            let mt_str = format_cell_plain(b.class.average_grade, None);
            let diff_str = match (b.personal.grade, b.class.average_grade) {
                (Some(p), Some(c)) if p > 0.0 && c > 0.0 => format!("{:+.1}", p - c),
                _ => "-".to_string(),
            };
            let rec_str = match b.personal.recovery_code {
                Some(RecoveryStatusCode::Sat) => "SAT",
                Some(RecoveryStatusCode::Ins) => "INS",
                Some(RecoveryStatusCode::Nc) => "NC",
                _ => "-",
            };

            let color = grade_ansi_color(b.personal.grade, b.personal.recovery_code);
            table.push_str(&format!(
                "{:<22} {color}{:>5}{ANSI_RESET} {:>5} {:>5} {:>3} {:>4}\n",
                short_name, mb_str, mt_str, diff_str, b.personal.absences, rec_str
            ));
        }
    } else if only_class_mt {
        table.push_str(&format!(
            "{ANSI_BOLD_CYAN}{:<22} {:>5} {:>5} {:>5} {:>5}{ANSI_RESET}\n",
            "Disciplina (M.T.)", "1ºB", "2ºB", "3ºB", "4ºB"
        ));
        table.push_str("──────────────────────────────────────────────\n");

        for subj in &year_info.grades {
            let short_name = compact_subject_name(&subj.subject_name, 22);
            let mut cells_colored = Vec::with_capacity(4);
            for bim_num in 1..=4 {
                if let Some(b) = subj.bimesters.iter().find(|x| x.bimester == bim_num) {
                    let val = b.class.average_grade;
                    let raw_cell = format_cell_plain(val, None);
                    let col = grade_ansi_color(val, None);
                    cells_colored.push(format!("{col}{:>5}{ANSI_RESET}", raw_cell));
                } else {
                    cells_colored.push(format!("{ANSI_DIM}{:>5}{ANSI_RESET}", "-"));
                }
            }
            table.push_str(&format!(
                "{:<22} {} {} {} {}\n",
                short_name, cells_colored[0], cells_colored[1], cells_colored[2], cells_colored[3]
            ));
        }
        if !year_info.bimesters_metrics.is_empty() {
            table.push_str("──────────────────────────────────────────────\n");
            let mut c_avgs: [String; 4] = std::array::from_fn(|_| String::from("-"));
            for (idx, m) in year_info.bimesters_metrics.iter().take(4).enumerate() {
                if m.class_average > 0.0 {
                    c_avgs[idx] = format!("{:.1}", m.class_average);
                }
            }
            table.push_str(&format!(
                "{ANSI_BOLD_CYAN}{:<22} {:>5} {:>5} {:>5} {:>5}{ANSI_RESET}\n",
                "Média Geral da Sala", c_avgs[0], c_avgs[1], c_avgs[2], c_avgs[3]
            ));
        }
    } else {
        table.push_str(&format!(
            "{ANSI_BOLD_CYAN}{:<21} {:>5} {:>5} {:>5} {:>5} {:>4} {:>3}{ANSI_RESET}\n",
            "Disciplina", "1ºB", "2ºB", "3ºB", "4ºB", "MF", "Flt"
        ));
        table.push_str("───────────────────────────────────────────────────────\n");

        for subj in &year_info.grades {
            let short_name = compact_subject_name(&subj.subject_name, 21);
            total_year_absences += subj.results.total_absences;

            let mut cells_colored = Vec::with_capacity(4);
            for bim_num in 1..=4 {
                if let Some(b) = subj.bimesters.iter().find(|x| x.bimester == bim_num) {
                    if b.personal.grade.is_some_and(|g| g > 0.0 && g < 6.0) {
                        low_count += 1;
                    }
                    if b.personal.recovery {
                        rec_count += 1;
                    }
                    let val = if visao == "turma" {
                        b.class.average_grade
                    } else {
                        b.personal.grade
                    };
                    let rec_code = if visao == "turma" {
                        None
                    } else {
                        b.personal.recovery_code
                    };
                    let raw_cell = format_cell_plain(val, rec_code);
                    let col = grade_ansi_color(val, rec_code);
                    cells_colored.push(format!("{col}{:>5}{ANSI_RESET}", raw_cell));
                } else {
                    cells_colored.push(format!("{ANSI_DIM}{:>5}{ANSI_RESET}", "-"));
                }
            }

            let mf_str = format_cell_plain(subj.results.grade, None);
            let mf_col = grade_ansi_color(subj.results.grade, None);
            table.push_str(&format!(
                "{:<21} {} {} {} {} {mf_col}{:>4}{ANSI_RESET} {:>3}\n",
                short_name,
                cells_colored[0],
                cells_colored[1],
                cells_colored[2],
                cells_colored[3],
                mf_str,
                subj.results.total_absences
            ));
        }

        if !year_info.bimesters_metrics.is_empty() {
            table.push_str("───────────────────────────────────────────────────────\n");
            let mut u_avgs: [String; 4] = std::array::from_fn(|_| String::from("-"));
            let mut c_avgs: [String; 4] = std::array::from_fn(|_| String::from("-"));
            for (idx, m) in year_info.bimesters_metrics.iter().take(4).enumerate() {
                if m.user_average > 0.0 {
                    u_avgs[idx] = format!("{:.1}", m.user_average);
                }
                if m.class_average > 0.0 {
                    c_avgs[idx] = format!("{:.1}", m.class_average);
                }
            }
            table.push_str(&format!(
                "{ANSI_BOLD_CYAN}{:<21} {:>5} {:>5} {:>5} {:>5} {:>4} {:>3}{ANSI_RESET}\n",
                "Sua Média Geral",
                u_avgs[0],
                u_avgs[1],
                u_avgs[2],
                u_avgs[3],
                "",
                total_year_absences
            ));
            table.push_str(&format!(
                "{ANSI_DIM}{:<21} {:>5} {:>5} {:>5} {:>5}{ANSI_RESET}\n",
                "Média da Sala (M.T.)", c_avgs[0], c_avgs[1], c_avgs[2], c_avgs[3]
            ));
        }
    }

    table.push_str("```");

    let color = if only_class_mt {
        0x5865F2
    } else if low_count > 0 {
        0xE74C3C
    } else if rec_count > 0 {
        0xF39C12
    } else {
        0x2ECC71
    };

    let fields = if only_class_mt {
        vec![json!({
            "name": "Modo Espiar Outra Sala",
            "value": format!("Exibindo as médias coletivas (`M.T.`) das `{}` disciplinas de **`{}`**", year_info.grades.len(), year_info.title),
            "inline": false
        })]
    } else {
        vec![
            json!({
                "name": "Disciplinas",
                "value": format!("`{}` matérias", year_info.grades.len()),
                "inline": true
            }),
            json!({
                "name": "Faltas Acumuladas",
                "value": format!("`{}` faltas", total_year_absences),
                "inline": true
            }),
            json!({
                "name": "Alertas no Ano",
                "value": if low_count == 0 && rec_count == 0 {
                    "`Nenhuma nota < 6.0`".to_string()
                } else {
                    format!("`{}` nota(s) < 6 • `{}` rec(s)", low_count, rec_count)
                },
                "inline": true
            }),
        ]
    };

    (color, table, fields)
}

fn compact_subject_name(name: &str, max_chars: usize) -> String {
    let replaced = name
        .replace("Linguagem de Programação", "LP")
        .replace("Língua Portuguesa e Literatura", "L.P.L.")
        .replace("Análise e Projeto de Sistemas", "An. Proj. Sistemas")
        .replace("Programação Orientada a Objetos", "P.O.O.")
        .replace("Gestão de Negócios", "Gest. Negócios")
        .replace("Inglês Técnico", "Inglês Téc.");

    let chars: Vec<char> = replaced.chars().collect();
    if chars.len() <= max_chars {
        replaced
    } else {
        let mut s: String = chars.into_iter().take(max_chars.saturating_sub(1)).collect();
        s.push('…');
        s
    }
}

fn format_cell_plain(grade: Option<f64>, rec: Option<RecoveryStatusCode>) -> String {
    let Some(v) = grade.filter(|&x| x > 0.0) else {
        return "-".to_string();
    };
    let suffix = match rec {
        Some(RecoveryStatusCode::Sat) => "*",
        Some(RecoveryStatusCode::Ins | RecoveryStatusCode::Nc) => "!",
        _ => "",
    };
    format!("{v:.1}{suffix}")
}

fn grade_ansi_color(grade: Option<f64>, rec: Option<RecoveryStatusCode>) -> &'static str {
    let Some(v) = grade.filter(|&x| x > 0.0) else {
        return "\u{1b}[0;37m";
    };
    if matches!(rec, Some(RecoveryStatusCode::Ins | RecoveryStatusCode::Nc)) || v < 6.0 {
        "\u{1b}[1;31m"
    } else if matches!(rec, Some(RecoveryStatusCode::Sat)) {
        "\u{1b}[1;33m"
    } else {
        "\u{1b}[1;32m"
    }
}

fn format_duration_human(secs: u64) -> String {
    if secs >= 3600 {
        let h = secs / 3600;
        let m = (secs % 3600) / 60;
        format!("{h}h{m:02}m")
    } else if secs >= 60 {
        let m = secs / 60;
        let s = secs % 60;
        format!("{m}m{s:02}s")
    } else {
        format!("{secs}s")
    }
}

fn render_progress_bar(ratio: f64) -> String {
    let clamped = ratio.clamp(0.0, 1.0);
    let filled = ((clamped * 10.0).round() as usize).min(10);
    let empty = 10usize.saturating_sub(filled);
    format!("{}{}", "█".repeat(filled), "░".repeat(empty))
}

fn extract_discord_user_id(interaction: &Value) -> String {
    interaction
        .get("member")
        .and_then(|m| m.get("user"))
        .and_then(|u| u.get("id"))
        .and_then(Value::as_str)
        .or_else(|| {
            interaction
                .get("user")
                .and_then(|u| u.get("id"))
                .and_then(Value::as_str)
        })
        .unwrap_or("0")
        .to_string()
}

fn get_opt_str(options: &[Value], name: &str) -> Option<String> {
    options.iter().find_map(|o| {
        if o.get("name").and_then(Value::as_str) == Some(name) {
            o.get("value").and_then(Value::as_str).map(ToString::to_string)
        } else {
            None
        }
    })
}

fn get_opt_i64(options: &[Value], name: &str) -> Option<i64> {
    options.iter().find_map(|o| {
        if o.get("name").and_then(Value::as_str) == Some(name) {
            o.get("value").and_then(Value::as_i64)
        } else {
            None
        }
    })
}

fn get_opt_bool(options: &[Value], name: &str) -> Option<bool> {
    options.iter().find_map(|o| {
        if o.get("name").and_then(Value::as_str) == Some(name) {
            o.get("value").and_then(Value::as_bool)
        } else {
            None
        }
    })
}

fn extract_modal_inputs(data: &Value) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    let Some(rows) = data.get("components").and_then(Value::as_array) else {
        return map;
    };
    for row in rows {
        if let Some(items) = row.get("components").and_then(Value::as_array) {
            for item in items {
                if let (Some(cid), Some(val)) = (
                    item.get("custom_id").and_then(Value::as_str),
                    item.get("value").and_then(Value::as_str),
                ) {
                    map.insert(cid.to_string(), val.to_string());
                }
            }
        }
    }
    map
}

fn parse_modal_number_expr(raw: &str) -> Option<NumberFilter> {
    let s = raw.trim().replace(',', ".");
    if let Some(rest) = s.strip_prefix(">=") {
        return rest.trim().parse::<f64>().ok().map(|v| NumberFilter {
            gte: Some(vec![v]),
            ..Default::default()
        });
    }
    if let Some(rest) = s.strip_prefix("<=") {
        return rest.trim().parse::<f64>().ok().map(|v| NumberFilter {
            lte: Some(vec![v]),
            ..Default::default()
        });
    }
    if let Some(rest) = s.strip_prefix('>') {
        return rest.trim().parse::<f64>().ok().map(|v| NumberFilter {
            gt: Some(vec![v]),
            ..Default::default()
        });
    }
    if let Some(rest) = s.strip_prefix('<') {
        return rest.trim().parse::<f64>().ok().map(|v| NumberFilter {
            lt: Some(vec![v]),
            ..Default::default()
        });
    }
    s.parse::<f64>().ok().map(|v| NumberFilter {
        eq: Some(vec![v]),
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_command_definitions_and_zero_trust_login_modal() {
        let cmds = DiscordBot::build_command_definitions();
        let arr = cmds.as_array().unwrap();
        assert_eq!(arr.len(), 3);
        assert_eq!(arr[0]["name"], "login");
        assert!(arr[0].get("options").is_none());
        assert_eq!(arr[1]["name"], "notas");
        assert_eq!(arr[2]["name"], "heuristica");

        let login_modal = DiscordBot::build_login_modal();
        assert_eq!(login_modal["type"], 9);
        assert_eq!(login_modal["data"]["custom_id"], "modal_nsac_login");
        assert_eq!(
            login_modal["data"]["components"].as_array().unwrap().len(),
            2
        );

        let filter_modal = DiscordBot::build_filter_modal();
        assert_eq!(filter_modal["type"], 9);
        assert_eq!(filter_modal["data"]["custom_id"], "modal_nsac_filter");
    }

    #[test]
    fn test_parse_modal_number_expr_operators() {
        let gte = parse_modal_number_expr(">= 6,5").unwrap();
        assert_eq!(gte.gte, Some(vec![6.5]));
        let lt = parse_modal_number_expr("<6").unwrap();
        assert_eq!(lt.lt, Some(vec![6.0]));
        let eq = parse_modal_number_expr("8.0").unwrap();
        assert_eq!(eq.eq, Some(vec![8.0]));
    }
}
