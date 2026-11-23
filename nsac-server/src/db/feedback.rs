//! Structured user feedback, suggestions, and bug reports (`migrations/0006_feedback.sql`).
//!
//! Anti-garbage rules:
//! - Normalized text length + minimum word count + distinct alphanumeric count + repeat-char guard.
//! - 24h per-account deduplication via SHA-256 over `(account_id, kind, normalized_title, normalized_body)`.
//! - DB-backed rate limits: 1 submission per 10 minutes and max 3 per 24 hours per account.
//! - Screenshots are stored as raw re-encoded JPEG `BYTEA` in `feedback_attachments` (never fetched on list queries).

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use nsac_core::error::ErrorCode;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{PgPool, Postgres, QueryBuilder};

use crate::db::sha256_hex;
use crate::error::ServerError;
use crate::media::SanitizedImage;

pub const FEEDBACK_COOLDOWN_SECS: i64 = 10 * 60;
pub const FEEDBACK_MAX_PER_DAY: i64 = 3;
pub const FEEDBACK_DEDUP_WINDOW_HOURS: i64 = 24;
pub const MAX_BUG_ATTACHMENTS: usize = 3;

pub const ALLOWED_AREAS: &[&str] = &[
    "boletim",
    "ranking",
    "frequencia",
    "sentinela",
    "social",
    "configuracoes",
    "login_update",
    "geral",
];

pub const SUGGESTION_EXTRA_AREA: &str = "nova_funcao";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FeedbackKind {
    Bug,
    Suggestion,
    Feedback,
}

