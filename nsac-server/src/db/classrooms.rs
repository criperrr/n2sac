//! Class Rooms, Representative Elections (public approval voting), Official Announcements,
//! Group Chats (`chat` & `off_topic`), Zipped Room Media & Notification Preferences.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Datelike, NaiveDate, Utc};
use nsac_core::crypto::CryptoService;
use nsac_core::error::ErrorCode;
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, PgPool, Postgres, Transaction};

use crate::db::ranking::{avatar_path, format_student_label};
use crate::db::social::{ALLOWED_REACTIONS, ChatRateLimitStatus, ChatReaction, gif_message_label};
use crate::error::ServerError;
use crate::media::{ZippedSanitizedImage, unzip_single_file};

/// Room becomes functional once strictly more than 5 students (> 5, i.e. >= 6) of the same class exist.
pub const MIN_MEMBERS_FOR_ACTIVE_ROOM: i64 = 6;
pub const MAX_REPRESENTATIVES: i64 = 3;
pub const MAX_VOTES_PER_STUDENT: i64 = 3;
pub const MAX_PINNED_ANNOUNCEMENTS: i64 = 3;
pub const MAX_ANNOUNCEMENT_TITLE_CHARS: usize = 120;
pub const MAX_ANNOUNCEMENT_BODY_CHARS: usize = 2000;
pub const MAX_ROOM_MESSAGE_CHARS: usize = 500;
pub const ROOM_CHAT_RATE_LIMIT_COUNT: i64 = 12;
pub const ROOM_CHAT_RATE_LIMIT_WINDOW_SECS: i64 = 120;
pub const ROOM_MESSAGE_EDIT_WINDOW_SECS: i64 = 15 * 60;
pub const ANNOUNCEMENT_PUSH_MAX_PER_HOUR: i64 = 3;
pub const ANNOUNCEMENT_PUSH_MAX_PER_DAY: i64 = 8;

/// Adaptive quorum: `clamp(ceil(0.6 * member_count), 3, 10)`.
pub fn adaptive_quorum(member_count: i64) -> i64 {
    if member_count <= 0 {
        return 3;
    }
    let raw = ((member_count as f64) * 0.6).ceil() as i64;
    raw.clamp(3, 10)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClassroomChannelKind {
    Chat,
    OffTopic,
}

impl ClassroomChannelKind {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "chat" => Some(Self::Chat),
            "off_topic" | "off-topic" | "offtopic" => Some(Self::OffTopic),
            _ => None,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Chat => "chat",
            Self::OffTopic => "off_topic",
        }
    }
}

pub const MAX_ANNOUNCEMENT_TAG_CHARS: usize = 20;
pub const MAX_ANNOUNCEMENT_TAG_WORDS: usize = 2;

/// Formats and validates a custom announcement tag:
/// - trims and collapses whitespace
/// - keeps only Unicode letters, numbers, spaces, `-`, `/`, `+`, `&`
/// - maps legacy English slugs to Portuguese display tags
/// - enforces at most 2 words and at most 20 characters total
/// - formats each word in Title Case (preserving short uppercase acronyms like `TCC`, `P1`, `CTI`)
pub fn normalize_announcement_tag(raw: &str) -> Result<String, ServerError> {
    let cleaned: String = raw
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, ' ' | '-' | '/' | '+' | '&') {
                c
            } else {
                ' '
            }
        })
        .collect();

    let words: Vec<&str> = cleaned.split_whitespace().collect();
    if words.is_empty() {
        return Ok("Geral".to_string());
    }
    if words.len() > MAX_ANNOUNCEMENT_TAG_WORDS {
        return Err(user_err(
            ErrorCode::InvalidParams,
            format!("A tag pode ter no máximo {MAX_ANNOUNCEMENT_TAG_WORDS} palavras."),
        ));
    }

    let joined_lower = words.join(" ").to_lowercase();
    let legacy_mapped = match joined_lower.as_str() {
        "general" | "geral" => Some("Geral"),
        "exam" | "prova" => Some("Prova"),
        "assignment" | "trabalho" => Some("Trabalho"),
        "schedule" | "horario" | "horário" => Some("Horário"),
        "event" | "evento" => Some("Evento"),
        _ => None,
    };

    let formatted = if let Some(mapped) = legacy_mapped {
        mapped.to_string()
    } else {
        words
            .into_iter()
            .map(format_tag_word)
            .collect::<Vec<_>>()
            .join(" ")
    };

    if formatted.chars().count() > MAX_ANNOUNCEMENT_TAG_CHARS {
        return Err(user_err(
            ErrorCode::InvalidParams,
            format!("A tag pode ter no máximo {MAX_ANNOUNCEMENT_TAG_CHARS} caracteres."),
        ));
    }

    Ok(formatted)
}

