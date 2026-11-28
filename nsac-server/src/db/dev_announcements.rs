//! Global developer announcements repository (posts, reactions, comments, polls).

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::{FromRow, PgPool};

use crate::error::ServerError;

#[derive(Debug, Clone, Serialize)]
pub struct DevAnnouncementReaction {
    pub emoji: String,
    pub count: i64,
    pub reacted_by_me: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct DevAnnouncementPollOption {
    pub id: i64,
    pub label: String,
    pub votes: i64,
    pub voted_by_me: bool,
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct DevAnnouncementComment {
    pub id: i64,
    pub announcement_id: i64,
    pub account_id: i64,
    pub author_name: String,
    pub body: String,
    pub is_mine: bool,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, FromRow)]
struct DevAnnouncementBaseRow {
    id: i64,
    author_account_id: i64,
    author_name: String,
    title: String,
    body: String,
    tag: String,
    pinned: bool,
    image_data_url: Option<String>,
    read_by_me: bool,
    comment_count: i64,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DevAnnouncementRow {
    pub id: i64,
    pub author_account_id: i64,
    pub author_name: String,
    pub title: String,
    pub body: String,
    pub tag: String,
    pub pinned: bool,
    pub image_data_url: Option<String>,
    pub read_by_me: bool,
    pub comment_count: i64,
    pub reactions: Vec<DevAnnouncementReaction>,
    pub poll_options: Vec<DevAnnouncementPollOption>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Clone)]
pub struct DevAnnouncementRepo {
    pool: PgPool,
}

impl DevAnnouncementRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn get_platform_role(&self, account_id: i64) -> Result<String, ServerError> {
        let role: Option<String> = sqlx::query_scalar(
            "SELECT platform_role FROM accounts WHERE id = $1",
        )
        .bind(account_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| ServerError::internal(format!("DB error: {e}")))?;

        Ok(role.unwrap_or_else(|| "user".to_string()))
    }

    pub async fn list_for_account(
        &self,
        account_id: i64,
        limit: i64,
    ) -> Result<Vec<DevAnnouncementRow>, ServerError> {
        let limit = limit.clamp(1, 100);
        let base_rows = sqlx::query_as::<_, DevAnnouncementBaseRow>(
            r#"
            SELECT
                da.id,
                da.author_account_id,
                COALESCE(NULLIF(TRIM(a.verified_official_name), ''), a.nsac_email) AS author_name,
                da.title,
                da.body,
                da.tag,
                da.pinned,
                da.image_data_url,
                (dar.account_id IS NOT NULL) AS read_by_me,
                (
                    SELECT COUNT(*)
                    FROM dev_announcement_comments dac
                    WHERE dac.announcement_id = da.id AND dac.deleted_at IS NULL
                ) AS comment_count,
                da.created_at,
                da.updated_at
            FROM dev_announcements da
            JOIN accounts a ON a.id = da.author_account_id
            LEFT JOIN dev_announcement_reads dar
                ON dar.announcement_id = da.id AND dar.account_id = $1
            WHERE da.deleted_at IS NULL
            ORDER BY da.pinned DESC, da.created_at DESC
            LIMIT $2
            "#,
        )
        .bind(account_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| ServerError::internal(format!("DB error: {e}")))?;

        if base_rows.is_empty() {
            return Ok(Vec::new());
        }

        let ids: Vec<i64> = base_rows.iter().map(|r| r.id).collect();
        let (mut reactions_map, mut polls_map) = tokio::try_join!(
            self.fetch_reactions_map(&ids, account_id),
            self.fetch_polls_map(&ids, account_id),
        )?;

        Ok(base_rows
            .into_iter()
            .map(|r| DevAnnouncementRow {
                reactions: reactions_map.remove(&r.id).unwrap_or_default(),
                poll_options: polls_map.remove(&r.id).unwrap_or_default(),
                id: r.id,
                author_account_id: r.author_account_id,
                author_name: r.author_name,
                title: r.title,
                body: r.body,
                tag: r.tag,
                pinned: r.pinned,
                image_data_url: r.image_data_url,
                read_by_me: r.read_by_me,
                comment_count: r.comment_count,
                created_at: r.created_at,
                updated_at: r.updated_at,
            })
            .collect())
    }

