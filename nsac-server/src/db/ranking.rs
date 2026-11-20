use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, PgPool};

use crate::error::ServerError;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RankingPreferences {
    pub account_id: i64,
    pub opted_in: bool,
    pub hide_name: bool,
    pub display_name: Option<String>,
    pub has_decided: bool,
    pub updated_at: DateTime<Utc>,
}

#[derive(FromRow)]
struct RankingPrefRow {
    account_id: i64,
    opted_in: bool,
    hide_name: bool,
    display_name: Option<String>,
    has_decided: bool,
    updated_at: DateTime<Utc>,
}

impl From<RankingPrefRow> for RankingPreferences {
    fn from(r: RankingPrefRow) -> Self {
        Self {
            account_id: r.account_id,
            opted_in: r.opted_in,
            hide_name: r.hide_name,
            display_name: r.display_name,
            has_decided: r.has_decided,
            updated_at: r.updated_at,
        }
    }
}

#[derive(FromRow)]
struct LeaderboardSqlRow {
    account_id: i64,
    nsac_email: String,
    class_code: Option<String>,
    hide_name: bool,
    display_name: Option<String>,
    personal_average: f64,
    class_average: f64,
    delta_vs_class: f64,
    posted_bimesters_count: i64,
    subjects_count: i64,
    total_absences: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct LeaderboardEntry {
    pub rank: usize,
    pub account_id: i64,
    pub is_me: bool,
    pub display_label: String,
    pub is_anonymous: bool,
    pub class_code: String,
    pub personal_average: f64,
    pub class_average: f64,
    pub delta_vs_class: f64,
    pub posted_bimesters_count: i64,
    pub subjects_count: i64,
    pub total_absences: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct SubjectComparisonRow {
    pub subject_name: String,
    pub my_average: Option<f64>,
    pub peer_average: Option<f64>,
    pub class_average: Option<f64>,
    pub delta_me_vs_peer: Option<f64>,
    pub my_absences: i64,
    pub peer_absences: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ComparisonSummaryResponse {
    pub scope: String,
    pub school_year: i32,
    pub bimester: Option<i16>,
    pub subject_filter: Option<String>,
    pub total_participants: usize,
    pub my_preferences: RankingPreferences,
    pub my_rank: Option<usize>,
    pub my_percentile: Option<f64>,
    pub my_entry: Option<LeaderboardEntry>,
    pub leaderboard: Vec<LeaderboardEntry>,
    pub peer_comparison: Option<PeerComparisonDetail>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PeerComparisonDetail {
    pub peer_entry: LeaderboardEntry,
    pub my_wins: usize,
    pub peer_wins: usize,
    pub ties: usize,
    pub subjects: Vec<SubjectComparisonRow>,
}

#[derive(FromRow)]
struct SubjectAvgRow {
    subject_name: String,
    avg_grade: Option<f64>,
    avg_class: Option<f64>,
    total_absences: i64,
}

#[derive(Debug, Clone)]
pub struct RankingRepo {
    pool: PgPool,
}

impl RankingRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn ensure_table(&self) -> Result<(), ServerError> {
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS account_ranking_preferences (
                account_id BIGINT PRIMARY KEY REFERENCES accounts(id) ON DELETE CASCADE,
                opted_in BOOLEAN NOT NULL DEFAULT FALSE,
                hide_name BOOLEAN NOT NULL DEFAULT FALSE,
                display_name TEXT NULL,
                has_decided BOOLEAN NOT NULL DEFAULT FALSE,
                updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
            )
            "#,
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            r#"
            CREATE INDEX IF NOT EXISTS idx_ranking_prefs_opted_in
                ON account_ranking_preferences (opted_in)
                WHERE opted_in = TRUE
            "#,
        )
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    pub async fn get_preferences(&self, account_id: i64) -> Result<RankingPreferences, ServerError> {
        self.ensure_table().await?;
        let row = sqlx::query_as::<_, RankingPrefRow>(
            r#"
            SELECT account_id, opted_in, hide_name, display_name, has_decided, updated_at
            FROM account_ranking_preferences
            WHERE account_id = $1
            "#,
        )
        .bind(account_id)
        .fetch_optional(&self.pool)
        .await?;

        Ok(match row {
            Some(r) => RankingPreferences::from(r),
            None => RankingPreferences {
                account_id,
                opted_in: false,
                hide_name: false,
                display_name: None,
                has_decided: false,
                updated_at: Utc::now(),
            },
        })
    }

    pub async fn upsert_preferences(
        &self,
        account_id: i64,
        opted_in: bool,
        hide_name: bool,
        display_name: Option<&str>,
    ) -> Result<RankingPreferences, ServerError> {
        self.ensure_table().await?;
        let cleaned_name = display_name
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.chars().take(36).collect::<String>());

        let row = sqlx::query_as::<_, RankingPrefRow>(
            r#"
            INSERT INTO account_ranking_preferences (
                account_id, opted_in, hide_name, display_name, has_decided, updated_at
            )
            VALUES ($1, $2, $3, $4, TRUE, NOW())
            ON CONFLICT (account_id) DO UPDATE SET
                opted_in = EXCLUDED.opted_in,
                hide_name = EXCLUDED.hide_name,
                display_name = EXCLUDED.display_name,
                has_decided = TRUE,
                updated_at = NOW()
            RETURNING account_id, opted_in, hide_name, display_name, has_decided, updated_at
            "#,
        )
        .bind(account_id)
        .bind(opted_in)
        .bind(hide_name)
        .bind(cleaned_name)
        .fetch_one(&self.pool)
        .await?;

        Ok(RankingPreferences::from(row))
    }

    pub async fn latest_school_year_for_account(&self, account_id: i64) -> Result<i32, ServerError> {
        let yr: Option<i32> = sqlx::query_scalar(
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

        Ok(yr.unwrap_or_else(|| Utc::now().format("%Y").to_string().parse().unwrap_or(2026)))
    }

    /// Single-query SQL aggregation across all opted-in students, ignoring unposted `0.0 / M.T. 0.0` bimesters.
    #[allow(clippy::too_many_arguments)]
    pub async fn fetch_leaderboard(
        &self,
        requester_account_id: i64,
        requester_class_code: Option<&str>,
        scope: &str,
        school_year: i32,
        bimester: Option<i16>,
        subject_contains: Option<&str>,
        compare_with_account_id: Option<i64>,
    ) -> Result<ComparisonSummaryResponse, ServerError> {
        self.ensure_table().await?;
        let my_prefs = self.get_preferences(requester_account_id).await?;

        let class_filter: Option<String> = match scope {
            "class" => requester_class_code.map(|c| c.trim().to_ascii_uppercase()),
            "course" => requester_class_code.map(|c| {
                let up = c.trim().to_ascii_uppercase();
                let prefix: String = up.chars().take(3).collect();
                format!("{prefix}%")
            }),
            _ => None, // "school" = all CTI classes
        };

        let subj_like: Option<String> = subject_contains
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| format!("%{}%", s.to_lowercase()));

        let rows = sqlx::query_as::<_, LeaderboardSqlRow>(
            r#"
            SELECT
                a.id AS account_id,
                a.nsac_email,
                a.class_code,
                rp.hide_name,
                rp.display_name,
                ROUND(AVG(g.grade)::numeric, 2)::float8 AS personal_average,
                ROUND(COALESCE(AVG(NULLIF(g.average_grade, 0)), 0)::numeric, 2)::float8 AS class_average,
                ROUND((AVG(g.grade) - COALESCE(AVG(NULLIF(g.average_grade, 0)), 0))::numeric, 2)::float8 AS delta_vs_class,
                COUNT(g.id)::bigint AS posted_bimesters_count,
                COUNT(DISTINCT g.subject_id)::bigint AS subjects_count,
                COALESCE(SUM(g.absences), 0)::bigint AS total_absences
            FROM accounts a
            JOIN account_ranking_preferences rp
                ON rp.account_id = a.id AND rp.opted_in = TRUE
            JOIN grades g
                ON g.account_id = a.id
            JOIN subjects s
                ON s.id = g.subject_id
            JOIN school_years sy
                ON sy.id = s.school_year_id
            WHERE a.active = TRUE
              AND sy.year = $1
              AND g.grade IS NOT NULL
              AND NOT (
                  g.grade = 0
                  AND COALESCE(g.average_grade, 0) = 0
                  AND COALESCE(g.recovery_code, 'NAC') = 'NAC'
              )
              AND ($2::smallint IS NULL OR g.bimester = $2)
              AND ($3::text IS NULL OR LOWER(s.name) LIKE $3)
              AND (
                  $4::text IS NULL
                  OR ($5::text = 'class' AND UPPER(COALESCE(a.class_code, '')) = $4)
                  OR ($5::text = 'course' AND UPPER(COALESCE(a.class_code, '')) LIKE $4)
              )
            GROUP BY a.id, a.nsac_email, a.class_code, rp.hide_name, rp.display_name
            HAVING COUNT(g.id) > 0
            ORDER BY personal_average DESC, delta_vs_class DESC, total_absences ASC, a.id ASC
            LIMIT 250
            "#,
        )
        .bind(school_year)
        .bind(bimester)
        .bind(subj_like.as_deref())
        .bind(class_filter.as_deref())
        .bind(scope)
        .fetch_all(&self.pool)
        .await?;

        let total_participants = rows.len();
        let mut leaderboard = Vec::with_capacity(total_participants);
        let mut my_rank: Option<usize> = None;
        let mut my_entry: Option<LeaderboardEntry> = None;

        for (idx, row) in rows.into_iter().enumerate() {
            let rank = idx + 1;
            let is_me = row.account_id == requester_account_id;
            let display_label = format_student_label(
                row.account_id,
                &row.nsac_email,
                row.hide_name,
                row.display_name.as_deref(),
                is_me,
            );

            let entry = LeaderboardEntry {
                rank,
                account_id: row.account_id,
                is_me,
                display_label,
                is_anonymous: row.hide_name,
                class_code: row.class_code.unwrap_or_else(|| "CTI".to_string()),
                personal_average: row.personal_average,
                class_average: row.class_average,
                delta_vs_class: row.delta_vs_class,
                posted_bimesters_count: row.posted_bimesters_count,
                subjects_count: row.subjects_count,
                total_absences: row.total_absences,
            };

            if is_me {
                my_rank = Some(rank);
                my_entry = Some(entry.clone());
            }
            leaderboard.push(entry);
        }

        let my_percentile = my_rank.map(|r| {
            if total_participants <= 1 {
                100.0
            } else {
                let pct = ((total_participants - r) as f64 / (total_participants - 1) as f64) * 100.0;
                (pct * 10.0).round() / 10.0
            }
        });

        let peer_comparison = if let Some(peer_id) = compare_with_account_id {
            if let Some(peer_entry) = leaderboard.iter().find(|e| e.account_id == peer_id).cloned() {
                Some(
                    self.build_head_to_head(
                        requester_account_id,
                        peer_entry,
                        school_year,
                        bimester,
                    )
                    .await?,
                )
            } else {
                None
            }
        } else {
            None
        };

        Ok(ComparisonSummaryResponse {
            scope: scope.to_string(),
            school_year,
            bimester,
            subject_filter: subject_contains.map(ToString::to_string),
            total_participants,
            my_preferences: my_prefs,
            my_rank,
            my_percentile,
            my_entry,
            leaderboard,
            peer_comparison,
        })
    }

    async fn build_head_to_head(
        &self,
        my_account_id: i64,
        peer_entry: LeaderboardEntry,
        school_year: i32,
        bimester: Option<i16>,
    ) -> Result<PeerComparisonDetail, ServerError> {
        let fetch_subject_avgs = |acct_id: i64| {
            sqlx::query_as::<_, SubjectAvgRow>(
                r#"
                SELECT
                    s.name AS subject_name,
                    ROUND(AVG(g.grade)::numeric, 2)::float8 AS avg_grade,
                    ROUND(AVG(NULLIF(g.average_grade, 0))::numeric, 2)::float8 AS avg_class,
                    COALESCE(SUM(g.absences), 0)::bigint AS total_absences
                FROM grades g
                JOIN subjects s ON s.id = g.subject_id
                JOIN school_years sy ON sy.id = s.school_year_id
                WHERE g.account_id = $1
                  AND sy.year = $2
                  AND g.grade IS NOT NULL
                  AND NOT (
                      g.grade = 0
                      AND COALESCE(g.average_grade, 0) = 0
                      AND COALESCE(g.recovery_code, 'NAC') = 'NAC'
                  )
                  AND ($3::smallint IS NULL OR g.bimester = $3)
                GROUP BY s.name
                ORDER BY s.name ASC
                "#,
            )
            .bind(acct_id)
            .bind(school_year)
            .bind(bimester)
            .fetch_all(&self.pool)
        };

        let (my_rows, peer_rows) = tokio::try_join!(
            fetch_subject_avgs(my_account_id),
            fetch_subject_avgs(peer_entry.account_id)
        )?;

        let mut map: std::collections::BTreeMap<String, SubjectComparisonRow> =
            std::collections::BTreeMap::new();

        for r in my_rows {
            map.insert(
                r.subject_name.clone(),
                SubjectComparisonRow {
                    subject_name: r.subject_name,
                    my_average: r.avg_grade,
                    peer_average: None,
                    class_average: r.avg_class,
                    delta_me_vs_peer: None,
                    my_absences: r.total_absences,
                    peer_absences: 0,
                },
            );
        }

        for r in peer_rows {
            let entry = map
                .entry(r.subject_name.clone())
                .or_insert_with(|| SubjectComparisonRow {
                    subject_name: r.subject_name,
                    my_average: None,
                    peer_average: None,
                    class_average: r.avg_class,
                    delta_me_vs_peer: None,
                    my_absences: 0,
                    peer_absences: 0,
                });
            entry.peer_average = r.avg_grade;
            entry.peer_absences = r.total_absences;
            if entry.class_average.is_none() {
                entry.class_average = r.avg_class;
            }
        }

        let mut my_wins = 0usize;
        let mut peer_wins = 0usize;
        let mut ties = 0usize;
        let mut subjects = Vec::with_capacity(map.len());

        for (_, mut row) in map {
            if let (Some(m), Some(p)) = (row.my_average, row.peer_average) {
                let diff = ((m - p) * 100.0).round() / 100.0;
                row.delta_me_vs_peer = Some(diff);
                if diff > 0.05 {
                    my_wins += 1;
                } else if diff < -0.05 {
                    peer_wins += 1;
                } else {
                    ties += 1;
                }
            }
            subjects.push(row);
        }

        Ok(PeerComparisonDetail {
            peer_entry,
            my_wins,
            peer_wins,
            ties,
            subjects,
        })
    }
}

fn format_student_label(
    account_id: i64,
    email: &str,
    hide_name: bool,
    display_name: Option<&str>,
    is_me: bool,
) -> String {
    if hide_name {
        let tag = format!("Aluno #{:03}", (account_id * 37 + 13) % 997);
        return if is_me {
            format!("{tag} (Você • Oculto)")
        } else {
            tag
        };
    }

    if let Some(custom) = display_name.map(str::trim).filter(|s| !s.is_empty()) {
        return custom.to_string();
    }

    // Derive clean capitalized display name from institutional email (e.g. "gabriel.camaforte@unesp.br" -> "Gabriel Camaforte")
    let local = email.split('@').next().unwrap_or("Aluno");
    let pretty: Vec<String> = local
        .split(['.', '_', '-'])
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => format!("{}{}", first.to_uppercase(), chars.as_str().to_lowercase()),
                None => String::new(),
            }
        })
        .collect();

    if pretty.is_empty() {
        format!("Aluno #{account_id}")
    } else {
        pretty.join(" ")
    }
}
