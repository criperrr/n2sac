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
    /// Show my profile photo next to my ranking entry (never while `hide_name`).
    pub show_avatar: bool,
    /// Let other ranking participants open my profile (never while `hide_name`).
    pub public_profile: bool,
    /// Findable in student search even without showing the name in the ranking.
    pub discoverable: bool,
    /// Never exposed anywhere: out of the ranking, search and profiles.
    pub invisible: bool,
    pub has_decided: bool,
    pub updated_at: DateTime<Utc>,
}

#[derive(FromRow)]
struct RankingPrefRow {
    account_id: i64,
    opted_in: bool,
    hide_name: bool,
    display_name: Option<String>,
    show_avatar: bool,
    public_profile: bool,
    discoverable: bool,
    invisible: bool,
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
            show_avatar: r.show_avatar,
            public_profile: r.public_profile,
            discoverable: r.discoverable,
            invisible: r.invisible,
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
    show_avatar: bool,
    /// `updated_at` (epoch seconds) of the stored profile photo, when there is one.
    avatar_version: Option<i64>,
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
    /// Versioned `/api/v1/profiles/{id}/avatar?v=…` path, only when the student shows a photo.
    pub avatar_url: Option<String>,
    /// Whether the requester may open this student's profile.
    pub has_public_profile: bool,
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

    pub async fn get_preferences(&self, account_id: i64) -> Result<RankingPreferences, ServerError> {
        let row = sqlx::query_as::<_, RankingPrefRow>(
            r#"
            SELECT account_id, opted_in, hide_name, display_name, show_avatar, public_profile,
                   discoverable, invisible, has_decided, updated_at
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
                show_avatar: false,
                public_profile: false,
                discoverable: false,
                invisible: false,
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
        show_avatar: Option<bool>,
        public_profile: Option<bool>,
    ) -> Result<RankingPreferences, ServerError> {
        if opted_in {
            let is_invisible: bool = sqlx::query_scalar(
                "SELECT invisible FROM account_ranking_preferences WHERE account_id = $1",
            )
            .bind(account_id)
            .fetch_optional(&self.pool)
            .await?
            .unwrap_or(false);
            if is_invisible {
                return Err(ServerError::invalid_params(
                    "Desative o modo invisível no seu Perfil para participar do ranking.",
                ));
            }
        }
        let cleaned_name = display_name
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.chars().take(36).collect::<String>());

        let row = sqlx::query_as::<_, RankingPrefRow>(
            r#"
            INSERT INTO account_ranking_preferences (
                account_id, opted_in, hide_name, display_name, show_avatar, public_profile,
                visibility_customized, has_decided, updated_at
            )
            -- Joining the ranking turns photo + public profile on by default…
            VALUES (
                $1, $2, $3, $4,
                COALESCE($5::boolean, $2), COALESCE($6::boolean, $2),
                ($5::boolean IS NOT NULL OR $6::boolean IS NOT NULL),
                TRUE, NOW()
            )
            ON CONFLICT (account_id) DO UPDATE SET
                opted_in = EXCLUDED.opted_in,
                hide_name = EXCLUDED.hide_name,
                display_name = EXCLUDED.display_name,
                -- …on every re-join too, unless the student has set these switches by hand:
                -- then their choice survives leaving and re-joining the ranking.
                show_avatar = CASE
                    WHEN $5::boolean IS NOT NULL THEN $5::boolean
                    WHEN EXCLUDED.opted_in AND NOT account_ranking_preferences.opted_in
                         AND NOT account_ranking_preferences.visibility_customized THEN TRUE
                    ELSE account_ranking_preferences.show_avatar
                END,
                public_profile = CASE
                    WHEN $6::boolean IS NOT NULL THEN $6::boolean
                    WHEN EXCLUDED.opted_in AND NOT account_ranking_preferences.opted_in
                         AND NOT account_ranking_preferences.visibility_customized THEN TRUE
                    ELSE account_ranking_preferences.public_profile
                END,
                visibility_customized = account_ranking_preferences.visibility_customized
                    OR $5::boolean IS NOT NULL OR $6::boolean IS NOT NULL,
                has_decided = TRUE,
                updated_at = NOW()
            RETURNING account_id, opted_in, hide_name, display_name, show_avatar, public_profile,
                      discoverable, invisible, has_decided, updated_at
            "#,
        )
        .bind(account_id)
        .bind(opted_in)
        .bind(hide_name)
        .bind(cleaned_name)
        .bind(show_avatar)
        .bind(public_profile)
        .fetch_one(&self.pool)
        .await?;

        Ok(RankingPreferences::from(row))
    }