    async fn fetch_reactions_map(
        &self,
        announcement_ids: &[i64],
        account_id: i64,
    ) -> Result<HashMap<i64, Vec<DevAnnouncementReaction>>, ServerError> {
        let rows: Vec<(i64, String, i64, bool)> = sqlx::query_as(
            r#"
            SELECT
                announcement_id,
                emoji,
                COUNT(*)::BIGINT AS count,
                BOOL_OR(account_id = $2) AS reacted_by_me
            FROM dev_announcement_reactions
            WHERE announcement_id = ANY($1)
            GROUP BY announcement_id, emoji
            ORDER BY COUNT(*) DESC, emoji ASC
            "#,
        )
        .bind(announcement_ids)
        .bind(account_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| ServerError::internal(format!("DB error: {e}")))?;

        let mut map: HashMap<i64, Vec<DevAnnouncementReaction>> = HashMap::new();
        for (ann_id, emoji, count, reacted_by_me) in rows {
            map.entry(ann_id).or_default().push(DevAnnouncementReaction {
                emoji,
                count,
                reacted_by_me,
            });
        }
        Ok(map)
    }

    async fn fetch_polls_map(
        &self,
        announcement_ids: &[i64],
        account_id: i64,
    ) -> Result<HashMap<i64, Vec<DevAnnouncementPollOption>>, ServerError> {
        let rows: Vec<(i64, i64, String, i64, bool)> = sqlx::query_as(
            r#"
            SELECT
                o.announcement_id,
                o.id,
                o.label,
                COUNT(v.account_id)::BIGINT AS votes,
                COALESCE(BOOL_OR(v.account_id = $2), FALSE) AS voted_by_me
            FROM dev_announcement_poll_options o
            LEFT JOIN dev_announcement_poll_votes v ON v.option_id = o.id
            WHERE o.announcement_id = ANY($1)
            GROUP BY o.announcement_id, o.id, o.label, o.position
            ORDER BY o.position ASC, o.id ASC
            "#,
        )
        .bind(announcement_ids)
        .bind(account_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| ServerError::internal(format!("DB error: {e}")))?;

        let mut map: HashMap<i64, Vec<DevAnnouncementPollOption>> = HashMap::new();
        for (ann_id, id, label, votes, voted_by_me) in rows {
            map.entry(ann_id).or_default().push(DevAnnouncementPollOption {
                id,
                label,
                votes,
                voted_by_me,
            });
        }
        Ok(map)
    }

    pub async fn count_unread(&self, account_id: i64) -> Result<i64, ServerError> {
        let count: i64 = sqlx::query_scalar(
            r#"
            SELECT COUNT(*)
            FROM dev_announcements da
            LEFT JOIN dev_announcement_reads dar
                ON dar.announcement_id = da.id AND dar.account_id = $1
            WHERE da.deleted_at IS NULL
              AND dar.account_id IS NULL
            "#,
        )
        .bind(account_id)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| ServerError::internal(format!("DB error: {e}")))?;

        Ok(count)
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn create(
        &self,
        author_account_id: i64,
        title: &str,
        body: &str,
        tag: &str,
        pinned: bool,
        image_data_url: Option<&str>,
        poll_options: &[String],
    ) -> Result<DevAnnouncementRow, ServerError> {
        let id: i64 = sqlx::query_scalar(
            r#"
            INSERT INTO dev_announcements (author_account_id, title, body, tag, pinned, image_data_url)
            VALUES ($1, $2, $3, $4, $5, $6)
            RETURNING id
            "#,
        )
        .bind(author_account_id)
        .bind(title)
        .bind(body)
        .bind(tag)
        .bind(pinned)
        .bind(image_data_url)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| ServerError::internal(format!("DB error: {e}")))?;

        for (idx, label) in poll_options.iter().enumerate() {
            sqlx::query(
                r#"
                INSERT INTO dev_announcement_poll_options (announcement_id, label, position)
                VALUES ($1, $2, $3)
                "#,
            )
            .bind(id)
            .bind(label)
            .bind(idx as i16)
            .execute(&self.pool)
            .await
            .map_err(|e| ServerError::internal(format!("DB error: {e}")))?;
        }

        let _ = self.mark_read(id, author_account_id).await;

