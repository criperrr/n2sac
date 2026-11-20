use chrono::{DateTime, Utc};
use sqlx::{FromRow, PgPool};

use crate::error::ServerError;

#[derive(Debug, Clone, PartialEq, FromRow)]
pub struct SentinelClassState {
    pub class_code: String,
    pub last_class_hash: Option<String>,
    pub last_checked_at: Option<DateTime<Utc>>,
    pub last_change_detected_at: Option<DateTime<Utc>>,
    pub hot_until: Option<DateTime<Utc>>,
    pub next_check_at: DateTime<Utc>,
    pub current_certainty: f64,
    pub current_interval_secs: i64,
    pub consecutive_errors: i32,
    pub last_api_bump_at: Option<DateTime<Utc>>,
    pub active_sentinel_account_id: Option<i64>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewGradePostingEvent {
    pub class_code: String,
    pub school_year: i32,
    pub bimester: i16,
    pub subject_name: String,
    pub day_of_year: i16,
    pub month: i16,
    pub day: i16,
    pub posted_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, FromRow)]
pub struct GradePostingEvent {
    pub id: i64,
    pub class_code: String,
    pub school_year: i32,
    pub bimester: i16,
    pub subject_name: String,
    pub day_of_year: i16,
    pub month: i16,
    pub day: i16,
    pub posted_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct SentinelRepo {
    pool: PgPool,
}

impl SentinelRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn ensure_class_initialized(
        &self,
        class_code: &str,
        baseline_interval_secs: i64,
        now: DateTime<Utc>,
    ) -> Result<SentinelClassState, ServerError> {
        sqlx::query(
            r#"
            INSERT INTO sentinel_class_state (
                class_code,
                next_check_at,
                current_interval_secs,
                updated_at
            )
            VALUES ($1, $2, $3, $2)
            ON CONFLICT (class_code) DO NOTHING
            "#,
        )
        .bind(class_code)
        .bind(now)
        .bind(baseline_interval_secs.max(1))
        .execute(&self.pool)
        .await?;

        self.get_class_state(class_code)
            .await?
            .ok_or_else(|| ServerError::internal("Failed to load initialized sentinel class state"))
    }

    pub async fn initialize_from_login(
        &self,
        class_code: &str,
        class_hash: &str,
        account_id: i64,
        baseline_interval_secs: i64,
        now: DateTime<Utc>,
    ) -> Result<SentinelClassState, ServerError> {
        let interval = baseline_interval_secs.max(1);
        let next_check_at = now + chrono::Duration::seconds(interval);
        let row = sqlx::query_as::<_, SentinelClassState>(
            r#"
            INSERT INTO sentinel_class_state (
                class_code,
                last_class_hash,
                next_check_at,
                current_certainty,
                current_interval_secs,
                active_sentinel_account_id,
                updated_at
            )
            VALUES ($1, $2, $3, 0.0, $4, $5, $6)
            ON CONFLICT (class_code) DO UPDATE SET
                last_class_hash = COALESCE(sentinel_class_state.last_class_hash, EXCLUDED.last_class_hash),
                active_sentinel_account_id = COALESCE(sentinel_class_state.active_sentinel_account_id, EXCLUDED.active_sentinel_account_id),
                updated_at = EXCLUDED.updated_at
            RETURNING
                class_code,
                last_class_hash,
                last_checked_at,
                last_change_detected_at,
                hot_until,
                next_check_at,
                current_certainty,
                current_interval_secs,
                consecutive_errors,
                last_api_bump_at,
                active_sentinel_account_id,
                updated_at
            "#,
        )
        .bind(class_code)
        .bind(class_hash)
        .bind(next_check_at)
        .bind(interval)
        .bind(account_id)
        .bind(now)
        .fetch_one(&self.pool)
        .await?;

        Ok(row)
    }