fn format_tag_word(word: &str) -> String {
    let char_count = word.chars().count();
    let has_alpha = word.chars().any(char::is_alphabetic);
    let is_short_acronym = has_alpha
        && (2..=4).contains(&char_count)
        && word.chars().all(|c| !c.is_alphabetic() || c.is_uppercase());
    if is_short_acronym {
        return word.to_string();
    }
    let mut chars = word.chars();
    match chars.next() {
        Some(first) => {
            let mut out = first.to_uppercase().to_string();
            out.push_str(&chars.as_str().to_lowercase());
            out
        }
        None => String::new(),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotificationPreferences {
    pub grades_enabled: bool,
    pub announcements_enabled: bool,
    pub social_enabled: bool,
    pub room_chat_mode: String,
    pub room_offtopic_mode: String,
    pub updated_at: DateTime<Utc>,
}

impl Default for NotificationPreferences {
    fn default() -> Self {
        Self {
            grades_enabled: true,
            announcements_enabled: true,
            social_enabled: true,
            room_chat_mode: "mentions".to_string(),
            room_offtopic_mode: "muted".to_string(),
            updated_at: Utc::now(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ClassroomInfo {
    pub id: i64,
    pub class_code: String,
    pub school_year: i32,
    pub member_count: i64,
    pub min_members_required: i64,
    pub is_functional: bool,
    pub quorum: i64,
    pub max_representatives: i64,
    pub max_votes_per_student: i64,
    pub avatar_url: Option<String>,
    pub banner_url: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RoomVoterChip {
    pub account_id: i64,
    pub display_label: String,
    pub avatar_url: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RoomRepresentativeSummary {
    pub account_id: i64,
    pub display_label: String,
    pub avatar_url: Option<String>,
    pub vote_count: i64,
    pub elected_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ElectionCandidateRow {
    pub rank: usize,
    pub account_id: i64,
    pub is_me: bool,
    pub display_label: String,
    pub nsac_email: String,
    pub avatar_url: Option<String>,
    pub vote_count: i64,
    pub is_representative: bool,
    pub voted_by_me: bool,
    /// Public (non-secret) list of classmates who voted for this candidate.
    pub voters: Vec<RoomVoterChip>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ElectionStatePayload {
    pub room: ClassroomInfo,
    pub am_i_representative: bool,
    pub my_votes: Vec<i64>,
    pub candidates: Vec<ElectionCandidateRow>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RoomChannelPreview {
    pub channel_kind: String,
    pub unread_count: i64,
    pub last_message_id: Option<i64>,
    pub last_message_preview: Option<String>,
    pub last_sender_label: Option<String>,
    pub last_message_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AnnouncementItem {
    pub id: i64,
    pub room_id: i64,
    pub author_id: i64,
    pub author_label: String,
    pub author_avatar_url: Option<String>,
    pub is_mine: bool,
    pub category: String,
    pub title: String,
    pub body: String,
    pub event_date: Option<String>,
    pub pinned: bool,
    pub notify_push: bool,
    pub attachment_url: Option<String>,
    pub read_count: i64,
    pub read_by_me: bool,
    pub reactions: Vec<ChatReaction>,
    pub created_at: DateTime<Utc>,
    pub edited_at: Option<DateTime<Utc>>,
    pub deleted: bool,
    pub rev: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ClassroomOverviewPayload {
    pub room: ClassroomInfo,
    pub am_i_representative: bool,
    pub my_votes: Vec<i64>,
    pub representatives: Vec<RoomRepresentativeSummary>,
    pub unread_announcements: i64,
    pub chat: RoomChannelPreview,
    pub off_topic: RoomChannelPreview,
    pub announcements: Vec<AnnouncementItem>,
    pub next_announcements_rev: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct RoomMessageReplyPreview {
    pub id: i64,
    pub sender_id: i64,
    pub sender_label: String,
    pub is_me: bool,
    pub text: String,
    pub deleted: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ClassroomAnnouncementRef {
    pub id: i64,
    pub title: String,
    pub category: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ClassroomChatMessage {
    pub id: i64,
    pub room_id: i64,
    pub channel_kind: String,
    pub sender_id: i64,
    pub sender_label: String,
    pub sender_avatar_url: Option<String>,
    pub sender_is_representative: bool,
    pub is_me: bool,
    pub text: String,
    pub attachment_url: Option<String>,
    pub created_at: DateTime<Utc>,
    pub edited_at: Option<DateTime<Utc>>,
    pub deleted: bool,
    pub deletion_kind: Option<String>,
    pub reply_to: Option<RoomMessageReplyPreview>,
    pub ref_announcement: Option<ClassroomAnnouncementRef>,
    pub reactions: Vec<ChatReaction>,
    pub rev: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct RoomMessagesPayload {
    pub room: ClassroomInfo,
    pub channel_kind: String,
    pub am_i_representative: bool,
    pub messages: Vec<ClassroomChatMessage>,
    pub rate_limit: ChatRateLimitStatus,
    pub typing_users: Vec<String>,
    pub next_rev: i64,
    pub has_more_before: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct SendRoomMessagePayload {
    pub message: ClassroomChatMessage,
    pub rate_limit: ChatRateLimitStatus,
}

#[derive(Debug, Clone)]
pub struct RoomPushTarget {
    pub account_id: i64,
    pub fcm_token: String,
    pub room_chat_mode: String,
    pub room_offtopic_mode: String,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ClassroomMessageQuery {
    pub after_rev: Option<i64>,
    pub before_id: Option<i64>,
    pub limit: i64,
}

#[derive(Debug, Clone)]
pub struct ResolvedClassroom {
    pub id: i64,
    pub class_code: String,
    pub school_year: i32,
}

#[derive(FromRow)]
struct ClassMemberRow {
    account_id: i64,
    nsac_email: String,
    ranking_display_name: Option<String>,
    avatar_version: Option<i64>,
}

impl ClassMemberRow {
    fn label(&self) -> String {
        format_student_label(
            self.account_id,
            &self.nsac_email,
            false,
            self.ranking_display_name.as_deref(),
            false,
        )
    }

    /// Official student name derived strictly from `nsac_email` (ignores custom nicknames).
    fn real_name(&self) -> String {
        format_student_label(self.account_id, &self.nsac_email, false, None, false)
    }

    fn avatar_url(&self) -> Option<String> {
        self.avatar_version.map(|v| avatar_path(self.account_id, v))
    }
}

#[derive(FromRow)]
struct AnnouncementSqlRow {
    id: i64,
    room_id: i64,
    author_id: i64,
    category: String,
    title: String,
    body_ciphertext: String,
    event_date: Option<NaiveDate>,
    pinned: bool,
    notify_push: bool,
    has_attachment: bool,
    created_at: DateTime<Utc>,
    edited_at: Option<DateTime<Utc>>,
    deleted_at: Option<DateTime<Utc>>,
    rev: i64,
}

#[derive(FromRow)]
struct RoomMessageSqlRow {
    id: i64,
    room_id: i64,
    channel_kind: String,
    sender_id: i64,
    body_ciphertext: String,
    reply_to_id: Option<i64>,
    ref_announcement_id: Option<i64>,
    has_attachment: bool,
    created_at: DateTime<Utc>,
    edited_at: Option<DateTime<Utc>>,
    deleted_at: Option<DateTime<Utc>>,
    deletion_kind: Option<String>,
    rev: i64,
}

#[derive(FromRow)]
struct ReactionSqlRow {
    target_id: i64,
    account_id: i64,
    emoji: String,
}

fn user_err(code: ErrorCode, msg: impl Into<String>) -> ServerError {
    let m = msg.into();
    ServerError::with_user_message(code, m.clone(), m)
}

fn truncate_preview(raw: &str, max_chars: usize) -> String {
    if let Some(label) = gif_message_label(raw) {
        return label.to_string();
    }
    let collapsed = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut chars = collapsed.chars();
    let preview: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{preview}…")
    } else {
        preview
    }
}

#[derive(Clone)]
pub struct ClassroomsRepo {
    pool: PgPool,
}

impl ClassroomsRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Resolves (and idempotently creates) the mandatory class room for an account.
    pub async fn resolve_user_classroom(&self, account_id: i64) -> Result<ResolvedClassroom, ServerError> {
        let raw_class_code: Option<Option<String>> = sqlx::query_scalar(
            "SELECT class_code FROM accounts WHERE id = $1 AND active = TRUE",
        )
        .bind(account_id)
        .fetch_optional(&self.pool)
        .await?;
        let class_code = raw_class_code
            .flatten()
            .map(|s| s.trim().to_ascii_uppercase())
            .filter(|s| !s.is_empty());

        let Some(class_code) = class_code else {
            return Err(user_err(
                ErrorCode::InvalidParams,
                "Sua turma ainda não foi identificada no NSAC. Atualize seu boletim primeiro.",
            ));
        };

        let latest_year: Option<i32> = sqlx::query_scalar(
            r#"
            SELECT MAX(sy.year)
            FROM accounts_school_years asy
            JOIN school_years sy ON sy.id = asy.school_year_id
            WHERE asy.account_id = $1
            "#,
        )
        .bind(account_id)
        .fetch_one(&self.pool)
        .await?;

        let school_year = latest_year.unwrap_or_else(|| Utc::now().year());
        let id = self.ensure_room(&class_code, school_year).await?;
        Ok(ResolvedClassroom {
            id,
            class_code,
            school_year,
        })
    }

    pub async fn ensure_room(&self, class_code: &str, school_year: i32) -> Result<i64, ServerError> {
        let normalized = class_code.trim().to_ascii_uppercase();
        if normalized.is_empty() {
            return Err(user_err(ErrorCode::InvalidParams, "Turma inválida."));
        }
        let id: i64 = sqlx::query_scalar(
            r#"
            INSERT INTO class_rooms (class_code, school_year, created_at, updated_at)
            VALUES ($1, $2, NOW(), NOW())
            ON CONFLICT (class_code, school_year) DO UPDATE SET updated_at = class_rooms.updated_at
            RETURNING id
            "#,
        )
        .bind(&normalized)
        .bind(school_year)
        .fetch_one(&self.pool)
        .await?;
        Ok(id)
    }

    async fn load_class_members(
        &self,
        class_code: &str,
    ) -> Result<Vec<ClassMemberRow>, ServerError> {
        Ok(sqlx::query_as::<_, ClassMemberRow>(
            r#"
            SELECT
                a.id AS account_id,
                a.nsac_email,
                COALESCE(NULLIF(TRIM(ap.display_name), ''), NULLIF(TRIM(rp.display_name), '')) AS ranking_display_name,
                CASE WHEN ap.avatar_data_url IS NOT NULL
                     THEN EXTRACT(EPOCH FROM ap.updated_at)::bigint END AS avatar_version
            FROM accounts a
            LEFT JOIN account_ranking_preferences rp ON rp.account_id = a.id
            LEFT JOIN account_profiles ap ON ap.account_id = a.id
            WHERE a.active = TRUE AND UPPER(a.class_code) = UPPER($1)
            ORDER BY a.id ASC
            "#,
        )
        .bind(class_code)
        .fetch_all(&self.pool)
        .await?)
    }

    async fn build_room_info(
        &self,
        room: &ResolvedClassroom,
        member_count: i64,
    ) -> Result<ClassroomInfo, ServerError> {
        let media_versions: Option<(Option<i64>, Option<i64>)> = sqlx::query_as(
            r#"
            SELECT
                CASE WHEN avatar_zip IS NOT NULL THEN EXTRACT(EPOCH FROM COALESCE(avatar_updated_at, updated_at))::bigint END,
                CASE WHEN banner_zip IS NOT NULL THEN EXTRACT(EPOCH FROM COALESCE(banner_updated_at, updated_at))::bigint END
            FROM class_room_media
            WHERE room_id = $1
            "#,
        )
        .bind(room.id)
        .fetch_optional(&self.pool)
        .await?;

        let (av, bv) = media_versions.unwrap_or((None, None));
        Ok(ClassroomInfo {
            id: room.id,
            class_code: room.class_code.clone(),
            school_year: room.school_year,
            member_count,
            min_members_required: MIN_MEMBERS_FOR_ACTIVE_ROOM,
            is_functional: member_count >= MIN_MEMBERS_FOR_ACTIVE_ROOM,
            quorum: adaptive_quorum(member_count),
            max_representatives: MAX_REPRESENTATIVES,
            max_votes_per_student: MAX_VOTES_PER_STUDENT,
            avatar_url: av.map(|v| format!("/api/v1/classrooms/me/avatar?v={v}")),
            banner_url: bv.map(|v| format!("/api/v1/classrooms/me/banner?v={v}")),
        })
    }

    pub async fn is_representative(
        &self,
        room_id: i64,
        account_id: i64,
    ) -> Result<bool, ServerError> {
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM class_representatives WHERE room_id = $1 AND account_id = $2)",
        )
        .bind(room_id)
        .bind(account_id)
        .fetch_one(&self.pool)
        .await?;
        Ok(exists)
    }

    async fn require_functional_room(&self, room: &ResolvedClassroom) -> Result<i64, ServerError> {
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*)::bigint FROM accounts WHERE active = TRUE AND UPPER(class_code) = UPPER($1)",
        )
        .bind(&room.class_code)
        .fetch_one(&self.pool)
        .await?;
        if count < MIN_MEMBERS_FOR_ACTIVE_ROOM {
            let missing = MIN_MEMBERS_FOR_ACTIVE_ROOM - count;
            return Err(user_err(
                ErrorCode::Forbidden,
                format!(
                    "O canal da turma {} precisa de pelo menos {} alunos cadastrados para funcionar (faltam {}).",
                    room.class_code, MIN_MEMBERS_FOR_ACTIVE_ROOM, missing
                ),
            ));
        }
        Ok(count)
    }

    async fn require_representative(
        &self,
        room: &ResolvedClassroom,
        account_id: i64,
    ) -> Result<(), ServerError> {
        self.require_functional_room(room).await?;
        if !self.is_representative(room.id, account_id).await? {
            return Err(user_err(
                ErrorCode::Forbidden,
                "Apenas representantes de classe eleitos podem realizar esta ação.",
            ));
        }
        Ok(())
    }

    /// Recalculates the active Top-3 representatives atomically inside a transaction.
    async fn recalculate_representatives(
        tx: &mut Transaction<'_, Postgres>,
        room_id: i64,
        class_code: &str,
        quorum: i64,
    ) -> Result<(), ServerError> {
        // Count valid votes where both voter and candidate are active members of the room's class.
        let tallies: Vec<(i64, i64, DateTime<Utc>)> = sqlx::query_as(
            r#"
            SELECT
                v.candidate_id,
                COUNT(*)::bigint AS vote_count,
                MAX(v.created_at) AS last_vote_at
            FROM class_representative_votes v
            JOIN accounts voter ON voter.id = v.voter_id AND voter.active = TRUE AND UPPER(voter.class_code) = UPPER($2)
            JOIN accounts cand ON cand.id = v.candidate_id AND cand.active = TRUE AND UPPER(cand.class_code) = UPPER($2)
            WHERE v.room_id = $1
            GROUP BY v.candidate_id
            HAVING COUNT(*) >= $3
            ORDER BY COUNT(*) DESC, MAX(v.created_at) ASC, v.candidate_id ASC
            LIMIT $4
            "#,
        )
        .bind(room_id)
        .bind(class_code)
        .bind(quorum)
        .bind(MAX_REPRESENTATIVES)
        .fetch_all(&mut **tx)
        .await?;

        // Preserve existing `elected_at` / `reached_threshold_at` for incumbents.
        let existing: Vec<(i64, DateTime<Utc>, DateTime<Utc>)> = sqlx::query_as(
            "SELECT account_id, reached_threshold_at, elected_at FROM class_representatives WHERE room_id = $1",
        )
        .bind(room_id)
        .fetch_all(&mut **tx)
        .await?;
        let existing_map: HashMap<i64, (DateTime<Utc>, DateTime<Utc>)> = existing
            .into_iter()
            .map(|(id, r, e)| (id, (r, e)))
            .collect();

        sqlx::query("DELETE FROM class_representatives WHERE room_id = $1")
            .bind(room_id)
            .execute(&mut **tx)
            .await?;

        for (cand_id, vote_count, last_vote_at) in tallies {
            let (reached_at, elected_at) = existing_map
                .get(&cand_id)
                .copied()
                .unwrap_or((last_vote_at, Utc::now()));
            sqlx::query(
                r#"
                INSERT INTO class_representatives (room_id, account_id, vote_count, reached_threshold_at, elected_at)
                VALUES ($1, $2, $3, $4, $5)
                "#,
            )
            .bind(room_id)
            .bind(cand_id)
            .bind(vote_count as i32)
            .bind(reached_at)
            .bind(elected_at)
            .execute(&mut **tx)
            .await?;
        }

        Ok(())
    }

    pub async fn get_election_state(
        &self,
        viewer_id: i64,
    ) -> Result<ElectionStatePayload, ServerError> {
        let room = self.resolve_user_classroom(viewer_id).await?;
        let members = self.load_class_members(&room.class_code).await?;
        let member_count = members.len() as i64;
        let room_info = self.build_room_info(&room, member_count).await?;

        let member_map: HashMap<i64, &ClassMemberRow> =
            members.iter().map(|m| (m.account_id, m)).collect();

        let votes: Vec<(i64, i64)> = sqlx::query_as(
            r#"
            SELECT v.voter_id, v.candidate_id
            FROM class_representative_votes v
            WHERE v.room_id = $1
            ORDER BY v.created_at ASC
            "#,
        )
        .bind(room.id)
        .fetch_all(&self.pool)
        .await?;

        let rep_ids: Vec<i64> = sqlx::query_scalar(
            "SELECT account_id FROM class_representatives WHERE room_id = $1",
        )
        .bind(room.id)
        .fetch_all(&self.pool)
        .await?;
        let rep_set: HashSet<i64> = rep_ids.into_iter().collect();

        let mut voters_by_candidate: HashMap<i64, Vec<RoomVoterChip>> = HashMap::new();
        let mut my_votes = Vec::new();
        for (voter_id, cand_id) in votes {
            let (Some(voter_row), Some(_)) = (member_map.get(&voter_id), member_map.get(&cand_id))
            else {
                continue;
            };
            if voter_id == viewer_id {
                my_votes.push(cand_id);
            }
            voters_by_candidate
                .entry(cand_id)
                .or_default()
                .push(RoomVoterChip {
                    account_id: voter_row.account_id,
                    display_label: voter_row.real_name(),
                    avatar_url: voter_row.avatar_url(),
                });
        }

        let my_vote_set: HashSet<i64> = my_votes.iter().copied().collect();
        let mut candidates: Vec<ElectionCandidateRow> = members
            .iter()
            .map(|m| {
                let voters = voters_by_candidate
                    .remove(&m.account_id)
                    .unwrap_or_default();
                let vote_count = voters.len() as i64;
                ElectionCandidateRow {
                    rank: 0,
                    account_id: m.account_id,
                    is_me: m.account_id == viewer_id,
                    display_label: m.real_name(),
                    nsac_email: m.nsac_email.clone(),
                    avatar_url: m.avatar_url(),
                    vote_count,
                    is_representative: rep_set.contains(&m.account_id),
                    voted_by_me: my_vote_set.contains(&m.account_id),
                    voters,
                }
            })
            .collect();

        candidates.sort_by(|a, b| {
            b.is_representative
                .cmp(&a.is_representative)
                .then_with(|| b.vote_count.cmp(&a.vote_count))
                .then_with(|| a.display_label.cmp(&b.display_label))
        });
        for (idx, row) in candidates.iter_mut().enumerate() {
            row.rank = idx + 1;
        }

        Ok(ElectionStatePayload {
            room: room_info,
            am_i_representative: rep_set.contains(&viewer_id),
            my_votes,
            candidates,
        })
    }

    pub async fn cast_vote(
        &self,
        voter_id: i64,
        candidate_id: i64,
    ) -> Result<ElectionStatePayload, ServerError> {
        if voter_id == candidate_id {
            return Err(user_err(
                ErrorCode::InvalidParams,
                "Você não pode votar em si mesmo como representante.",
            ));
        }

        let room = self.resolve_user_classroom(voter_id).await?;
        let member_count = self.require_functional_room(&room).await?;
        let quorum = adaptive_quorum(member_count);

        let same_class: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM accounts WHERE id = $1 AND active = TRUE AND UPPER(class_code) = UPPER($2))",
        )
        .bind(candidate_id)
        .bind(&room.class_code)
        .fetch_one(&self.pool)
        .await?;
        if !same_class {
            return Err(user_err(
                ErrorCode::Forbidden,
                "Você só pode votar em colegas da sua própria turma.",
            ));
        }

        let mut tx = self.pool.begin().await?;
        // Serialize election mutations per room to prevent race conditions on Top-3 / max-3 votes.
        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(room.id)
            .execute(&mut *tx)
            .await?;

        let already_voted: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM class_representative_votes WHERE room_id = $1 AND voter_id = $2 AND candidate_id = $3)",
        )
        .bind(room.id)
        .bind(voter_id)
        .bind(candidate_id)
        .fetch_one(&mut *tx)
        .await?;

        if !already_voted {
            let current_votes: i64 = sqlx::query_scalar(
                "SELECT COUNT(*)::bigint FROM class_representative_votes WHERE room_id = $1 AND voter_id = $2",
            )
            .bind(room.id)
            .bind(voter_id)
            .fetch_one(&mut *tx)
            .await?;

            if current_votes >= MAX_VOTES_PER_STUDENT {
                return Err(user_err(
                    ErrorCode::InvalidParams,
                    format!(
                        "Você já usou seus {MAX_VOTES_PER_STUDENT} votos. Retire um voto antes de indicar outro colega."
                    ),
                ));
            }

            sqlx::query(
                r#"
                INSERT INTO class_representative_votes (room_id, voter_id, candidate_id, created_at)
                VALUES ($1, $2, $3, NOW())
                ON CONFLICT DO NOTHING
                "#,
            )
            .bind(room.id)
            .bind(voter_id)
            .bind(candidate_id)
            .execute(&mut *tx)
            .await?;
        }

        Self::recalculate_representatives(&mut tx, room.id, &room.class_code, quorum).await?;
        tx.commit().await?;

        self.get_election_state(voter_id).await
    }

    pub async fn revoke_vote(
        &self,
        voter_id: i64,
        candidate_id: i64,
    ) -> Result<ElectionStatePayload, ServerError> {
        let room = self.resolve_user_classroom(voter_id).await?;
        let member_count = self.require_functional_room(&room).await?;
        let quorum = adaptive_quorum(member_count);

        let mut tx = self.pool.begin().await?;
        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(room.id)
            .execute(&mut *tx)
            .await?;

        sqlx::query(
            "DELETE FROM class_representative_votes WHERE room_id = $1 AND voter_id = $2 AND candidate_id = $3",
        )
        .bind(room.id)
        .bind(voter_id)
        .bind(candidate_id)
        .execute(&mut *tx)
        .await?;

        Self::recalculate_representatives(&mut tx, room.id, &room.class_code, quorum).await?;
        tx.commit().await?;

        self.get_election_state(voter_id).await
    }

    /// Updates room avatar and/or banner (zipped JPEG). Representatives only.
    pub async fn update_classroom_appearance(
        &self,
        actor_id: i64,
        avatar_zip: Option<Option<Vec<u8>>>,
        banner_zip: Option<Option<Vec<u8>>>,
    ) -> Result<ClassroomInfo, ServerError> {
        let room = self.resolve_user_classroom(actor_id).await?;
        self.require_representative(&room, actor_id).await?;

        sqlx::query(
            "INSERT INTO class_room_media (room_id, updated_by_id, updated_at) VALUES ($1, $2, NOW()) ON CONFLICT (room_id) DO NOTHING",
        )
        .bind(room.id)
        .bind(actor_id)
        .execute(&self.pool)
        .await?;

        if let Some(opt_av) = avatar_zip {
            sqlx::query(
                "UPDATE class_room_media SET avatar_zip = $2, avatar_updated_at = NOW(), updated_by_id = $3, updated_at = NOW() WHERE room_id = $1",
            )
            .bind(room.id)
            .bind(opt_av)
            .bind(actor_id)
            .execute(&self.pool)
            .await?;
        }
        if let Some(opt_bn) = banner_zip {
            sqlx::query(
                "UPDATE class_room_media SET banner_zip = $2, banner_updated_at = NOW(), updated_by_id = $3, updated_at = NOW() WHERE room_id = $1",
            )
            .bind(room.id)
            .bind(opt_bn)
            .bind(actor_id)
            .execute(&self.pool)
            .await?;
        }

        let member_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*)::bigint FROM accounts WHERE active = TRUE AND UPPER(class_code) = UPPER($1)",
        )
        .bind(&room.class_code)
        .fetch_one(&self.pool)
        .await?;

        self.build_room_info(&room, member_count).await
    }

    pub async fn get_classroom_media_jpeg(
        &self,
        viewer_id: i64,
        is_banner: bool,
    ) -> Result<Vec<u8>, ServerError> {
        let room = self.resolve_user_classroom(viewer_id).await?;
        let col = if is_banner { "banner_zip" } else { "avatar_zip" };
        let sql = format!("SELECT {col} FROM class_room_media WHERE room_id = $1");
        let zip_opt: Option<Option<Vec<u8>>> = sqlx::query_scalar(&sql)
            .bind(room.id)
            .fetch_optional(&self.pool)
            .await?;
        let Some(zip_bytes) = zip_opt.flatten() else {
            return Err(ServerError::not_found("Room media not found"));
        };
        unzip_single_file(&zip_bytes)
    }

    pub async fn get_classroom_overview(
        &self,
        viewer_id: i64,
        crypto: &CryptoService,
    ) -> Result<ClassroomOverviewPayload, ServerError> {
        let room = self.resolve_user_classroom(viewer_id).await?;
        let members = self.load_class_members(&room.class_code).await?;
        let member_count = members.len() as i64;
        let room_info = self.build_room_info(&room, member_count).await?;
        let member_map: HashMap<i64, &ClassMemberRow> =
            members.iter().map(|m| (m.account_id, m)).collect();

        let rep_rows: Vec<(i64, i32, DateTime<Utc>)> = sqlx::query_as(
            r#"
            SELECT account_id, vote_count, elected_at
            FROM class_representatives
            WHERE room_id = $1
            ORDER BY vote_count DESC, reached_threshold_at ASC, account_id ASC
            "#,
        )
        .bind(room.id)
        .fetch_all(&self.pool)
        .await?;

        let mut am_i_representative = false;
        let representatives: Vec<RoomRepresentativeSummary> = rep_rows
            .into_iter()
            .filter_map(|(acc_id, vote_count, elected_at)| {
                if acc_id == viewer_id {
                    am_i_representative = true;
                }
                let m = member_map.get(&acc_id)?;
                Some(RoomRepresentativeSummary {
                    account_id: acc_id,
                    display_label: m.label(),
                    avatar_url: m.avatar_url(),
                    vote_count: i64::from(vote_count),
                    elected_at,
                })
            })
            .collect();

        let my_votes: Vec<i64> = sqlx::query_scalar(
            "SELECT candidate_id FROM class_representative_votes WHERE room_id = $1 AND voter_id = $2",
        )
        .bind(room.id)
        .bind(viewer_id)
        .fetch_all(&self.pool)
        .await?;

        let unread_announcements: i64 = sqlx::query_scalar(
            r#"
            SELECT COUNT(*)::bigint
            FROM class_announcements a
            WHERE a.room_id = $1 AND a.deleted_at IS NULL
              AND NOT EXISTS (
                  SELECT 1 FROM class_announcement_reads r
                  WHERE r.announcement_id = a.id AND r.account_id = $2
              )
            "#,
        )
        .bind(room.id)
        .bind(viewer_id)
        .fetch_one(&self.pool)
        .await?;

        let (announcements, next_announcements_rev) = self
            .load_announcements_inner(room.id, viewer_id, &member_map, None, None, None, 25, crypto)
            .await?;

        let chat = self
            .load_channel_preview(room.id, viewer_id, ClassroomChannelKind::Chat, &member_map, crypto)
            .await?;
        let off_topic = self
            .load_channel_preview(
                room.id,
                viewer_id,
                ClassroomChannelKind::OffTopic,
                &member_map,
                crypto,
            )
            .await?;

        Ok(ClassroomOverviewPayload {
            room: room_info,
            am_i_representative,
            my_votes,
            representatives,
            unread_announcements,
            chat,
            off_topic,
            announcements,
            next_announcements_rev,
        })
    }

    async fn load_channel_preview(
        &self,
        room_id: i64,
        viewer_id: i64,
        kind: ClassroomChannelKind,
        member_map: &HashMap<i64, &ClassMemberRow>,
        crypto: &CryptoService,
    ) -> Result<RoomChannelPreview, ServerError> {
        let last_read: i64 = sqlx::query_scalar(
            "SELECT last_read_message_id FROM room_channel_read_state WHERE account_id = $1 AND room_id = $2 AND channel_kind = $3",
        )
        .bind(viewer_id)
        .bind(room_id)
        .bind(kind.as_str())
        .fetch_optional(&self.pool)
        .await?
        .unwrap_or(0);

        let unread_count: i64 = sqlx::query_scalar(
            r#"
            SELECT COUNT(*)::bigint
            FROM room_messages
            WHERE room_id = $1 AND channel_kind = $2 AND id > $3 AND sender_id <> $4 AND deleted_at IS NULL
            "#,
        )
        .bind(room_id)
        .bind(kind.as_str())
        .bind(last_read)
        .bind(viewer_id)
        .fetch_one(&self.pool)
        .await?;

        #[allow(clippy::type_complexity)]
        let last_msg: Option<(i64, i64, String, bool, Option<DateTime<Utc>>, DateTime<Utc>)> =
            sqlx::query_as(
                r#"
                SELECT id, sender_id, body_ciphertext, has_attachment, deleted_at, created_at
                FROM room_messages
                WHERE room_id = $1 AND channel_kind = $2
                ORDER BY id DESC
                LIMIT 1
                "#,
            )
            .bind(room_id)
            .bind(kind.as_str())
            .fetch_optional(&self.pool)
            .await?;

        let (last_message_id, last_message_preview, last_sender_label, last_message_at) =
            match last_msg {
                Some((id, sender_id, cipher, has_att, deleted_at, created_at)) => {
                    let preview = if deleted_at.is_some() {
                        Some("Mensagem removida".to_string())
                    } else if let Ok(plain) = crypto.decrypt(&cipher) {
                        let txt = plain.as_str().trim();
                        if txt.is_empty() && has_att {
                            Some("📷 Imagem".to_string())
                        } else {
                            Some(truncate_preview(txt, 72))
                        }
                    } else {
                        None
                    };
                    let sender_label = member_map
                        .get(&sender_id)
                        .map(|m| m.label())
                        .unwrap_or_else(|| format!("Aluno #{sender_id}"));
                    (Some(id), preview, Some(sender_label), Some(created_at))
                }
                None => (None, None, None, None),
            };

        Ok(RoomChannelPreview {
            channel_kind: kind.as_str().to_string(),
            unread_count,
            last_message_id,
            last_message_preview,
            last_sender_label,
            last_message_at,
        })
    }

    #[allow(clippy::too_many_arguments)]
    async fn load_announcements_inner(
        &self,
        room_id: i64,
        viewer_id: i64,
        member_map: &HashMap<i64, &ClassMemberRow>,
        category: Option<&str>,
        before_id: Option<i64>,
        after_rev: Option<i64>,
        limit: i64,
        crypto: &CryptoService,
    ) -> Result<(Vec<AnnouncementItem>, i64), ServerError> {
        let capped = limit.clamp(1, 50);
        let max_rev: i64 = sqlx::query_scalar(
            "SELECT COALESCE(MAX(rev), 0)::bigint FROM class_announcements WHERE room_id = $1",
        )
        .bind(room_id)
        .fetch_one(&self.pool)
        .await?;

        let rows: Vec<AnnouncementSqlRow> = if let Some(after) = after_rev {
            sqlx::query_as::<_, AnnouncementSqlRow>(
                r#"
                SELECT id, room_id, author_id, category, title, body_ciphertext, event_date,
                       pinned, notify_push, has_attachment, created_at, edited_at, deleted_at, rev
                FROM class_announcements
                WHERE room_id = $1 AND rev > $2
                ORDER BY rev ASC
                LIMIT $3
                "#,
            )
            .bind(room_id)
            .bind(after)
            .bind(capped)
            .fetch_all(&self.pool)
            .await?
        } else {
            sqlx::query_as::<_, AnnouncementSqlRow>(
                r#"
                SELECT id, room_id, author_id, category, title, body_ciphertext, event_date,
                       pinned, notify_push, has_attachment, created_at, edited_at, deleted_at, rev
                FROM class_announcements
                WHERE room_id = $1
                  AND deleted_at IS NULL
                  AND ($2::text IS NULL OR LOWER(category) = LOWER($2))
                  AND ($3::bigint IS NULL OR id < $3)
                ORDER BY pinned DESC, id DESC
                LIMIT $4
                "#,
            )
            .bind(room_id)
            .bind(category)
            .bind(before_id)
            .bind(capped)
            .fetch_all(&self.pool)
            .await?
        };

        if rows.is_empty() {
            return Ok((Vec::new(), max_rev));
        }

        let ids: Vec<i64> = rows.iter().map(|r| r.id).collect();
        let reactions = sqlx::query_as::<_, ReactionSqlRow>(
            r#"
            SELECT announcement_id AS target_id, account_id, emoji
            FROM class_announcement_reactions
            WHERE announcement_id = ANY($1)
            ORDER BY created_at ASC
            "#,
        )
        .bind(&ids)
        .fetch_all(&self.pool)
        .await?;

        let read_counts: Vec<(i64, i64, bool)> = sqlx::query_as(
            r#"
            SELECT
                announcement_id,
                COUNT(*)::bigint AS total_reads,
                BOOL_OR(account_id = $2) AS read_by_me
            FROM class_announcement_reads
            WHERE announcement_id = ANY($1)
            GROUP BY announcement_id
            "#,
        )
        .bind(&ids)
        .bind(viewer_id)
        .fetch_all(&self.pool)
        .await?;
        let read_map: HashMap<i64, (i64, bool)> = read_counts
            .into_iter()
            .map(|(id, cnt, me)| (id, (cnt, me)))
            .collect();

        let mut items = Vec::with_capacity(rows.len());
        for r in rows {
            let deleted = r.deleted_at.is_some();
            let body = if deleted {
                String::new()
            } else {
                crypto
                    .decrypt(&r.body_ciphertext)
                    .map(|p| p.as_str().to_string())
                    .unwrap_or_default()
            };
            let mut grouped: Vec<ChatReaction> = Vec::new();
            if !deleted {
                for re in reactions.iter().filter(|re| re.target_id == r.id) {
                    match grouped.iter_mut().find(|g| g.emoji == re.emoji) {
                        Some(g) => {
                            g.count += 1;
                            g.mine |= re.account_id == viewer_id;
                        }
                        None => grouped.push(ChatReaction {
                            emoji: re.emoji.clone(),
                            count: 1,
                            mine: re.account_id == viewer_id,
                        }),
                    }
                }
            }
            let (read_count, read_by_me) = read_map.get(&r.id).copied().unwrap_or((0, false));
            let author = member_map.get(&r.author_id);
            items.push(AnnouncementItem {
                id: r.id,
                room_id: r.room_id,
                author_id: r.author_id,
                author_label: author
                    .map(|m| m.label())
                    .unwrap_or_else(|| format!("Representante #{}", r.author_id)),
                author_avatar_url: author.and_then(|m| m.avatar_url()),
                is_mine: r.author_id == viewer_id,
                category: r.category,
                title: if deleted { String::new() } else { r.title },
                body,
                event_date: r.event_date.map(|d| d.format("%Y-%m-%d").to_string()),
                pinned: r.pinned,
                notify_push: r.notify_push,
                attachment_url: if !deleted && r.has_attachment {
                    Some(format!("/api/v1/classrooms/me/announcements/{}/attachment", r.id))
                } else {
                    None
                },
                read_count,
                read_by_me,
                reactions: grouped,
                created_at: r.created_at,
                edited_at: r.edited_at,
                deleted,
                rev: r.rev,
            });
        }

        Ok((items, max_rev))
    }

    pub async fn list_announcements(
        &self,
        viewer_id: i64,
        category: Option<&str>,
        before_id: Option<i64>,
        after_rev: Option<i64>,
        limit: i64,
        crypto: &CryptoService,
    ) -> Result<(Vec<AnnouncementItem>, i64), ServerError> {
        let room = self.resolve_user_classroom(viewer_id).await?;
        let members = self.load_class_members(&room.class_code).await?;
        let member_map: HashMap<i64, &ClassMemberRow> =
            members.iter().map(|m| (m.account_id, m)).collect();
        self.load_announcements_inner(
            room.id,
            viewer_id,
            &member_map,
            category,
            before_id,
            after_rev,
            limit,
            crypto,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn create_announcement(
        &self,
        author_id: i64,
        category: &str,
        title: &str,
        body: &str,
        event_date: Option<NaiveDate>,
        pinned: bool,
        notify_push: bool,
        attachment: Option<ZippedSanitizedImage>,
        crypto: &CryptoService,
    ) -> Result<(ResolvedClassroom, AnnouncementItem), ServerError> {
        let room = self.resolve_user_classroom(author_id).await?;
        self.require_representative(&room, author_id).await?;

        let clean_title = title.trim();
        let clean_body = body.trim();
        if clean_body.is_empty() && attachment.is_none() {
            return Err(user_err(
                ErrorCode::InvalidParams,
                "Escreva o texto do anúncio ou adicione uma imagem.",
            ));
        }
        if clean_title.chars().count() > MAX_ANNOUNCEMENT_TITLE_CHARS {
            return Err(user_err(
                ErrorCode::InvalidParams,
                format!("O título pode ter no máximo {MAX_ANNOUNCEMENT_TITLE_CHARS} caracteres."),
            ));
        }
        if clean_body.chars().count() > MAX_ANNOUNCEMENT_BODY_CHARS {
            return Err(user_err(
                ErrorCode::InvalidParams,
                format!("O anúncio pode ter no máximo {MAX_ANNOUNCEMENT_BODY_CHARS} caracteres."),
            ));
        }

        if notify_push {
            let (hour_cnt, day_cnt): (i64, i64) = sqlx::query_as(
                r#"
                SELECT
                    COUNT(*) FILTER (WHERE created_at > NOW() - INTERVAL '1 hour')::bigint,
                    COUNT(*) FILTER (WHERE created_at > NOW() - INTERVAL '1 day')::bigint
                FROM class_announcements
                WHERE room_id = $1 AND notify_push = TRUE AND deleted_at IS NULL
                "#,
            )
            .bind(room.id)
            .fetch_one(&self.pool)
            .await?;
            if hour_cnt >= ANNOUNCEMENT_PUSH_MAX_PER_HOUR {
                return Err(ServerError::rate_limited(
                    "Limite de 3 anúncios com notificação por hora atingido. Você ainda pode publicar como aviso silencioso.",
                    "Announcement hourly push limit",
                ));
            }
            if day_cnt >= ANNOUNCEMENT_PUSH_MAX_PER_DAY {
                return Err(ServerError::rate_limited(
                    "Limite diário de 8 anúncios com notificação atingido. Você ainda pode publicar como aviso silencioso.",
                    "Announcement daily push limit",
                ));
            }
        }

        if pinned {
            let pinned_cnt: i64 = sqlx::query_scalar(
                "SELECT COUNT(*)::bigint FROM class_announcements WHERE room_id = $1 AND pinned = TRUE AND deleted_at IS NULL",
            )
            .bind(room.id)
            .fetch_one(&self.pool)
            .await?;
            if pinned_cnt >= MAX_PINNED_ANNOUNCEMENTS {
                return Err(user_err(
                    ErrorCode::InvalidParams,
                    format!("A turma já tem {MAX_PINNED_ANNOUNCEMENTS} anúncios fixados. Desafixe um antes."),
                ));
            }
        }

        let cipher = crypto.encrypt(clean_body)?;
        let has_attachment = attachment.is_some();

        let mut tx = self.pool.begin().await?;
        let row = sqlx::query_as::<_, AnnouncementSqlRow>(
            r#"
            INSERT INTO class_announcements (
                room_id, author_id, category, title, body_ciphertext, event_date,
                pinned, notify_push, has_attachment, created_at, rev
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, NOW(), nextval('room_events_rev_seq'))
            RETURNING id, room_id, author_id, category, title, body_ciphertext, event_date,
                      pinned, notify_push, has_attachment, created_at, edited_at, deleted_at, rev
            "#,
        )
        .bind(room.id)
        .bind(author_id)
        .bind(category)
        .bind(clean_title)
        .bind(&cipher)
        .bind(event_date)
        .bind(pinned)
        .bind(notify_push)
        .bind(has_attachment)
        .fetch_one(&mut *tx)
        .await?;

        if let Some(att) = attachment {
            sqlx::query(
                r#"
                INSERT INTO class_announcement_attachments (
                    announcement_id, mime_type, width, height, size_bytes, sha256, zip_bytes, created_at
                )
                VALUES ($1, 'image/jpeg', $2, $3, $4, $5, $6, NOW())
                "#,
            )
            .bind(row.id)
            .bind(att.width as i32)
            .bind(att.height as i32)
            .bind(att.raw_jpeg_bytes.len() as i32)
            .bind(&att.sha256)
            .bind(&att.zip_bytes)
            .execute(&mut *tx)
            .await?;
        }

        // Author automatically marks own announcement as read.
        sqlx::query(
            "INSERT INTO class_announcement_reads (announcement_id, account_id, read_at) VALUES ($1, $2, NOW()) ON CONFLICT DO NOTHING",
        )
        .bind(row.id)
        .bind(author_id)
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;

        let members = self.load_class_members(&room.class_code).await?;
        let member_map: HashMap<i64, &ClassMemberRow> =
            members.iter().map(|m| (m.account_id, m)).collect();
        let author = member_map.get(&author_id);

        let item = AnnouncementItem {
            id: row.id,
            room_id: row.room_id,
            author_id: row.author_id,
            author_label: author
                .map(|m| m.label())
                .unwrap_or_else(|| format!("Representante #{author_id}")),
            author_avatar_url: author.and_then(|m| m.avatar_url()),
            is_mine: true,
            category: row.category,
            title: row.title,
            body: clean_body.to_string(),
            event_date: row.event_date.map(|d| d.format("%Y-%m-%d").to_string()),
            pinned: row.pinned,
            notify_push: row.notify_push,
            attachment_url: if has_attachment {
                Some(format!("/api/v1/classrooms/me/announcements/{}/attachment", row.id))
            } else {
                None
            },
            read_count: 1,
            read_by_me: true,
            reactions: Vec::new(),
            created_at: row.created_at,
            edited_at: row.edited_at,
            deleted: false,
            rev: row.rev,
        };

        Ok((room, item))
    }

    pub async fn update_announcement(
        &self,
        actor_id: i64,
        announcement_id: i64,
        title: Option<&str>,
        body: Option<&str>,
        pinned: Option<bool>,
        crypto: &CryptoService,
    ) -> Result<(ResolvedClassroom, AnnouncementItem), ServerError> {
        let room = self.resolve_user_classroom(actor_id).await?;
        self.require_representative(&room, actor_id).await?;

        let existing = sqlx::query_as::<_, AnnouncementSqlRow>(
            r#"
            SELECT id, room_id, author_id, category, title, body_ciphertext, event_date,
                   pinned, notify_push, has_attachment, created_at, edited_at, deleted_at, rev
            FROM class_announcements
            WHERE id = $1 AND room_id = $2 AND deleted_at IS NULL
            "#,
        )
        .bind(announcement_id)
        .bind(room.id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| user_err(ErrorCode::NotFound, "Anúncio não encontrado."))?;

        if (title.is_some() || body.is_some()) && existing.author_id != actor_id {
            return Err(user_err(
                ErrorCode::Forbidden,
                "Apenas o representante autor pode editar o texto do anúncio.",
            ));
        }

        if pinned == Some(true) && !existing.pinned {
            let pinned_cnt: i64 = sqlx::query_scalar(
                "SELECT COUNT(*)::bigint FROM class_announcements WHERE room_id = $1 AND pinned = TRUE AND deleted_at IS NULL",
            )
            .bind(room.id)
            .fetch_one(&self.pool)
            .await?;
            if pinned_cnt >= MAX_PINNED_ANNOUNCEMENTS {
                return Err(user_err(
                    ErrorCode::InvalidParams,
                    format!("A turma já tem {MAX_PINNED_ANNOUNCEMENTS} anúncios fixados."),
                ));
            }
        }

        let new_title = match title {
            Some(t) => {
                let trimmed = t.trim();
                if trimmed.chars().count() > MAX_ANNOUNCEMENT_TITLE_CHARS {
                    return Err(user_err(
                        ErrorCode::InvalidParams,
                        format!("O título pode ter no máximo {MAX_ANNOUNCEMENT_TITLE_CHARS} caracteres."),
                    ));
                }
                trimmed.to_string()
            }
            None => existing.title.clone(),
        };

        let (new_cipher, edited_changed) = match body {
            Some(b) => {
                let trimmed = b.trim();
                if trimmed.is_empty() && !existing.has_attachment {
                    return Err(user_err(
                        ErrorCode::InvalidParams,
                        "O anúncio não pode ficar vazio.",
                    ));
                }
                if trimmed.chars().count() > MAX_ANNOUNCEMENT_BODY_CHARS {
                    return Err(user_err(
                        ErrorCode::InvalidParams,
                        format!("O anúncio pode ter no máximo {MAX_ANNOUNCEMENT_BODY_CHARS} caracteres."),
                    ));
                }
                (crypto.encrypt(trimmed)?, true)
            }
            None => (existing.body_ciphertext.clone(), title.is_some()),
        };

        let new_pinned = pinned.unwrap_or(existing.pinned);
        sqlx::query(
            r#"
            UPDATE class_announcements
            SET title = $3,
                body_ciphertext = $4,
                pinned = $5,
                edited_at = CASE WHEN $6 THEN NOW() ELSE edited_at END,
                rev = nextval('room_events_rev_seq')
            WHERE id = $1 AND room_id = $2
            "#,
        )
        .bind(announcement_id)
        .bind(room.id)
        .bind(&new_title)
        .bind(&new_cipher)
        .bind(new_pinned)
        .bind(edited_changed)
        .execute(&self.pool)
        .await?;

        let (items, _) = self
            .list_announcements(actor_id, None, None, None, 50, crypto)
            .await?;
        let updated = items
            .into_iter()
            .find(|a| a.id == announcement_id)
            .ok_or_else(|| ServerError::not_found("Anúncio não encontrado."))?;
        Ok((room, updated))
    }

    pub async fn delete_announcement(
        &self,
        actor_id: i64,
        announcement_id: i64,
    ) -> Result<(ResolvedClassroom, i64), ServerError> {
        let room = self.resolve_user_classroom(actor_id).await?;
        self.require_representative(&room, actor_id).await?;

        let rev: Option<i64> = sqlx::query_scalar(
            r#"
            UPDATE class_announcements
            SET deleted_at = NOW(),
                deleted_by_id = $3,
                pinned = FALSE,
                rev = nextval('room_events_rev_seq')
            WHERE id = $1 AND room_id = $2 AND deleted_at IS NULL
            RETURNING rev
            "#,
        )
        .bind(announcement_id)
        .bind(room.id)
        .bind(actor_id)
        .fetch_optional(&self.pool)
        .await?;

        let Some(rev) = rev else {
            return Err(user_err(ErrorCode::NotFound, "Anúncio não encontrado."));
        };
        Ok((room, rev))
    }

    pub async fn get_announcement_attachment_jpeg(
        &self,
        viewer_id: i64,
        announcement_id: i64,
    ) -> Result<Vec<u8>, ServerError> {
        let room = self.resolve_user_classroom(viewer_id).await?;
        let zip_bytes: Option<Vec<u8>> = sqlx::query_scalar(
            r#"
            SELECT att.zip_bytes
            FROM class_announcement_attachments att
            JOIN class_announcements a ON a.id = att.announcement_id
            WHERE att.announcement_id = $1 AND a.room_id = $2 AND a.deleted_at IS NULL
            "#,
        )
        .bind(announcement_id)
        .bind(room.id)
        .fetch_optional(&self.pool)
        .await?;

        let Some(zip_bytes) = zip_bytes else {
            return Err(ServerError::not_found("Attachment not found"));
        };
        unzip_single_file(&zip_bytes)
    }

    pub async fn mark_announcement_read(
        &self,
        viewer_id: i64,
        announcement_id: i64,
    ) -> Result<i64, ServerError> {
        let room = self.resolve_user_classroom(viewer_id).await?;
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM class_announcements WHERE id = $1 AND room_id = $2 AND deleted_at IS NULL)",
        )
        .bind(announcement_id)
        .bind(room.id)
        .fetch_one(&self.pool)
        .await?;
        if !exists {
            return Err(user_err(ErrorCode::NotFound, "Anúncio não encontrado."));
        }
        sqlx::query(
            "INSERT INTO class_announcement_reads (announcement_id, account_id, read_at) VALUES ($1, $2, NOW()) ON CONFLICT DO NOTHING",
        )
        .bind(announcement_id)
        .bind(viewer_id)
        .execute(&self.pool)
        .await?;
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*)::bigint FROM class_announcement_reads WHERE announcement_id = $1",
        )
        .bind(announcement_id)
        .fetch_one(&self.pool)
        .await?;
        Ok(count)
    }

    pub async fn set_announcement_reaction(
        &self,
        viewer_id: i64,
        announcement_id: i64,
        emoji: Option<&str>,
    ) -> Result<(ResolvedClassroom, i64), ServerError> {
        let room = self.resolve_user_classroom(viewer_id).await?;
        self.require_functional_room(&room).await?;

        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM class_announcements WHERE id = $1 AND room_id = $2 AND deleted_at IS NULL)",
        )
        .bind(announcement_id)
        .bind(room.id)
        .fetch_one(&self.pool)
        .await?;
        if !exists {
            return Err(user_err(ErrorCode::NotFound, "Anúncio não encontrado."));
        }

        if let Some(em) = emoji.map(str::trim).filter(|s| !s.is_empty()) {
            let allowed = ["👍", "👀", "❤️", "🔥", "🎉", "💯", "😂", "😮", "😢"];
            if !allowed.contains(&em) {
                return Err(user_err(ErrorCode::InvalidParams, "Reação inválida."));
            }
            sqlx::query(
                r#"
                INSERT INTO class_announcement_reactions (announcement_id, account_id, emoji, created_at)
                VALUES ($1, $2, $3, NOW())
                ON CONFLICT (announcement_id, account_id) DO UPDATE SET emoji = EXCLUDED.emoji, created_at = NOW()
                "#,
            )
            .bind(announcement_id)
            .bind(viewer_id)
            .bind(em)
            .execute(&self.pool)
            .await?;
        } else {
            sqlx::query(
                "DELETE FROM class_announcement_reactions WHERE announcement_id = $1 AND account_id = $2",
            )
            .bind(announcement_id)
            .bind(viewer_id)
            .execute(&self.pool)
            .await?;
        }

        let rev: i64 = sqlx::query_scalar(
            "UPDATE class_announcements SET rev = nextval('room_events_rev_seq') WHERE id = $1 RETURNING rev",
        )
        .bind(announcement_id)
        .fetch_one(&self.pool)
        .await?;

        Ok((room, rev))
    }

    pub async fn room_chat_rate_limit(
        &self,
        sender_id: i64,
    ) -> Result<ChatRateLimitStatus, ServerError> {
        let timestamps: Vec<DateTime<Utc>> = sqlx::query_scalar(
            r#"
            SELECT created_at
            FROM room_messages
            WHERE sender_id = $1
              AND created_at > NOW() - ($2::bigint * INTERVAL '1 second')
            ORDER BY created_at ASC
            "#,
        )
        .bind(sender_id)
        .bind(ROOM_CHAT_RATE_LIMIT_WINDOW_SECS)
        .fetch_all(&self.pool)
        .await?;

        let used = timestamps.len() as i64;
        let remaining = (ROOM_CHAT_RATE_LIMIT_COUNT - used).max(0);
        let retry_after_secs = if remaining == 0 {
            timestamps
                .first()
                .map(|oldest| {
                    let elapsed = (Utc::now() - *oldest).num_seconds();
                    (ROOM_CHAT_RATE_LIMIT_WINDOW_SECS - elapsed).max(1)
                })
                .unwrap_or(ROOM_CHAT_RATE_LIMIT_WINDOW_SECS)
        } else {
            0
        };

        Ok(ChatRateLimitStatus {
            limit: ROOM_CHAT_RATE_LIMIT_COUNT,
            window_secs: ROOM_CHAT_RATE_LIMIT_WINDOW_SECS,
            remaining,
            retry_after_secs,
        })
    }

    #[allow(clippy::too_many_arguments)]
    async fn hydrate_room_messages(
        &self,
        room_id: i64,
        viewer_id: i64,
        rows: Vec<RoomMessageSqlRow>,
        member_map: &HashMap<i64, &ClassMemberRow>,
        rep_set: &HashSet<i64>,
        blocked_set: &HashSet<i64>,
        crypto: &CryptoService,
    ) -> Result<Vec<ClassroomChatMessage>, ServerError> {
        if rows.is_empty() {
            return Ok(Vec::new());
        }

        let ids: Vec<i64> = rows.iter().map(|r| r.id).collect();
        let reply_ids: Vec<i64> = rows.iter().filter_map(|r| r.reply_to_id).collect();
        let ann_ids: Vec<i64> = rows.iter().filter_map(|r| r.ref_announcement_id).collect();

        let reactions = sqlx::query_as::<_, ReactionSqlRow>(
            r#"
            SELECT message_id AS target_id, account_id, emoji
            FROM room_message_reactions
            WHERE message_id = ANY($1)
            ORDER BY created_at ASC
            "#,
        )
        .bind(&ids)
        .fetch_all(&self.pool)
        .await?;

        let reply_rows = if reply_ids.is_empty() {
            Vec::new()
        } else {
            sqlx::query_as::<_, RoomMessageSqlRow>(
                r#"
                SELECT id, room_id, channel_kind, sender_id, body_ciphertext, reply_to_id,
                       ref_announcement_id, has_attachment, created_at, edited_at, deleted_at, deletion_kind, rev
                FROM room_messages
                WHERE id = ANY($1) AND room_id = $2
                "#,
            )
            .bind(&reply_ids)
            .bind(room_id)
            .fetch_all(&self.pool)
            .await?
        };

        let reply_map: HashMap<i64, RoomMessageReplyPreview> = reply_rows
            .into_iter()
            .map(|r| {
                let deleted = r.deleted_at.is_some();
                let sender_label = member_map
                    .get(&r.sender_id)
                    .map(|m| m.label())
                    .unwrap_or_else(|| format!("Aluno #{}", r.sender_id));
                let text = if deleted {
                    String::new()
                } else {
                    crypto
                        .decrypt(&r.body_ciphertext)
                        .map(|p| {
                            let s = p.as_str().trim();
                            if s.is_empty() && r.has_attachment {
                                "📷 Imagem".to_string()
                            } else {
                                truncate_preview(s, 140)
                            }
                        })
                        .unwrap_or_default()
                };
                (
                    r.id,
                    RoomMessageReplyPreview {
                        id: r.id,
                        sender_id: r.sender_id,
                        sender_label,
                        is_me: r.sender_id == viewer_id,
                        text,
                        deleted,
                    },
                )
            })
            .collect();

        let ann_map: HashMap<i64, ClassroomAnnouncementRef> = if ann_ids.is_empty() {
            HashMap::new()
        } else {
            let ann_rows: Vec<(i64, String, String)> = sqlx::query_as(
                "SELECT id, title, category FROM class_announcements WHERE id = ANY($1) AND room_id = $2",
            )
            .bind(&ann_ids)
            .bind(room_id)
            .fetch_all(&self.pool)
            .await?;
            ann_rows
                .into_iter()
                .map(|(id, title, category)| (id, ClassroomAnnouncementRef { id, title, category }))
                .collect()
        };

        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            if blocked_set.contains(&r.sender_id) {
                continue;
            }
            let deleted = r.deleted_at.is_some();
            let text = if deleted {
                String::new()
            } else {
                match crypto.decrypt(&r.body_ciphertext) {
                    Ok(plain) => plain.as_str().to_string(),
                    Err(_) => continue,
                }
            };
            let mut grouped: Vec<ChatReaction> = Vec::new();
            if !deleted {
                for re in reactions.iter().filter(|re| re.target_id == r.id) {
                    match grouped.iter_mut().find(|g| g.emoji == re.emoji) {
                        Some(g) => {
                            g.count += 1;
                            g.mine |= re.account_id == viewer_id;
                        }
                        None => grouped.push(ChatReaction {
                            emoji: re.emoji.clone(),
                            count: 1,
                            mine: re.account_id == viewer_id,
                        }),
                    }
                }
            }
            let sender = member_map.get(&r.sender_id);
            out.push(ClassroomChatMessage {
                id: r.id,
                room_id: r.room_id,
                channel_kind: r.channel_kind.clone(),
                sender_id: r.sender_id,
                sender_label: sender
                    .map(|m| m.label())
                    .unwrap_or_else(|| format!("Aluno #{}", r.sender_id)),
                sender_avatar_url: sender.and_then(|m| m.avatar_url()),
                sender_is_representative: rep_set.contains(&r.sender_id),
                is_me: r.sender_id == viewer_id,
                text,
                attachment_url: if !deleted && r.has_attachment {
                    Some(format!(
                        "/api/v1/classrooms/me/channels/{}/messages/{}/attachment",
                        r.channel_kind, r.id
                    ))
                } else {
                    None
                },
                created_at: r.created_at,
                edited_at: r.edited_at,
                deleted,
                deletion_kind: r.deletion_kind,
                reply_to: if deleted {
                    None
                } else {
                    r.reply_to_id.and_then(|id| reply_map.get(&id).cloned())
                },
                ref_announcement: if deleted {
                    None
                } else {
                    r.ref_announcement_id.and_then(|id| ann_map.get(&id).cloned())
                },
                reactions: grouped,
                rev: r.rev,
            });
        }

        Ok(out)
    }

    pub async fn get_channel_messages(
        &self,
        viewer_id: i64,
        kind: ClassroomChannelKind,
        query: ClassroomMessageQuery,
        crypto: &CryptoService,
    ) -> Result<RoomMessagesPayload, ServerError> {
        let room = self.resolve_user_classroom(viewer_id).await?;
        let members = self.load_class_members(&room.class_code).await?;
        let member_count = members.len() as i64;
        let room_info = self.build_room_info(&room, member_count).await?;
        let member_map: HashMap<i64, &ClassMemberRow> =
            members.iter().map(|m| (m.account_id, m)).collect();

        let rep_ids: Vec<i64> = sqlx::query_scalar(
            "SELECT account_id FROM class_representatives WHERE room_id = $1",
        )
        .bind(room.id)
        .fetch_all(&self.pool)
        .await?;
        let rep_set: HashSet<i64> = rep_ids.into_iter().collect();

        let blocked_ids: Vec<i64> = sqlx::query_scalar(
            "SELECT blocked_id FROM social_blocks WHERE blocker_id = $1",
        )
        .bind(viewer_id)
        .fetch_all(&self.pool)
        .await?;
        let blocked_set: HashSet<i64> = blocked_ids.into_iter().collect();

        let capped = query.limit.clamp(1, 100);
        let max_rev: i64 = sqlx::query_scalar(
            "SELECT COALESCE(MAX(rev), 0)::bigint FROM room_messages WHERE room_id = $1 AND channel_kind = $2",
        )
        .bind(room.id)
        .bind(kind.as_str())
        .fetch_one(&self.pool)
        .await?;

        let (rows, next_rev, check_older) = if let Some(after_rev) = query.after_rev {
            let mut rows = sqlx::query_as::<_, RoomMessageSqlRow>(
                r#"
                SELECT id, room_id, channel_kind, sender_id, body_ciphertext, reply_to_id,
                       ref_announcement_id, has_attachment, created_at, edited_at, deleted_at, deletion_kind, rev
                FROM room_messages
                WHERE room_id = $1 AND channel_kind = $2 AND rev > $3
                ORDER BY rev ASC
                LIMIT $4
                "#,
            )
            .bind(room.id)
            .bind(kind.as_str())
            .bind(after_rev)
            .bind(capped)
            .fetch_all(&self.pool)
            .await?;
            let next = if rows.len() as i64 == capped {
                rows.last().map_or(max_rev, |r| r.rev)
            } else {
                max_rev
            };
            rows.sort_by_key(|r| r.id);
            (rows, next, false)
        } else {
            let ceiling = query.before_id.unwrap_or(i64::MAX);
            let mut rows = sqlx::query_as::<_, RoomMessageSqlRow>(
                r#"
                SELECT id, room_id, channel_kind, sender_id, body_ciphertext, reply_to_id,
                       ref_announcement_id, has_attachment, created_at, edited_at, deleted_at, deletion_kind, rev
                FROM room_messages
                WHERE room_id = $1 AND channel_kind = $2 AND id < $3
                ORDER BY id DESC
                LIMIT $4
                "#,
            )
            .bind(room.id)
            .bind(kind.as_str())
            .bind(ceiling)
            .bind(capped)
            .fetch_all(&self.pool)
            .await?;
            rows.reverse();
            (rows, max_rev, true)
        };

        let has_more_before = match (check_older, rows.first()) {
            (true, Some(first)) => sqlx::query_scalar::<_, bool>(
                "SELECT EXISTS (SELECT 1 FROM room_messages WHERE room_id = $1 AND channel_kind = $2 AND id < $3)",
            )
            .bind(room.id)
            .bind(kind.as_str())
            .bind(first.id)
            .fetch_one(&self.pool)
            .await?,
            _ => false,
        };

        if let Some(last_id) = rows.iter().map(|r| r.id).max()
            && query.before_id.is_none()
        {
            sqlx::query(
                r#"
                INSERT INTO room_channel_read_state (account_id, room_id, channel_kind, last_read_message_id, updated_at)
                VALUES ($1, $2, $3, $4, NOW())
                ON CONFLICT (account_id, room_id, channel_kind) DO UPDATE SET
                    last_read_message_id = GREATEST(room_channel_read_state.last_read_message_id, EXCLUDED.last_read_message_id),
                    updated_at = NOW()
                "#,
            )
            .bind(viewer_id)
            .bind(room.id)
            .bind(kind.as_str())
            .bind(last_id)
            .execute(&self.pool)
            .await?;
        }

        let messages = self
            .hydrate_room_messages(
                room.id,
                viewer_id,
                rows,
                &member_map,
                &rep_set,
                &blocked_set,
                crypto,
            )
            .await?;
        let rate_limit = self.room_chat_rate_limit(viewer_id).await?;

        Ok(RoomMessagesPayload {
            room: room_info,
            channel_kind: kind.as_str().to_string(),
            am_i_representative: rep_set.contains(&viewer_id),
            messages,
            rate_limit,
            typing_users: Vec::new(),
            next_rev,
            has_more_before,
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn send_channel_message(
        &self,
        sender_id: i64,
        kind: ClassroomChannelKind,
        text: &str,
        reply_to_id: Option<i64>,
        ref_announcement_id: Option<i64>,
        attachment: Option<ZippedSanitizedImage>,
        crypto: &CryptoService,
    ) -> Result<(ResolvedClassroom, SendRoomMessagePayload, Option<i64>), ServerError> {
        let room = self.resolve_user_classroom(sender_id).await?;
        self.require_functional_room(&room).await?;

        let is_rep = self.is_representative(room.id, sender_id).await?;
        if attachment.is_some() && !is_rep {
            return Err(user_err(
                ErrorCode::Forbidden,
                "Apenas representantes de classe podem enviar imagens nos canais da turma.",
            ));
        }

        let clean = text.trim();
        if clean.is_empty() && attachment.is_none() {
            return Err(user_err(
                ErrorCode::InvalidParams,
                "A mensagem não pode estar vazia.",
            ));
        }
        if clean.chars().count() > MAX_ROOM_MESSAGE_CHARS {
            return Err(user_err(
                ErrorCode::InvalidParams,
                format!("A mensagem pode ter no máximo {MAX_ROOM_MESSAGE_CHARS} caracteres."),
            ));
        }

        let rl = self.room_chat_rate_limit(sender_id).await?;
        if rl.remaining <= 0 {
            return Err(ServerError::rate_limited(
                format!(
                    "Você enviou muitas mensagens seguidas. Aguarde {}s.",
                    rl.retry_after_secs
                ),
                "Room chat rate limit exceeded",
            ));
        }

        let mut reply_target_sender: Option<i64> = None;
        if let Some(rid) = reply_to_id {
            let found: Option<i64> = sqlx::query_scalar(
                "SELECT sender_id FROM room_messages WHERE id = $1 AND room_id = $2 AND channel_kind = $3",
            )
            .bind(rid)
            .bind(room.id)
            .bind(kind.as_str())
            .fetch_optional(&self.pool)
            .await?;
            if found.is_none() {
                return Err(user_err(
                    ErrorCode::NotFound,
                    "Mensagem respondida não encontrada.",
                ));
            }
            reply_target_sender = found;
        }

        if let Some(aid) = ref_announcement_id {
            let exists: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM class_announcements WHERE id = $1 AND room_id = $2 AND deleted_at IS NULL)",
            )
            .bind(aid)
            .bind(room.id)
            .fetch_one(&self.pool)
            .await?;
            if !exists {
                return Err(user_err(
                    ErrorCode::NotFound,
                    "Anúncio referenciado não encontrado.",
                ));
            }
        }

        let cipher = crypto.encrypt(clean)?;
        let has_attachment = attachment.is_some();

        let mut tx = self.pool.begin().await?;
        let row = sqlx::query_as::<_, RoomMessageSqlRow>(
            r#"
            INSERT INTO room_messages (
                room_id, channel_kind, sender_id, body_ciphertext,
                reply_to_id, ref_announcement_id, has_attachment, created_at, rev
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, NOW(), nextval('room_events_rev_seq'))
            RETURNING id, room_id, channel_kind, sender_id, body_ciphertext, reply_to_id,
                      ref_announcement_id, has_attachment, created_at, edited_at, deleted_at, deletion_kind, rev
            "#,
        )
        .bind(room.id)
        .bind(kind.as_str())
        .bind(sender_id)
        .bind(&cipher)
        .bind(reply_to_id)
        .bind(ref_announcement_id)
        .bind(has_attachment)
        .fetch_one(&mut *tx)
        .await?;

        if let Some(att) = attachment {
            sqlx::query(
                r#"
                INSERT INTO room_message_attachments (
                    message_id, mime_type, width, height, size_bytes, sha256, zip_bytes, created_at
                )
                VALUES ($1, 'image/jpeg', $2, $3, $4, $5, $6, NOW())
                "#,
            )
            .bind(row.id)
            .bind(att.width as i32)
            .bind(att.height as i32)
            .bind(att.raw_jpeg_bytes.len() as i32)
            .bind(&att.sha256)
            .bind(&att.zip_bytes)
            .execute(&mut *tx)
            .await?;
        }

        sqlx::query(
            r#"
            INSERT INTO room_channel_read_state (account_id, room_id, channel_kind, last_read_message_id, updated_at)
            VALUES ($1, $2, $3, $4, NOW())
            ON CONFLICT (account_id, room_id, channel_kind) DO UPDATE SET
                last_read_message_id = GREATEST(room_channel_read_state.last_read_message_id, EXCLUDED.last_read_message_id),
                updated_at = NOW()
            "#,
        )
        .bind(sender_id)
        .bind(room.id)
        .bind(kind.as_str())
        .bind(row.id)
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;

        let members = self.load_class_members(&room.class_code).await?;
        let member_map: HashMap<i64, &ClassMemberRow> =
            members.iter().map(|m| (m.account_id, m)).collect();
        let mut rep_set = HashSet::new();
        if is_rep {
            rep_set.insert(sender_id);
        }
        let mut hydrated = self
            .hydrate_room_messages(
                room.id,
                sender_id,
                vec![row],
                &member_map,
                &rep_set,
                &HashSet::new(),
                crypto,
            )
            .await?;
        let message = hydrated
            .pop()
            .ok_or_else(|| ServerError::internal("Falha ao carregar mensagem enviada."))?;
        let rate_limit = self.room_chat_rate_limit(sender_id).await?;

        Ok((
            room,
            SendRoomMessagePayload {
                message,
                rate_limit,
            },
            reply_target_sender,
        ))
    }

    pub async fn edit_channel_message(
        &self,
        actor_id: i64,
        kind: ClassroomChannelKind,
        message_id: i64,
        text: &str,
        crypto: &CryptoService,
    ) -> Result<(ResolvedClassroom, ClassroomChatMessage), ServerError> {
        let room = self.resolve_user_classroom(actor_id).await?;
        let clean = text.trim();
        if clean.is_empty() || clean.chars().count() > MAX_ROOM_MESSAGE_CHARS {
            return Err(user_err(
                ErrorCode::InvalidParams,
                format!("A mensagem deve ter entre 1 e {MAX_ROOM_MESSAGE_CHARS} caracteres."),
            ));
        }

        let existing = sqlx::query_as::<_, RoomMessageSqlRow>(
            r#"
            SELECT id, room_id, channel_kind, sender_id, body_ciphertext, reply_to_id,
                   ref_announcement_id, has_attachment, created_at, edited_at, deleted_at, deletion_kind, rev
            FROM room_messages
            WHERE id = $1 AND room_id = $2 AND channel_kind = $3
            "#,
        )
        .bind(message_id)
        .bind(room.id)
        .bind(kind.as_str())
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| user_err(ErrorCode::NotFound, "Mensagem não encontrada."))?;

        if existing.sender_id != actor_id {
            return Err(user_err(
                ErrorCode::Forbidden,
                "Você só pode editar suas próprias mensagens.",
            ));
        }
        if existing.deleted_at.is_some() {
            return Err(user_err(
                ErrorCode::InvalidParams,
                "Não é possível editar uma mensagem apagada.",
            ));
        }
        if (Utc::now() - existing.created_at).num_seconds() > ROOM_MESSAGE_EDIT_WINDOW_SECS {
            return Err(user_err(
                ErrorCode::Forbidden,
                "O tempo limite de 15 minutos para editar já passou.",
            ));
        }

        let cipher = crypto.encrypt(clean)?;
        let updated = sqlx::query_as::<_, RoomMessageSqlRow>(
            r#"
            UPDATE room_messages
            SET body_ciphertext = $2, edited_at = NOW(), rev = nextval('room_events_rev_seq')
            WHERE id = $1
            RETURNING id, room_id, channel_kind, sender_id, body_ciphertext, reply_to_id,
                      ref_announcement_id, has_attachment, created_at, edited_at, deleted_at, deletion_kind, rev
            "#,
        )
        .bind(message_id)
        .bind(&cipher)
        .fetch_one(&self.pool)
        .await?;

        let members = self.load_class_members(&room.class_code).await?;
        let member_map: HashMap<i64, &ClassMemberRow> =
            members.iter().map(|m| (m.account_id, m)).collect();
        let rep_ids: Vec<i64> = sqlx::query_scalar(
            "SELECT account_id FROM class_representatives WHERE room_id = $1",
        )
        .bind(room.id)
        .fetch_all(&self.pool)
        .await?;
        let rep_set: HashSet<i64> = rep_ids.into_iter().collect();

        let msg = self
            .hydrate_room_messages(
                room.id,
                actor_id,
                vec![updated],
                &member_map,
                &rep_set,
                &HashSet::new(),
                crypto,
            )
            .await?
            .pop()
            .ok_or_else(|| ServerError::internal("Falha ao hidratar mensagem editada."))?;

        Ok((room, msg))
    }

    pub async fn delete_channel_message(
        &self,
        actor_id: i64,
        kind: ClassroomChannelKind,
        message_id: i64,
        crypto: &CryptoService,
    ) -> Result<(ResolvedClassroom, ClassroomChatMessage), ServerError> {
        let room = self.resolve_user_classroom(actor_id).await?;
        let existing = sqlx::query_as::<_, RoomMessageSqlRow>(
            r#"
            SELECT id, room_id, channel_kind, sender_id, body_ciphertext, reply_to_id,
                   ref_announcement_id, has_attachment, created_at, edited_at, deleted_at, deletion_kind, rev
            FROM room_messages
            WHERE id = $1 AND room_id = $2 AND channel_kind = $3
            "#,
        )
        .bind(message_id)
        .bind(room.id)
        .bind(kind.as_str())
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| user_err(ErrorCode::NotFound, "Mensagem não encontrada."))?;

        let is_rep = self.is_representative(room.id, actor_id).await?;
        let deletion_kind = if existing.sender_id == actor_id {
            "author"
        } else if is_rep {
            "representative"
        } else {
            return Err(user_err(
                ErrorCode::Forbidden,
                "Apenas o autor ou um representante de classe pode remover esta mensagem.",
            ));
        };

        let updated = sqlx::query_as::<_, RoomMessageSqlRow>(
            r#"
            UPDATE room_messages
            SET deleted_at = NOW(),
                deleted_by_id = $2,
                deletion_kind = $3,
                rev = nextval('room_events_rev_seq')
            WHERE id = $1
            RETURNING id, room_id, channel_kind, sender_id, body_ciphertext, reply_to_id,
                      ref_announcement_id, has_attachment, created_at, edited_at, deleted_at, deletion_kind, rev
            "#,
        )
        .bind(message_id)
        .bind(actor_id)
        .bind(deletion_kind)
        .fetch_one(&self.pool)
        .await?;

        let members = self.load_class_members(&room.class_code).await?;
        let member_map: HashMap<i64, &ClassMemberRow> =
            members.iter().map(|m| (m.account_id, m)).collect();
        let rep_ids: Vec<i64> = sqlx::query_scalar(
            "SELECT account_id FROM class_representatives WHERE room_id = $1",
        )
        .bind(room.id)
        .fetch_all(&self.pool)
        .await?;
        let rep_set: HashSet<i64> = rep_ids.into_iter().collect();

        let msg = self
            .hydrate_room_messages(
                room.id,
                actor_id,
                vec![updated],
                &member_map,
                &rep_set,
                &HashSet::new(),
                crypto,
            )
            .await?
            .pop()
            .ok_or_else(|| ServerError::internal("Falha ao hidratar mensagem apagada."))?;

        Ok((room, msg))
    }

    pub async fn set_channel_message_reaction(
        &self,
        viewer_id: i64,
        kind: ClassroomChannelKind,
        message_id: i64,
        emoji: Option<&str>,
        crypto: &CryptoService,
    ) -> Result<(ResolvedClassroom, ClassroomChatMessage), ServerError> {
        let room = self.resolve_user_classroom(viewer_id).await?;
        self.require_functional_room(&room).await?;

        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM room_messages WHERE id = $1 AND room_id = $2 AND channel_kind = $3 AND deleted_at IS NULL)",
        )
        .bind(message_id)
        .bind(room.id)
        .bind(kind.as_str())
        .fetch_one(&self.pool)
        .await?;
        if !exists {
            return Err(user_err(ErrorCode::NotFound, "Mensagem não encontrada."));
        }

        if let Some(em) = emoji.map(str::trim).filter(|s| !s.is_empty()) {
            if !ALLOWED_REACTIONS.contains(&em) {
                return Err(user_err(ErrorCode::InvalidParams, "Reação inválida."));
            }
            sqlx::query(
                r#"
                INSERT INTO room_message_reactions (message_id, account_id, emoji, created_at)
                VALUES ($1, $2, $3, NOW())
                ON CONFLICT (message_id, account_id) DO UPDATE SET emoji = EXCLUDED.emoji, created_at = NOW()
                "#,
            )
            .bind(message_id)
            .bind(viewer_id)
            .bind(em)
            .execute(&self.pool)
            .await?;
        } else {
            sqlx::query(
                "DELETE FROM room_message_reactions WHERE message_id = $1 AND account_id = $2",
            )
            .bind(message_id)
            .bind(viewer_id)
            .execute(&self.pool)
            .await?;
        }

        let updated = sqlx::query_as::<_, RoomMessageSqlRow>(
            r#"
            UPDATE room_messages
            SET rev = nextval('room_events_rev_seq')
            WHERE id = $1
            RETURNING id, room_id, channel_kind, sender_id, body_ciphertext, reply_to_id,
                      ref_announcement_id, has_attachment, created_at, edited_at, deleted_at, deletion_kind, rev
            "#,
        )
        .bind(message_id)
        .fetch_one(&self.pool)
        .await?;

        let members = self.load_class_members(&room.class_code).await?;
        let member_map: HashMap<i64, &ClassMemberRow> =
            members.iter().map(|m| (m.account_id, m)).collect();
        let rep_ids: Vec<i64> = sqlx::query_scalar(
            "SELECT account_id FROM class_representatives WHERE room_id = $1",
        )
        .bind(room.id)
        .fetch_all(&self.pool)
        .await?;
        let rep_set: HashSet<i64> = rep_ids.into_iter().collect();

        let msg = self
            .hydrate_room_messages(
                room.id,
                viewer_id,
                vec![updated],
                &member_map,
                &rep_set,
                &HashSet::new(),
                crypto,
            )
            .await?
            .pop()
            .ok_or_else(|| ServerError::internal("Falha ao hidratar reação."))?;

        Ok((room, msg))
    }

    pub async fn get_channel_message_attachment_jpeg(
        &self,
        viewer_id: i64,
        kind: ClassroomChannelKind,
        message_id: i64,
    ) -> Result<Vec<u8>, ServerError> {
        let room = self.resolve_user_classroom(viewer_id).await?;
        let zip_bytes: Option<Vec<u8>> = sqlx::query_scalar(
            r#"
            SELECT att.zip_bytes
            FROM room_message_attachments att
            JOIN room_messages m ON m.id = att.message_id
            WHERE att.message_id = $1 AND m.room_id = $2 AND m.channel_kind = $3 AND m.deleted_at IS NULL
            "#,
        )
        .bind(message_id)
        .bind(room.id)
        .bind(kind.as_str())
        .fetch_optional(&self.pool)
        .await?;
        let Some(zip_bytes) = zip_bytes else {
            return Err(ServerError::not_found("Attachment not found"));
        };
        unzip_single_file(&zip_bytes)
    }

    pub async fn list_announcement_push_targets(
        &self,
        class_code: &str,
        exclude_account_id: i64,
    ) -> Result<Vec<(i64, String)>, ServerError> {
        let rows: Vec<(i64, String)> = sqlx::query_as(
            r#"
            SELECT a.id, a.fcm_token
            FROM accounts a
            LEFT JOIN account_notification_preferences p ON p.account_id = a.id
            WHERE a.active = TRUE
              AND UPPER(a.class_code) = UPPER($1)
              AND a.id <> $2
              AND a.fcm_token IS NOT NULL
              AND TRIM(a.fcm_token) <> ''
              AND COALESCE(p.announcements_enabled, TRUE) = TRUE
            "#,
        )
        .bind(class_code)
        .bind(exclude_account_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    pub async fn list_room_chat_push_targets(
        &self,
        class_code: &str,
        exclude_account_id: i64,
    ) -> Result<Vec<RoomPushTarget>, ServerError> {
        let rows: Vec<(i64, String, String, String)> = sqlx::query_as(
            r#"
            SELECT
                a.id,
                a.fcm_token,
                COALESCE(p.room_chat_mode, 'mentions'),
                COALESCE(p.room_offtopic_mode, 'muted')
            FROM accounts a
            LEFT JOIN account_notification_preferences p ON p.account_id = a.id
            WHERE a.active = TRUE
              AND UPPER(a.class_code) = UPPER($1)
              AND a.id <> $2
              AND a.fcm_token IS NOT NULL
              AND TRIM(a.fcm_token) <> ''
            "#,
        )
        .bind(class_code)
        .bind(exclude_account_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(
                |(account_id, fcm_token, room_chat_mode, room_offtopic_mode)| RoomPushTarget {
                    account_id,
                    fcm_token,
                    room_chat_mode,
                    room_offtopic_mode,
                },
            )
            .collect())
    }

    pub async fn is_grades_push_enabled(&self, account_id: i64) -> Result<bool, ServerError> {
        let enabled: Option<bool> = sqlx::query_scalar(
            "SELECT grades_enabled FROM account_notification_preferences WHERE account_id = $1",
        )
        .bind(account_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(enabled.unwrap_or(true))
    }

    pub async fn get_notification_preferences(
        &self,
        account_id: i64,
    ) -> Result<NotificationPreferences, ServerError> {
        let row: Option<(bool, bool, bool, String, String, DateTime<Utc>)> = sqlx::query_as(
            r#"
            SELECT grades_enabled, announcements_enabled, social_enabled,
                   room_chat_mode, room_offtopic_mode, updated_at
            FROM account_notification_preferences
            WHERE account_id = $1
            "#,
        )
        .bind(account_id)
        .fetch_optional(&self.pool)
        .await?;

        Ok(match row {
            Some((
                grades_enabled,
                announcements_enabled,
                social_enabled,
                room_chat_mode,
                room_offtopic_mode,
                updated_at,
            )) => NotificationPreferences {
                grades_enabled,
                announcements_enabled,
                social_enabled,
                room_chat_mode,
                room_offtopic_mode,
                updated_at,
            },
            None => NotificationPreferences::default(),
        })
    }

    pub async fn update_notification_preferences(
        &self,
        account_id: i64,
        grades_enabled: Option<bool>,
        announcements_enabled: Option<bool>,
        social_enabled: Option<bool>,
        room_chat_mode: Option<&str>,
        room_offtopic_mode: Option<&str>,
    ) -> Result<NotificationPreferences, ServerError> {
        let current = self.get_notification_preferences(account_id).await?;
        let valid_mode = |m: &str| matches!(m, "all" | "mentions" | "muted");

        let chat_mode = match room_chat_mode {
            Some(m) if valid_mode(m) => m.to_string(),
            Some(_) => {
                return Err(user_err(
                    ErrorCode::InvalidParams,
                    "Modo de notificação inválido para Chat da Turma.",
                ));
            }
            None => current.room_chat_mode,
        };
        let offtopic_mode = match room_offtopic_mode {
            Some(m) if valid_mode(m) => m.to_string(),
            Some(_) => {
                return Err(user_err(
                    ErrorCode::InvalidParams,
                    "Modo de notificação inválido para Chat Off-Topic.",
                ));
            }
            None => current.room_offtopic_mode,
        };

        let grades = grades_enabled.unwrap_or(current.grades_enabled);
        let announcements = announcements_enabled.unwrap_or(current.announcements_enabled);
        let social = social_enabled.unwrap_or(current.social_enabled);

        let updated_at: DateTime<Utc> = sqlx::query_scalar(
            r#"
            INSERT INTO account_notification_preferences (
                account_id, grades_enabled, announcements_enabled, social_enabled,
                room_chat_mode, room_offtopic_mode, updated_at
            )
            VALUES ($1, $2, $3, $4, $5, $6, NOW())
            ON CONFLICT (account_id) DO UPDATE SET
                grades_enabled = EXCLUDED.grades_enabled,
                announcements_enabled = EXCLUDED.announcements_enabled,
                social_enabled = EXCLUDED.social_enabled,
                room_chat_mode = EXCLUDED.room_chat_mode,
                room_offtopic_mode = EXCLUDED.room_offtopic_mode,
                updated_at = NOW()
            RETURNING updated_at
            "#,
        )
        .bind(account_id)
        .bind(grades)
        .bind(announcements)
        .bind(social)
        .bind(&chat_mode)
        .bind(&offtopic_mode)
        .fetch_one(&self.pool)
        .await?;

        Ok(NotificationPreferences {
            grades_enabled: grades,
            announcements_enabled: announcements,
            social_enabled: social,
            room_chat_mode: chat_mode,
            room_offtopic_mode: offtopic_mode,
            updated_at,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adaptive_quorum_respects_floor_3_and_cap_10() {
        assert_eq!(adaptive_quorum(1), 3);
        assert_eq!(adaptive_quorum(5), 3);
        assert_eq!(adaptive_quorum(6), 4);
        assert_eq!(adaptive_quorum(10), 6);
        assert_eq!(adaptive_quorum(15), 9);
        assert_eq!(adaptive_quorum(17), 10);
        assert_eq!(adaptive_quorum(35), 10);
    }
}
