//! Autonomous community moderation for public profile content.
//!
//! Rules (see `migrations/0005_moderation.sql`):
//! - Kinds: `media` (profile photo + banner, NSFW) and `text` (display names + bio, offensive).
//! - One open vote per person per target/kind; reporters can't report themselves, need to see the
//!   content, are rate limited, and accounts permanently restricted in that kind don't count.
//! - `STRIKE_REPORTERS` distinct reporters inside `REPORT_WINDOW_DAYS` = one strike: the content
//!   is deleted from the database and the account can't set that kind for `FIRST_STRIKE_DAYS`.
//!   Swapping the image doesn't reset the count (reports are about the account, in the window).
//! - A second strike (another full set of distinct reporters later) is permanent.

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;

use crate::error::ServerError;

pub const STRIKE_REPORTERS: i64 = 3;
pub const REPORT_WINDOW_DAYS: i64 = 30;
pub const FIRST_STRIKE_DAYS: i64 = 7;
pub const MAX_REPORTS_PER_DAY: i64 = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReportKind {
    Media,
    Text,
}

impl ReportKind {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "media" => Some(Self::Media),
            "text" => Some(Self::Text),
            _ => None,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Media => "media",
            Self::Text => "text",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Restriction {
    pub kind: ReportKind,
    /// `None` when permanent.
    pub until: Option<DateTime<Utc>>,
    pub permanent: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReportOutcome {
    /// Vote counted.
    Received,
    /// This person already has an open report on it.
    AlreadyReported,
    /// This vote completed a strike: content removed and the account restricted.
    Actioned,
}

fn user_error(message: &'static str) -> ServerError {
    ServerError::with_user_message(nsac_core::error::ErrorCode::InvalidParams, message, message)
}

/// Pure strike rule, kept separate for tests.
pub fn next_restriction(previous_strikes: i32, now: DateTime<Utc>) -> (i32, Option<DateTime<Utc>>, bool) {
    let strikes = previous_strikes + 1;
    if strikes >= 2 {
        (strikes, None, true)
    } else {
        (strikes, Some(now + Duration::days(FIRST_STRIKE_DAYS)), false)
    }
}

#[derive(Clone)]
pub struct ModerationRepo {
    pool: PgPool,
}

impl ModerationRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Active restrictions of an account (expired temporary ones are omitted).
    pub async fn restrictions(&self, account_id: i64) -> Result<Vec<Restriction>, ServerError> {
        let rows: Vec<(String, Option<DateTime<Utc>>, bool)> = sqlx::query_as(
            r#"
            SELECT kind, restricted_until, permanent
            FROM moderation_restrictions
            WHERE account_id = $1 AND (permanent OR restricted_until > NOW())
            "#,
        )
        .bind(account_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .filter_map(|(kind, until, permanent)| {
                Some(Restriction { kind: ReportKind::parse(&kind)?, until: if permanent { None } else { until }, permanent })
            })
            .collect())
    }

    pub async fn restriction(&self, account_id: i64, kind: ReportKind) -> Result<Option<Restriction>, ServerError> {
        Ok(self.restrictions(account_id).await?.into_iter().find(|r| r.kind == kind))
    }

    /// Error to return when a restricted account tries to set content of `kind`.
    pub fn restricted_error(restriction: &Restriction) -> ServerError {
        let what = match restriction.kind {
            ReportKind::Media => "foto de perfil e banner",
            ReportKind::Text => "nome de exibição e bio",
        };
        let message = match restriction.until {
            Some(until) if !restriction.permanent => format!(
                "Por denúncias da comunidade, você não pode alterar {what} até {}.",
                // Brasília time (UTC−3) for the message; falls back to UTC.
                chrono::FixedOffset::west_opt(3 * 3600).map_or_else(
                    || until.format("%d/%m às %H:%M UTC").to_string(),
                    |tz| until.with_timezone(&tz).format("%d/%m às %H:%M").to_string()
                )
            ),
            _ => format!("Por denúncias repetidas da comunidade, você não pode mais alterar {what}."),
        };
        ServerError::with_user_message(nsac_core::error::ErrorCode::Forbidden, message, "Moderation restriction")
    }

    async fn target_has_content(&self, target: i64, kind: ReportKind) -> Result<bool, ServerError> {
        let sql = match kind {
            ReportKind::Media => {
                "SELECT EXISTS (SELECT 1 FROM account_profiles WHERE account_id = $1 AND (avatar_data_url IS NOT NULL OR banner_data_url IS NOT NULL))"
            }
            ReportKind::Text => {
                "SELECT EXISTS (SELECT 1 FROM account_profiles WHERE account_id = $1 AND (display_name IS NOT NULL OR bio IS NOT NULL))
                 OR EXISTS (SELECT 1 FROM account_ranking_preferences WHERE account_id = $1 AND display_name IS NOT NULL AND NOT hide_name)"
            }
        };
        Ok(sqlx::query_scalar(sql).bind(target).fetch_one(&self.pool).await?)
    }

    /// Files a report. The caller already checked that the reporter can see the target's profile.
    pub async fn report(&self, reporter: i64, target: i64, kind: ReportKind) -> Result<ReportOutcome, ServerError> {
        if reporter == target {
            return Err(user_error("Você não pode denunciar a si mesmo."));
        }
        if self
            .restriction(reporter, kind)
            .await?
            .is_some_and(|r| r.permanent)
        {
            // Accounts banned for this kind lose their vote (keeps trolls from retaliating).
            return Ok(ReportOutcome::Received);
        }
        let recent: i64 = sqlx::query_scalar(
            "SELECT COUNT(*)::bigint FROM content_reports WHERE reporter_id = $1 AND created_at > NOW() - INTERVAL '1 day'",
        )
        .bind(reporter)
        .fetch_one(&self.pool)
        .await?;
        if recent >= MAX_REPORTS_PER_DAY {
            return Err(ServerError::rate_limited(
                "Você atingiu o limite de denúncias por hoje.",
                "Report rate limit",
            ));
        }
        if !self.target_has_content(target, kind).await? {
            return Err(user_error("Não há nada para denunciar neste perfil agora."));
        }

        let mut tx = self.pool.begin().await?;
        // Serialize strikes per target so two simultaneous last votes can't double-strike.
        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(target)
            .execute(&mut *tx)
            .await?;

        let inserted = sqlx::query(
            r#"
            INSERT INTO content_reports (reporter_id, target_id, kind)
            VALUES ($1, $2, $3)
            ON CONFLICT (reporter_id, target_id, kind) WHERE resolved_at IS NULL DO NOTHING
            "#,
        )
        .bind(reporter)
        .bind(target)
        .bind(kind.as_str())
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if inserted == 0 {
            tx.commit().await?;
            return Ok(ReportOutcome::AlreadyReported);
        }

        // Distinct, still-eligible reporters in the window.
        let reporters: Vec<i64> = sqlx::query_scalar(
            r#"
            SELECT DISTINCT r.reporter_id
            FROM content_reports r
            JOIN accounts a ON a.id = r.reporter_id AND a.active
            WHERE r.target_id = $1 AND r.kind = $2 AND r.resolved_at IS NULL
              AND r.created_at > NOW() - ($3::bigint * INTERVAL '1 day')
              AND NOT EXISTS (
                  SELECT 1 FROM moderation_restrictions m
                  WHERE m.account_id = r.reporter_id AND m.kind = $2 AND m.permanent
              )
            "#,
        )
        .bind(target)
        .bind(kind.as_str())
        .bind(REPORT_WINDOW_DAYS)
        .fetch_all(&mut *tx)
        .await?;

        if (reporters.len() as i64) < STRIKE_REPORTERS {
            tx.commit().await?;
            return Ok(ReportOutcome::Received);
        }

        let previous: i32 = sqlx::query_scalar(
            "SELECT strikes FROM moderation_restrictions WHERE account_id = $1 AND kind = $2",
        )
        .bind(target)
        .bind(kind.as_str())
        .fetch_optional(&mut *tx)
        .await?
        .unwrap_or(0);
        let (strikes, until, permanent) = next_restriction(previous, Utc::now());

        sqlx::query(
            r#"
            INSERT INTO moderation_restrictions (account_id, kind, strikes, restricted_until, permanent, updated_at)
            VALUES ($1, $2, $3, $4, $5, NOW())
            ON CONFLICT (account_id, kind) DO UPDATE SET
                strikes = EXCLUDED.strikes,
                restricted_until = EXCLUDED.restricted_until,
                permanent = EXCLUDED.permanent,
                updated_at = NOW()
            "#,
        )
        .bind(target)
        .bind(kind.as_str())
        .bind(strikes)
        .bind(until)
        .bind(permanent)
        .execute(&mut *tx)
        .await?;

        // Remove the content from the database completely.
        match kind {
            ReportKind::Media => {
                sqlx::query("UPDATE account_profiles SET avatar_data_url = NULL, banner_data_url = NULL, updated_at = NOW() WHERE account_id = $1")
                    .bind(target)
                    .execute(&mut *tx)
                    .await?;
            }
            ReportKind::Text => {
                sqlx::query("UPDATE account_profiles SET display_name = NULL, bio = NULL, updated_at = NOW() WHERE account_id = $1")
                    .bind(target)
                    .execute(&mut *tx)
                    .await?;
                sqlx::query("UPDATE account_ranking_preferences SET display_name = NULL, updated_at = NOW() WHERE account_id = $1")
                    .bind(target)
                    .execute(&mut *tx)
                    .await?;
            }
        }

        sqlx::query("UPDATE content_reports SET resolved_at = NOW() WHERE target_id = $1 AND kind = $2 AND resolved_at IS NULL")
            .bind(target)
            .bind(kind.as_str())
            .execute(&mut *tx)
            .await?;
        sqlx::query("INSERT INTO moderation_events (account_id, kind, action, reporter_ids) VALUES ($1, $2, $3, $4)")
            .bind(target)
            .bind(kind.as_str())
            .bind(if permanent { "permanent_restriction" } else { "temporary_restriction" })
            .bind(&reporters)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;

        tracing::warn!(target_account = target, kind = kind.as_str(), strikes, permanent, "Moderation strike applied");
        Ok(ReportOutcome::Actioned)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_strike_is_temporary_second_is_permanent() {
        let now = Utc::now();
        let (s1, until1, perm1) = next_restriction(0, now);
        assert_eq!(s1, 1);
        assert!(!perm1);
        assert_eq!(until1, Some(now + Duration::days(FIRST_STRIKE_DAYS)));
        let (s2, until2, perm2) = next_restriction(1, now);
        assert_eq!(s2, 2);
        assert!(perm2);
        assert_eq!(until2, None);
    }
}