    pub async fn get_class_state(
        &self,
        class_code: &str,
    ) -> Result<Option<SentinelClassState>, ServerError> {
        let row = sqlx::query_as::<_, SentinelClassState>(
            r#"
            SELECT
                class_code,
                last_class_hash,
                last_checked_at,
                last_change_detected_at,
                hot_until,
                next_check_at,
                current_certainty,
                current_interval_secs,
                consecutive_errors,
                last_api_bump_at,
                active_sentinel_account_id,
                updated_at
            FROM sentinel_class_state
            WHERE class_code = $1
            "#,
        )
        .bind(class_code)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row)
    }

    pub async fn list_due_classes(
        &self,
        now: DateTime<Utc>,
    ) -> Result<Vec<SentinelClassState>, ServerError> {
        let rows = sqlx::query_as::<_, SentinelClassState>(
            r#"
            SELECT
                s.class_code,
                s.last_class_hash,
                s.last_checked_at,
                s.last_change_detected_at,
                s.hot_until,
                s.next_check_at,
                s.current_certainty,
                s.current_interval_secs,
                s.consecutive_errors,
                s.last_api_bump_at,
                s.active_sentinel_account_id,
                s.updated_at
            FROM sentinel_class_state s
            WHERE s.next_check_at <= $1
              AND EXISTS (
                  SELECT 1
                  FROM accounts a
                  WHERE a.class_code = s.class_code AND a.active = TRUE
              )
            ORDER BY s.next_check_at ASC, s.class_code ASC
            "#,
        )
        .bind(now)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows)
    }

    pub async fn upsert_class_state(
        &self,
        state: &SentinelClassState,
    ) -> Result<(), ServerError> {
        sqlx::query(
            r#"
            INSERT INTO sentinel_class_state (
                class_code,
                last_class_hash,
                last_checked_at,
                last_change_detected_at,
                hot_until,
                next_check_at,
                current_certainty,
                current_interval_secs,
                consecutive_errors,
                last_api_bump_at,
                active_sentinel_account_id,
                updated_at
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)
            ON CONFLICT (class_code) DO UPDATE SET
                last_class_hash = EXCLUDED.last_class_hash,
                last_checked_at = EXCLUDED.last_checked_at,
                last_change_detected_at = EXCLUDED.last_change_detected_at,
                hot_until = EXCLUDED.hot_until,
                next_check_at = EXCLUDED.next_check_at,
                current_certainty = EXCLUDED.current_certainty,
                current_interval_secs = EXCLUDED.current_interval_secs,
                consecutive_errors = EXCLUDED.consecutive_errors,
                last_api_bump_at = EXCLUDED.last_api_bump_at,
                active_sentinel_account_id = EXCLUDED.active_sentinel_account_id,
                updated_at = EXCLUDED.updated_at
            "#,
        )
        .bind(&state.class_code)
        .bind(&state.last_class_hash)
        .bind(state.last_checked_at)
        .bind(state.last_change_detected_at)
        .bind(state.hot_until)
        .bind(state.next_check_at)
        .bind(state.current_certainty)
        .bind(state.current_interval_secs)
        .bind(state.consecutive_errors)
        .bind(state.last_api_bump_at)
        .bind(state.active_sentinel_account_id)
        .bind(state.updated_at)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    pub async fn try_record_api_bump(
        &self,
        class_code: &str,
        weight: f64,
        cooldown_secs: i64,
        now: DateTime<Utc>,
    ) -> Result<Option<SentinelClassState>, ServerError> {
        let bounded_weight = weight.clamp(0.0, 1.0);
        let cooldown = cooldown_secs.max(0) as f64;

        let row = sqlx::query_as::<_, SentinelClassState>(
            r#"
            UPDATE sentinel_class_state
            SET
                current_certainty = LEAST(1.0, current_certainty + $2),
                last_api_bump_at = $4,
                updated_at = $4
            WHERE class_code = $1
              AND (
                  last_api_bump_at IS NULL
                  OR last_api_bump_at <= $4 - make_interval(secs => $3)
              )
            RETURNING
                class_code,
                last_class_hash,
                last_checked_at,
                last_change_detected_at,
                hot_until,
                next_check_at,
                current_certainty,
                current_interval_secs,
                consecutive_errors,
                last_api_bump_at,
                active_sentinel_account_id,
                updated_at
            "#,
        )
        .bind(class_code)
        .bind(bounded_weight)
        .bind(cooldown)
        .bind(now)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row)
    }

    pub async fn set_active_sentinel(
        &self,
        class_code: &str,
        account_id: Option<i64>,
        now: DateTime<Utc>,
    ) -> Result<(), ServerError> {
        sqlx::query(
            r#"
            UPDATE sentinel_class_state
            SET active_sentinel_account_id = $2, updated_at = $3
            WHERE class_code = $1
            "#,
        )
        .bind(class_code)
        .bind(account_id)
        .bind(now)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    pub async fn insert_posting_event(
        &self,
        event: &NewGradePostingEvent,
    ) -> Result<GradePostingEvent, ServerError> {
        let row = sqlx::query_as::<_, GradePostingEvent>(
            r#"
            INSERT INTO grade_posting_events (
                class_code,
                school_year,
                bimester,
                subject_name,
                day_of_year,
                month,
                day,
                posted_at
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
            RETURNING
                id,
                class_code,
                school_year,
                bimester,
                subject_name,
                day_of_year,
                month,
                day,
                posted_at
            "#,
        )
        .bind(&event.class_code)
        .bind(event.school_year)
        .bind(event.bimester)
        .bind(&event.subject_name)
        .bind(event.day_of_year)
        .bind(event.month)
        .bind(event.day)
        .bind(event.posted_at)
        .fetch_one(&self.pool)
        .await?;

        Ok(row)
    }

    pub async fn insert_posting_events_batch(
        &self,
        events: &[NewGradePostingEvent],
    ) -> Result<Vec<GradePostingEvent>, ServerError> {
        if events.is_empty() {
            return Ok(Vec::new());
        }

        let mut tx = self.pool.begin().await?;
        let mut inserted = Vec::with_capacity(events.len());

        for event in events {
            let row = sqlx::query_as::<_, GradePostingEvent>(
                r#"
                INSERT INTO grade_posting_events (
                    class_code,
                    school_year,
                    bimester,
                    subject_name,
                    day_of_year,
                    month,
                    day,
                    posted_at
                )
                VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
                RETURNING
                    id,
                    class_code,
                    school_year,
                    bimester,
                    subject_name,
                    day_of_year,
                    month,
                    day,
                    posted_at
                "#,
            )
            .bind(&event.class_code)
            .bind(event.school_year)
            .bind(event.bimester)
            .bind(&event.subject_name)
            .bind(event.day_of_year)
            .bind(event.month)
            .bind(event.day)
            .bind(event.posted_at)
            .fetch_one(&mut *tx)
            .await?;

            inserted.push(row);
        }

        tx.commit().await?;
        Ok(inserted)
    }

    pub async fn list_events_for_class(
        &self,
        class_code: &str,
    ) -> Result<Vec<GradePostingEvent>, ServerError> {
        let rows = sqlx::query_as::<_, GradePostingEvent>(
            r#"
            SELECT
                id,
                class_code,
                school_year,
                bimester,
                subject_name,
                day_of_year,
                month,
                day,
                posted_at
            FROM grade_posting_events
            WHERE class_code = $1
            ORDER BY posted_at ASC, id ASC
            "#,
        )
        .bind(class_code)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows)
    }

    pub async fn list_events_for_class_year_bimester(
        &self,
        class_code: &str,
        school_year: i32,
        bimester: i16,
    ) -> Result<Vec<GradePostingEvent>, ServerError> {
        let rows = sqlx::query_as::<_, GradePostingEvent>(
            r#"
            SELECT
                id,
                class_code,
                school_year,
                bimester,
                subject_name,
                day_of_year,
                month,
                day,
                posted_at
            FROM grade_posting_events
            WHERE class_code = $1
              AND school_year = $2
              AND bimester = $3
            ORDER BY posted_at ASC, id ASC
            "#,
        )
        .bind(class_code)
        .bind(school_year)
        .bind(bimester)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows)
    }
}