    /// Account-wide exposure settings. Invisible mode wins over everything: it leaves the ranking,
    /// turns search, photo and public profile off, and counts as a hand-made visibility choice
    /// (so re-joining the ranking later doesn't silently re-expose the photo/profile).
    pub async fn set_privacy(
        &self,
        account_id: i64,
        discoverable: Option<bool>,
        invisible: Option<bool>,
    ) -> Result<RankingPreferences, ServerError> {
        let row = sqlx::query_as::<_, RankingPrefRow>(
            r#"
            INSERT INTO account_ranking_preferences (
                account_id, discoverable, invisible, visibility_customized, has_decided, updated_at
            )
            VALUES (
                $1,
                COALESCE($2::boolean, FALSE) AND NOT COALESCE($3::boolean, FALSE),
                COALESCE($3::boolean, FALSE),
                COALESCE($3::boolean, FALSE),
                COALESCE($3::boolean, FALSE),
                NOW()
            )
            ON CONFLICT (account_id) DO UPDATE SET
                invisible = COALESCE($3::boolean, account_ranking_preferences.invisible),
                discoverable = CASE
                    WHEN COALESCE($3::boolean, account_ranking_preferences.invisible) THEN FALSE
                    ELSE COALESCE($2::boolean, account_ranking_preferences.discoverable)
                END,
                opted_in = account_ranking_preferences.opted_in
                    AND NOT COALESCE($3::boolean, account_ranking_preferences.invisible),
                show_avatar = account_ranking_preferences.show_avatar
                    AND NOT COALESCE($3::boolean, account_ranking_preferences.invisible),
                public_profile = account_ranking_preferences.public_profile
                    AND NOT COALESCE($3::boolean, account_ranking_preferences.invisible),
                visibility_customized = account_ranking_preferences.visibility_customized
                    OR COALESCE($3::boolean, FALSE),
                has_decided = account_ranking_preferences.has_decided OR COALESCE($3::boolean, FALSE),
                updated_at = NOW()
            RETURNING account_id, opted_in, hide_name, display_name, show_avatar, public_profile,
                      discoverable, invisible, has_decided, updated_at
            "#,
        )
        .bind(account_id)
        .bind(discoverable)
        .bind(invisible)
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

        // Aggregate grades per student first and join `account_profiles` only on the <=250
        // output rows so PostgreSQL never includes the TOASTed `avatar_data_url` blob in
        // GROUP BY / Sort keys.
        let rows = sqlx::query_as::<_, LeaderboardSqlRow>(
            r#"
            WITH student_stats AS (
                SELECT
                    a.id AS account_id,
                    a.nsac_email,
                    a.class_code,
                    rp.hide_name,
                    rp.display_name,
                    rp.show_avatar,
                    ROUND(AVG(g.grade)::numeric, 2)::float8 AS personal_average,
                    ROUND(COALESCE(AVG(NULLIF(g.average_grade, 0)), 0)::numeric, 2)::float8 AS class_average,
                    ROUND((AVG(g.grade) - COALESCE(AVG(NULLIF(g.average_grade, 0)), 0))::numeric, 2)::float8 AS delta_vs_class,
                    COUNT(g.id)::bigint AS posted_bimesters_count,
                    COUNT(DISTINCT g.subject_id)::bigint AS subjects_count,
                    COALESCE(SUM(g.absences), 0)::bigint AS total_absences
                FROM accounts a
                JOIN account_ranking_preferences rp
                    ON rp.account_id = a.id AND rp.opted_in = TRUE AND rp.invisible = FALSE
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
                GROUP BY a.id, a.nsac_email, a.class_code, rp.hide_name, rp.display_name, rp.show_avatar
                HAVING COUNT(g.id) > 0
                ORDER BY personal_average DESC, delta_vs_class DESC, total_absences ASC, a.id ASC
                LIMIT 250
            )
            SELECT
                st.account_id,
                st.nsac_email,
                st.class_code,
                st.hide_name,
                st.display_name,
                st.show_avatar,
                CASE WHEN ap.avatar_data_url IS NOT NULL
                     THEN EXTRACT(EPOCH FROM ap.updated_at)::bigint END AS avatar_version,
                st.personal_average,
                st.class_average,
                st.delta_vs_class,
                st.posted_bimesters_count,
                st.subjects_count,
                st.total_absences
            FROM student_stats st
            LEFT JOIN account_profiles ap
                ON ap.account_id = st.account_id
            ORDER BY st.personal_average DESC, st.delta_vs_class DESC, st.total_absences ASC, st.account_id ASC
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
                avatar_url: (is_me || (row.show_avatar && !row.hide_name))
                    .then_some(row.avatar_version)
                    .flatten()
                    .map(|v| avatar_path(row.account_id, v)),
                has_public_profile: is_me || !row.hide_name,
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

/// Versioned avatar path: a new photo gets a new URL, so clients may cache it forever.
pub fn avatar_path(account_id: i64, version: i64) -> String {
    format!("/api/v1/profiles/{account_id}/avatar?v={version}")
}

pub(crate) fn format_student_label(
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