        self.get_by_id(id, author_account_id)
            .await?
            .ok_or_else(|| ServerError::not_found("Announcement not found after creation"))
    }

    pub async fn get_by_id(
        &self,
        id: i64,
        account_id: i64,
    ) -> Result<Option<DevAnnouncementRow>, ServerError> {
        let base = sqlx::query_as::<_, DevAnnouncementBaseRow>(
            r#"
            SELECT
                da.id,
                da.author_account_id,
                COALESCE(NULLIF(TRIM(a.verified_official_name), ''), a.nsac_email) AS author_name,
                da.title,
                da.body,
                da.tag,
                da.pinned,
                da.image_data_url,
                (dar.account_id IS NOT NULL) AS read_by_me,
                (
                    SELECT COUNT(*)
                    FROM dev_announcement_comments dac
                    WHERE dac.announcement_id = da.id AND dac.deleted_at IS NULL
                ) AS comment_count,
                da.created_at,
                da.updated_at
            FROM dev_announcements da
            JOIN accounts a ON a.id = da.author_account_id
            LEFT JOIN dev_announcement_reads dar
                ON dar.announcement_id = da.id AND dar.account_id = $2
            WHERE da.id = $1 AND da.deleted_at IS NULL
            "#,
        )
        .bind(id)
        .bind(account_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| ServerError::internal(format!("DB error: {e}")))?;

        let Some(r) = base else {
            return Ok(None);
        };

        let ids = [r.id];
        let (mut reactions_map, mut polls_map) = tokio::try_join!(
            self.fetch_reactions_map(&ids, account_id),
            self.fetch_polls_map(&ids, account_id),
        )?;

        Ok(Some(DevAnnouncementRow {
            reactions: reactions_map.remove(&r.id).unwrap_or_default(),
            poll_options: polls_map.remove(&r.id).unwrap_or_default(),
            id: r.id,
            author_account_id: r.author_account_id,
            author_name: r.author_name,
            title: r.title,
            body: r.body,
            tag: r.tag,
            pinned: r.pinned,
            image_data_url: r.image_data_url,
            read_by_me: r.read_by_me,
            comment_count: r.comment_count,
            created_at: r.created_at,
            updated_at: r.updated_at,
        }))
    }

    pub async fn soft_delete(&self, id: i64) -> Result<bool, ServerError> {
        let res = sqlx::query(
            "UPDATE dev_announcements SET deleted_at = NOW(), updated_at = NOW() WHERE id = $1 AND deleted_at IS NULL",
        )
        .bind(id)
        .execute(&self.pool)
        .await
        .map_err(|e| ServerError::internal(format!("DB error: {e}")))?;

        Ok(res.rows_affected() > 0)
    }

    pub async fn mark_read(&self, id: i64, account_id: i64) -> Result<(), ServerError> {
        sqlx::query(
            r#"
            INSERT INTO dev_announcement_reads (announcement_id, account_id)
            VALUES ($1, $2)
            ON CONFLICT (announcement_id, account_id) DO NOTHING
            "#,
        )
        .bind(id)
        .bind(account_id)
        .execute(&self.pool)
        .await
        .map_err(|e| ServerError::internal(format!("DB error: {e}")))?;

        Ok(())
    }

    pub async fn toggle_reaction(
        &self,
        announcement_id: i64,
        account_id: i64,
        emoji: &str,
    ) -> Result<Vec<DevAnnouncementReaction>, ServerError> {
        let deleted = sqlx::query(
            r#"
            DELETE FROM dev_announcement_reactions
            WHERE announcement_id = $1 AND account_id = $2 AND emoji = $3
            "#,
        )
        .bind(announcement_id)
        .bind(account_id)
        .bind(emoji)
        .execute(&self.pool)
        .await
        .map_err(|e| ServerError::internal(format!("DB error: {e}")))?;

        if deleted.rows_affected() == 0 {
            sqlx::query(
                r#"
                INSERT INTO dev_announcement_reactions (announcement_id, account_id, emoji)
                VALUES ($1, $2, $3)
                ON CONFLICT DO NOTHING
                "#,
            )
            .bind(announcement_id)
            .bind(account_id)
            .bind(emoji)
            .execute(&self.pool)
            .await
            .map_err(|e| ServerError::internal(format!("DB error: {e}")))?;
        }

        let mut map = self.fetch_reactions_map(&[announcement_id], account_id).await?;
        Ok(map.remove(&announcement_id).unwrap_or_default())
    }

    pub async fn vote_poll(
        &self,
        announcement_id: i64,
        option_id: i64,
        account_id: i64,
    ) -> Result<Vec<DevAnnouncementPollOption>, ServerError> {
        let valid: Option<i64> = sqlx::query_scalar(
            "SELECT id FROM dev_announcement_poll_options WHERE id = $1 AND announcement_id = $2",
        )
        .bind(option_id)
        .bind(announcement_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| ServerError::internal(format!("DB error: {e}")))?;

        if valid.is_none() {
            return Err(ServerError::not_found("Opção de enquete não encontrada."));
        }

        sqlx::query(
            r#"
            INSERT INTO dev_announcement_poll_votes (announcement_id, option_id, account_id)
            VALUES ($1, $2, $3)
            ON CONFLICT (announcement_id, account_id)
            DO UPDATE SET option_id = EXCLUDED.option_id, created_at = NOW()
            "#,
        )
        .bind(announcement_id)
        .bind(option_id)
        .bind(account_id)
        .execute(&self.pool)
        .await
        .map_err(|e| ServerError::internal(format!("DB error: {e}")))?;

        let mut map = self.fetch_polls_map(&[announcement_id], account_id).await?;
        Ok(map.remove(&announcement_id).unwrap_or_default())
    }

    pub async fn list_comments(
        &self,
        announcement_id: i64,
        account_id: i64,
    ) -> Result<Vec<DevAnnouncementComment>, ServerError> {
        let rows = sqlx::query_as::<_, DevAnnouncementComment>(
            r#"
            SELECT
                c.id,
                c.announcement_id,
                c.account_id,
                COALESCE(NULLIF(TRIM(a.verified_official_name), ''), a.nsac_email) AS author_name,
                c.body,
                (c.account_id = $2) AS is_mine,
                c.created_at
            FROM dev_announcement_comments c
            JOIN accounts a ON a.id = c.account_id
            WHERE c.announcement_id = $1 AND c.deleted_at IS NULL
            ORDER BY c.created_at ASC
            LIMIT 200
            "#,
        )
        .bind(announcement_id)
        .bind(account_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| ServerError::internal(format!("DB error: {e}")))?;

        Ok(rows)
    }

    pub async fn create_comment(
        &self,
        announcement_id: i64,
        account_id: i64,
        body: &str,
    ) -> Result<DevAnnouncementComment, ServerError> {
        let id: i64 = sqlx::query_scalar(
            r#"
            INSERT INTO dev_announcement_comments (announcement_id, account_id, body)
            VALUES ($1, $2, $3)
            RETURNING id
            "#,
        )
        .bind(announcement_id)
        .bind(account_id)
        .bind(body)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| ServerError::internal(format!("DB error: {e}")))?;

        let row = sqlx::query_as::<_, DevAnnouncementComment>(
            r#"
            SELECT
                c.id,
                c.announcement_id,
                c.account_id,
                COALESCE(NULLIF(TRIM(a.verified_official_name), ''), a.nsac_email) AS author_name,
                c.body,
                (c.account_id = $2) AS is_mine,
                c.created_at
            FROM dev_announcement_comments c
            JOIN accounts a ON a.id = c.account_id
            WHERE c.id = $1
            "#,
        )
        .bind(id)
        .bind(account_id)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| ServerError::internal(format!("DB error: {e}")))?;

        Ok(row)
    }

    pub async fn delete_comment(
        &self,
        comment_id: i64,
        account_id: i64,
        is_dev: bool,
    ) -> Result<bool, ServerError> {
        let res = if is_dev {
            sqlx::query(
                "UPDATE dev_announcement_comments SET deleted_at = NOW() WHERE id = $1 AND deleted_at IS NULL",
            )
            .bind(comment_id)
            .execute(&self.pool)
            .await
        } else {
            sqlx::query(
                "UPDATE dev_announcement_comments SET deleted_at = NOW() WHERE id = $1 AND account_id = $2 AND deleted_at IS NULL",
            )
            .bind(comment_id)
            .bind(account_id)
            .execute(&self.pool)
            .await
        }
        .map_err(|e| ServerError::internal(format!("DB error: {e}")))?;

        Ok(res.rows_affected() > 0)
    }

    pub async fn list_push_tokens_for_dev_announcements(
        &self,
        exclude_account_id: i64,
    ) -> Result<Vec<(i64, String)>, ServerError> {
        let rows: Vec<(i64, String)> = sqlx::query_as(
            r#"
            SELECT a.id, a.fcm_token
            FROM accounts a
            WHERE a.active = TRUE
              AND a.fcm_token IS NOT NULL
              AND BTRIM(a.fcm_token) <> ''
              AND a.id <> $1
            "#,
        )
        .bind(exclude_account_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| ServerError::internal(format!("DB error: {e}")))?;

        Ok(rows)
    }
}