impl FeedbackKind {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim() {
            "bug" => Some(Self::Bug),
            "suggestion" => Some(Self::Suggestion),
            "feedback" => Some(Self::Feedback),
            _ => None,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Bug => "bug",
            Self::Suggestion => "suggestion",
            Self::Feedback => "feedback",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FeedbackStatus {
    Open,
    Triaged,
    Resolved,
    Dismissed,
}

impl FeedbackStatus {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim() {
            "open" => Some(Self::Open),
            "triaged" => Some(Self::Triaged),
            "resolved" => Some(Self::Resolved),
            "dismissed" => Some(Self::Dismissed),
            _ => None,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Triaged => "triaged",
            Self::Resolved => "resolved",
            Self::Dismissed => "dismissed",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BugSeverity {
    Blocker,
    Major,
    Minor,
}

impl BugSeverity {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim() {
            "blocker" => Some(Self::Blocker),
            "major" => Some(Self::Major),
            "minor" => Some(Self::Minor),
            _ => None,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Blocker => "blocker",
            Self::Major => "major",
            Self::Minor => "minor",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BugFrequency {
    Always,
    Sometimes,
    Once,
}

impl BugFrequency {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim() {
            "always" => Some(Self::Always),
            "sometimes" => Some(Self::Sometimes),
            "once" => Some(Self::Once),
            _ => None,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Always => "always",
            Self::Sometimes => "sometimes",
            Self::Once => "once",
        }
    }
}

pub fn is_valid_area(kind: FeedbackKind, area: &str) -> bool {
    ALLOWED_AREAS.contains(&area)
        || (kind == FeedbackKind::Suggestion && area == SUGGESTION_EXTRA_AREA)
}

/// Trims leading/trailing whitespace and collapses internal runs of whitespace into single spaces.
pub fn collapse_whitespace(raw: &str) -> String {
    raw.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Preserves user line breaks (capped at 2 consecutive newlines) while trimming redundant spaces per line.
pub fn clean_multiline(raw: &str) -> String {
    let mut lines: Vec<String> = Vec::new();
    let mut consecutive_empty = 0usize;
    for line in raw.replace("\r\n", "\n").lines() {
        let trimmed = collapse_whitespace(line);
        if trimmed.is_empty() {
            consecutive_empty += 1;
            if !lines.is_empty() && consecutive_empty <= 1 {
                lines.push(String::new());
            }
        } else {
            consecutive_empty = 0;
            lines.push(trimmed);
        }
    }
    while lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    lines.join("\n")
}

/// Validates a structured text field against length, word count, distinct characters, and spam repetition.
pub fn validate_structured_text(
    label: &str,
    raw: &str,
    min_chars: usize,
    max_chars: usize,
    min_words: usize,
    min_distinct_alnum: usize,
) -> Result<String, ServerError> {
    let cleaned = clean_multiline(raw);
    let collapsed = collapse_whitespace(&cleaned);
    let char_count = collapsed.chars().count();

    if char_count < min_chars {
        return Err(ServerError::with_user_message(
            ErrorCode::InvalidParams,
            format!("{label}: escreva pelo menos {min_chars} caracteres com detalhes."),
            format!("Feedback field '{label}' too short ({char_count} < {min_chars})"),
        ));
    }
    if char_count > max_chars {
        return Err(ServerError::with_user_message(
            ErrorCode::InvalidParams,
            format!("{label}: limite máximo de {max_chars} caracteres."),
            format!("Feedback field '{label}' too long ({char_count} > {max_chars})"),
        ));
    }

    let word_count = collapsed
        .split_whitespace()
        .filter(|w| w.chars().any(|c| c.is_alphanumeric()))
        .count();
    if word_count < min_words {
        return Err(ServerError::with_user_message(
            ErrorCode::InvalidParams,
            format!("{label}: escreva pelo menos {min_words} palavras explicando melhor."),
            format!("Feedback field '{label}' too few words ({word_count} < {min_words})"),
        ));
    }

    let mut distinct_alnum = HashSet::new();
    let mut char_freq: HashMap<char, usize> = HashMap::new();
    let mut non_ws_total = 0usize;
    for ch in collapsed.chars().filter(|c| !c.is_whitespace()) {
        non_ws_total += 1;
        for lc in ch.to_lowercase() {
            if lc.is_alphanumeric() {
                distinct_alnum.insert(lc);
            }
            *char_freq.entry(lc).or_insert(0) += 1;
        }
    }

    if distinct_alnum.len() < min_distinct_alnum {
        return Err(ServerError::with_user_message(
            ErrorCode::InvalidParams,
            format!("{label}: texto muito repetitivo ou genérico. Descreva com mais clareza."),
            format!(
                "Feedback field '{label}' low entropy (distinct={})",
                distinct_alnum.len()
            ),
        ));
    }

    if non_ws_total >= 12 {
        let max_single = char_freq.values().copied().max().unwrap_or(0);
        if max_single * 100 > non_ws_total * 60 {
            return Err(ServerError::with_user_message(
                ErrorCode::InvalidParams,
                format!("{label}: texto parece repetição de caracteres. Escreva uma descrição real."),
                format!("Feedback field '{label}' repetitive characters"),
            ));
        }
    }

    Ok(cleaned)
}

pub fn compute_content_hash(account_id: i64, kind: FeedbackKind, title: &str, body_json: &Value) -> String {
    let norm_title = collapse_whitespace(title).to_lowercase();
    let norm_body = collapse_whitespace(&body_json.to_string()).to_lowercase();
    sha256_hex(&format!("{account_id}|{}|{norm_title}|{norm_body}", kind.as_str()))
}

#[derive(Debug, Clone)]
pub struct NewFeedbackSubmission {
    pub account_id: i64,
    pub kind: FeedbackKind,
    pub area: String,
    pub severity: Option<BugSeverity>,
    pub frequency: Option<BugFrequency>,
    pub rating: Option<i16>,
    pub title: String,
    pub body_json: Value,
    pub context_json: Value,
    pub content_hash: String,
    pub attachments: Vec<SanitizedImage>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CreatedFeedback {
    pub id: i64,
    pub kind: FeedbackKind,
    pub status: FeedbackStatus,
    pub area: String,
    pub title: String,
    pub attachment_count: i16,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FeedbackMineItem {
    pub id: i64,
    pub kind: FeedbackKind,
    pub status: FeedbackStatus,
    pub area: String,
    pub severity: Option<BugSeverity>,
    pub frequency: Option<BugFrequency>,
    pub rating: Option<i16>,
    pub title: String,
    pub attachment_count: i16,
    pub dev_note: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AdminAttachmentMeta {
    pub id: i64,
    pub sort_order: i16,
    pub mime_type: String,
    pub width: i32,
    pub height: i32,
    pub size_bytes: i32,
}

#[derive(Debug, Clone, Serialize)]
pub struct AdminFeedbackAuthor {
    pub account_id: i64,
    pub class_code: String,
    pub display_name: Option<String>,
    pub nsac_email: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct AdminFeedbackItem {
    pub id: i64,
    pub kind: FeedbackKind,
    pub status: FeedbackStatus,
    pub area: String,
    pub severity: Option<BugSeverity>,
    pub frequency: Option<BugFrequency>,
    pub rating: Option<i16>,
    pub title: String,
    pub body: Value,
    pub context: Value,
    pub attachment_count: i16,
    pub attachments: Vec<AdminAttachmentMeta>,
    pub dev_note: Option<String>,
    pub author: AdminFeedbackAuthor,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Default, Clone)]
pub struct AdminFeedbackFilter {
    pub status: Option<FeedbackStatus>,
    pub kind: Option<FeedbackKind>,
    pub area: Option<String>,
    pub limit: i64,
    pub offset: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct AdminFeedbackList {
    pub total: i64,
    pub items: Vec<AdminFeedbackItem>,
}

#[derive(Clone)]
pub struct FeedbackRepo {
    pool: PgPool,
}

#[derive(sqlx::FromRow)]
struct AdminSubmissionRow {
    id: i64,
    kind: String,
    status: String,
    area: String,
    severity: Option<String>,
    frequency: Option<String>,
    rating: Option<i16>,
    title: String,
    body_json: Value,
    context_json: Value,
    attachment_count: i16,
    dev_note: Option<String>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    account_id: i64,
    class_code: String,
    nsac_email: String,
    display_name: Option<String>,
}

impl FeedbackRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn create(&self, new_sub: NewFeedbackSubmission) -> Result<CreatedFeedback, ServerError> {
        let mut tx = self.pool.begin().await?;

        // 1) Cooldown check (10 min)
        let last_created: Option<DateTime<Utc>> = sqlx::query_scalar(
            r#"
            SELECT created_at
            FROM feedback_submissions
            WHERE account_id = $1
            ORDER BY created_at DESC
            LIMIT 1
            "#,
        )
        .bind(new_sub.account_id)
        .fetch_optional(&mut *tx)
        .await?;

        if let Some(last) = last_created {
            let elapsed = (Utc::now() - last).num_seconds();
            if elapsed < FEEDBACK_COOLDOWN_SECS {
                let wait_mins = ((FEEDBACK_COOLDOWN_SECS - elapsed) + 59) / 60;
                return Err(ServerError::rate_limited(
                    format!(
                        "Aguarde cerca de {wait_mins} min antes de enviar outro relato."
                    ),
                    "Feedback cooldown active",
                ));
            }
        }

        // 2) Daily cap (max 3 per 24h)
        let recent_24h: i64 = sqlx::query_scalar(
            r#"
            SELECT COUNT(*)
            FROM feedback_submissions
            WHERE account_id = $1 AND created_at > NOW() - INTERVAL '24 hours'
            "#,
        )
        .bind(new_sub.account_id)
        .fetch_one(&mut *tx)
        .await?;

        if recent_24h >= FEEDBACK_MAX_PER_DAY {
            return Err(ServerError::rate_limited(
                format!("Você atingiu o limite de {FEEDBACK_MAX_PER_DAY} envios em 24 horas. Tente novamente amanhã."),
                "Feedback daily cap exceeded",
            ));
        }

        // 3) 24h duplicate suppression
        let duplicate_exists: bool = sqlx::query_scalar(
            r#"
            SELECT EXISTS(
                SELECT 1
                FROM feedback_submissions
                WHERE account_id = $1
                  AND kind = $2
                  AND content_hash = $3
                  AND created_at > NOW() - make_interval(hours => $4)
            )
            "#,
        )
        .bind(new_sub.account_id)
        .bind(new_sub.kind.as_str())
        .bind(&new_sub.content_hash)
        .bind(FEEDBACK_DEDUP_WINDOW_HOURS as i32)
        .fetch_one(&mut *tx)
        .await?;

        if duplicate_exists {
            return Err(ServerError::conflict(
                "Você já enviou um relato idêntico recentemente.",
                "Duplicate feedback submission within 24h",
            ));
        }

        let attachment_count = i16::try_from(new_sub.attachments.len())
            .map_err(|_| ServerError::invalid_params("Too many attachments"))?;

        let (id, created_at): (i64, DateTime<Utc>) = sqlx::query_as(
            r#"
            INSERT INTO feedback_submissions (
                account_id,
                kind,
                status,
                area,
                severity,
                frequency,
                rating,
                title,
                body_json,
                context_json,
                content_hash,
                attachment_count
            )
            VALUES ($1, $2, 'open', $3, $4, $5, $6, $7, $8, $9, $10, $11)
            RETURNING id, created_at
            "#,
        )
        .bind(new_sub.account_id)
        .bind(new_sub.kind.as_str())
        .bind(&new_sub.area)
        .bind(new_sub.severity.map(BugSeverity::as_str))
        .bind(new_sub.frequency.map(BugFrequency::as_str))
        .bind(new_sub.rating)
        .bind(&new_sub.title)
        .bind(&new_sub.body_json)
        .bind(&new_sub.context_json)
        .bind(&new_sub.content_hash)
        .bind(attachment_count)
        .fetch_one(&mut *tx)
        .await?;

        for (idx, img) in new_sub.attachments.into_iter().enumerate() {
            let sort_order = i16::try_from(idx)
                .map_err(|_| ServerError::internal("Attachment index overflow"))?;
            let width = i32::try_from(img.width)
                .map_err(|_| ServerError::internal("Attachment width overflow"))?;
            let height = i32::try_from(img.height)
                .map_err(|_| ServerError::internal("Attachment height overflow"))?;
            let size_bytes = i32::try_from(img.bytes.len())
                .map_err(|_| ServerError::internal("Attachment size overflow"))?;

            sqlx::query(
                r#"
                INSERT INTO feedback_attachments (
                    submission_id,
                    sort_order,
                    mime_type,
                    width,
                    height,
                    size_bytes,
                    sha256,
                    image_bytes
                )
                VALUES ($1, $2, 'image/jpeg', $3, $4, $5, $6, $7)
                "#,
            )
            .bind(id)
            .bind(sort_order)
            .bind(width)
            .bind(height)
            .bind(size_bytes)
            .bind(&img.sha256)
            .bind(&img.bytes)
            .execute(&mut *tx)
            .await?;
        }

        tx.commit().await?;

        Ok(CreatedFeedback {
            id,
            kind: new_sub.kind,
            status: FeedbackStatus::Open,
            area: new_sub.area,
            title: new_sub.title,
            attachment_count,
            created_at,
        })
    }

    pub async fn list_mine(
        &self,
        account_id: i64,
        limit: i64,
    ) -> Result<Vec<FeedbackMineItem>, ServerError> {
        #[allow(clippy::type_complexity)]
        let rows: Vec<(
            i64,
            String,
            String,
            String,
            Option<String>,
            Option<String>,
            Option<i16>,
            String,
            i16,
            Option<String>,
            DateTime<Utc>,
            DateTime<Utc>,
        )> = sqlx::query_as(
            r#"
            SELECT
                id,
                kind,
                status,
                area,
                severity,
                frequency,
                rating,
                title,
                attachment_count,
                dev_note,
                created_at,
                updated_at
            FROM feedback_submissions
            WHERE account_id = $1
            ORDER BY created_at DESC
            LIMIT $2
            "#,
        )
        .bind(account_id)
        .bind(limit.clamp(1, 25))
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .into_iter()
            .filter_map(
                |(
                    id,
                    kind,
                    status,
                    area,
                    severity,
                    frequency,
                    rating,
                    title,
                    attachment_count,
                    dev_note,
                    created_at,
                    updated_at,
                )| {
                    Some(FeedbackMineItem {
                        id,
                        kind: FeedbackKind::parse(&kind)?,
                        status: FeedbackStatus::parse(&status)?,
                        area,
                        severity: severity.as_deref().and_then(BugSeverity::parse),
                        frequency: frequency.as_deref().and_then(BugFrequency::parse),
                        rating,
                        title,
                        attachment_count,
                        dev_note,
                        created_at,
                        updated_at,
                    })
                },
            )
            .collect())
    }

    pub async fn list_admin(&self, filter: AdminFeedbackFilter) -> Result<AdminFeedbackList, ServerError> {
        let mut count_qb: QueryBuilder<'_, Postgres> =
            QueryBuilder::new("SELECT COUNT(*) FROM feedback_submissions fs WHERE TRUE");
        if let Some(status) = filter.status {
            count_qb.push(" AND fs.status = ");
            count_qb.push_bind(status.as_str());
        }
        if let Some(kind) = filter.kind {
            count_qb.push(" AND fs.kind = ");
            count_qb.push_bind(kind.as_str());
        }
        if let Some(ref area) = filter.area {
            count_qb.push(" AND fs.area = ");
            count_qb.push_bind(area);
        }
        let total: i64 = count_qb.build_query_scalar().fetch_one(&self.pool).await?;

        let mut qb: QueryBuilder<'_, Postgres> = QueryBuilder::new(
            r#"
            SELECT
                fs.id,
                fs.kind,
                fs.status,
                fs.area,
                fs.severity,
                fs.frequency,
                fs.rating,
                fs.title,
                fs.body_json,
                fs.context_json,
                fs.attachment_count,
                fs.dev_note,
                fs.created_at,
                fs.updated_at,
                a.id AS account_id,
                a.class_code,
                a.nsac_email,
                ap.display_name
            FROM feedback_submissions fs
            JOIN accounts a ON a.id = fs.account_id
            LEFT JOIN account_profiles ap ON ap.account_id = a.id
            WHERE TRUE
            "#,
        );
        if let Some(status) = filter.status {
            qb.push(" AND fs.status = ");
            qb.push_bind(status.as_str());
        }
        if let Some(kind) = filter.kind {
            qb.push(" AND fs.kind = ");
            qb.push_bind(kind.as_str());
        }
        if let Some(ref area) = filter.area {
            qb.push(" AND fs.area = ");
            qb.push_bind(area);
        }
        qb.push(" ORDER BY fs.created_at DESC LIMIT ");
        qb.push_bind(filter.limit.clamp(1, 100));
        qb.push(" OFFSET ");
        qb.push_bind(filter.offset.max(0));

        let rows: Vec<AdminSubmissionRow> = qb.build_query_as().fetch_all(&self.pool).await?;
        let ids: Vec<i64> = rows.iter().map(|r| r.id).collect();

        let mut att_map: HashMap<i64, Vec<AdminAttachmentMeta>> = HashMap::new();
        if !ids.is_empty() {
            let att_rows: Vec<(i64, i64, i16, String, i32, i32, i32)> = sqlx::query_as(
                r#"
                SELECT id, submission_id, sort_order, mime_type, width, height, size_bytes
                FROM feedback_attachments
                WHERE submission_id = ANY($1)
                ORDER BY submission_id, sort_order ASC
                "#,
            )
            .bind(&ids)
            .fetch_all(&self.pool)
            .await?;

            for (id, submission_id, sort_order, mime_type, width, height, size_bytes) in att_rows {
                att_map
                    .entry(submission_id)
                    .or_default()
                    .push(AdminAttachmentMeta {
                        id,
                        sort_order,
                        mime_type,
                        width,
                        height,
                        size_bytes,
                    });
            }
        }

        let items = rows
            .into_iter()
            .filter_map(|row| Self::map_admin_row(row, &mut att_map))
            .collect();

        Ok(AdminFeedbackList { total, items })
    }

    pub async fn get_admin_item(&self, submission_id: i64) -> Result<Option<AdminFeedbackItem>, ServerError> {
        let row: Option<AdminSubmissionRow> = sqlx::query_as(
            r#"
            SELECT
                fs.id,
                fs.kind,
                fs.status,
                fs.area,
                fs.severity,
                fs.frequency,
                fs.rating,
                fs.title,
                fs.body_json,
                fs.context_json,
                fs.attachment_count,
                fs.dev_note,
                fs.created_at,
                fs.updated_at,
                a.id AS account_id,
                a.class_code,
                a.nsac_email,
                ap.display_name
            FROM feedback_submissions fs
            JOIN accounts a ON a.id = fs.account_id
            LEFT JOIN account_profiles ap ON ap.account_id = a.id
            WHERE fs.id = $1
            "#,
        )
        .bind(submission_id)
        .fetch_optional(&self.pool)
        .await?;

        let Some(row) = row else {
            return Ok(None);
        };

        let att_rows: Vec<(i64, i16, String, i32, i32, i32)> = sqlx::query_as(
            r#"
            SELECT id, sort_order, mime_type, width, height, size_bytes
            FROM feedback_attachments
            WHERE submission_id = $1
            ORDER BY sort_order ASC
            "#,
        )
        .bind(submission_id)
        .fetch_all(&self.pool)
        .await?;

        let mut att_map = HashMap::new();
        att_map.insert(
            submission_id,
            att_rows
                .into_iter()
                .map(|(id, sort_order, mime_type, width, height, size_bytes)| AdminAttachmentMeta {
                    id,
                    sort_order,
                    mime_type,
                    width,
                    height,
                    size_bytes,
                })
                .collect(),
        );

        Ok(Self::map_admin_row(row, &mut att_map))
    }

    pub async fn get_attachment(
        &self,
        submission_id: i64,
        attachment_id: i64,
    ) -> Result<Option<(String, Vec<u8>)>, ServerError> {
        let row: Option<(String, Vec<u8>)> = sqlx::query_as(
            r#"
            SELECT mime_type, image_bytes
            FROM feedback_attachments
            WHERE submission_id = $1 AND id = $2
            "#,
        )
        .bind(submission_id)
        .bind(attachment_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
    }

    pub async fn update_admin(
        &self,
        submission_id: i64,
        status: Option<FeedbackStatus>,
        dev_note: Option<Option<String>>,
    ) -> Result<Option<AdminFeedbackItem>, ServerError> {
        let update_note = dev_note.is_some();
        let note_val = dev_note.flatten();

        let affected = sqlx::query(
            r#"
            UPDATE feedback_submissions
            SET
                status = COALESCE($2, status),
                dev_note = CASE WHEN $3 THEN $4 ELSE dev_note END,
                updated_at = NOW()
            WHERE id = $1
            "#,
        )
        .bind(submission_id)
        .bind(status.map(FeedbackStatus::as_str))
        .bind(update_note)
        .bind(note_val)
        .execute(&self.pool)
        .await?
        .rows_affected();

        if affected == 0 {
            return Ok(None);
        }

        self.get_admin_item(submission_id).await
    }

    fn map_admin_row(
        row: AdminSubmissionRow,
        att_map: &mut HashMap<i64, Vec<AdminAttachmentMeta>>,
    ) -> Option<AdminFeedbackItem> {
        Some(AdminFeedbackItem {
            id: row.id,
            kind: FeedbackKind::parse(&row.kind)?,
            status: FeedbackStatus::parse(&row.status)?,
            area: row.area,
            severity: row.severity.as_deref().and_then(BugSeverity::parse),
            frequency: row.frequency.as_deref().and_then(BugFrequency::parse),
            rating: row.rating,
            title: row.title,
            body: row.body_json,
            context: row.context_json,
            attachment_count: row.attachment_count,
            attachments: att_map.remove(&row.id).unwrap_or_default(),
            dev_note: row.dev_note,
            author: AdminFeedbackAuthor {
                account_id: row.account_id,
                class_code: row.class_code,
                display_name: row.display_name,
                nsac_email: row.nsac_email,
            },
            created_at: row.created_at,
            updated_at: row.updated_at,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_low_effort_and_gibberish() {
        assert!(validate_structured_text("Título", "oi", 10, 100, 2, 5).is_err());
        assert!(validate_structured_text("Título", "aaaaaaaaaaaaaaaaaa", 10, 100, 2, 5).is_err());
        assert!(validate_structured_text("Passos", "a a a a a a a a a a a a", 20, 1200, 4, 8).is_err());
        assert!(validate_structured_text("Passos", "....................", 20, 1200, 4, 8).is_err());
        assert!(validate_structured_text("Passos", "oi                tudo", 20, 1200, 4, 8).is_err());
    }

    #[test]
    fn accepts_clear_structured_descriptions() {
        let res = validate_structured_text(
            "Passos",
            "1. Abri o chat com um amigo\n2. Toquei em GIF e digitei gato\n3. O teclado travou",
            20,
            1200,
            4,
            8,
        );
        assert!(res.is_ok());
    }
}
