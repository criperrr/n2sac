use std::collections::HashMap;

use nsac_core::models::{
    AllYearsResponse, BimesterData, ClassBiInformation, FullGrades, PersonalBiInformation,
    RecoveryMessage, RecoveryStatusCode, ResultData, UnifiedBimesterData, WarningValue, YearInfo,
};
use sqlx::{FromRow, PgPool};

use crate::error::ServerError;

const fn recovery_message_str(msg: RecoveryMessage) -> &'static str {
    match msg {
        RecoveryMessage::Satisfatorio => "Satisfatório",
        RecoveryMessage::Insatisfatorio => "Insatisfatório",
        RecoveryMessage::NaoCompareceu => "Não Compareceu",
        RecoveryMessage::NaoAconteceu => "Não Aconteceu",
    }
}

#[derive(FromRow)]
struct AccountSchoolYearRow {
    school_year_id: i64,
    year: i32,
    title: String,
    status: String,
}

#[derive(FromRow)]
struct BimesterMetricRow {
    school_year_id: i64,
    user_average: Option<f64>,
    class_average: Option<f64>,
    total_absences: i32,
}

#[derive(FromRow)]
struct GradeJoinedRow {
    school_year_id: i64,
    subject_id: i64,
    subject_name: String,
    bimester: i16,
    grade: Option<f64>,
    average_grade: Option<f64>,
    absences: i32,
    recovery_code: String,
    recovery_message: String,
    recovered: bool,
}

#[derive(FromRow)]
struct SubjectFinalJoinedRow {
    school_year_id: i64,
    subject_id: i64,
    subject_name: String,
    final_grade: Option<f64>,
    total_absences: i32,
}

#[derive(Debug, Clone)]
pub struct BoletimRepo {
    pool: PgPool,
}

impl BoletimRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn upsert_boletim(
        &self,
        account_id: i64,
        boletim: &AllYearsResponse,
    ) -> Result<(), ServerError> {
        let mut tx = self.pool.begin().await?;

        // Sort years and subjects deterministically so concurrent fan-outs acquire locks in identical order.
        let mut years_sorted: Vec<&YearInfo> = boletim.data.iter().collect();
        years_sorted.sort_by(|a, b| a.year.cmp(&b.year).then_with(|| a.title.cmp(&b.title)));

        for year_info in years_sorted {
            let year_i32 = i32::from(year_info.year);
            let school_year_id: i64 = sqlx::query_scalar(
                r#"
                INSERT INTO school_years (year, title)
                VALUES ($1, $2)
                ON CONFLICT (title, year) DO UPDATE SET
                    year = EXCLUDED.year
                RETURNING id
                "#,
            )
            .bind(year_i32)
            .bind(&year_info.title)
            .fetch_one(&mut *tx)
            .await?;

            sqlx::query(
                r#"
                INSERT INTO accounts_school_years (account_id, school_year_id, status)
                VALUES ($1, $2, $3)
                ON CONFLICT (account_id, school_year_id) DO UPDATE SET
                    status = EXCLUDED.status
                "#,
            )
            .bind(account_id)
            .bind(school_year_id)
            .bind(&year_info.status)
            .execute(&mut *tx)
            .await?;

            for (idx, metric) in year_info.bimesters_metrics.iter().take(4).enumerate() {
                let bim_i16 = i16::try_from(idx + 1).map_err(|_| {
                    ServerError::invalid_params("Bimester metric index out of range")
                })?;
                let total_absences_i32 = i32::try_from(metric.total_absences).map_err(|_| {
                    ServerError::invalid_params("Bimester total_absences exceeds i32 range")
                })?;

                sqlx::query(
                    r#"
                    INSERT INTO bimesters (
                        account_id,
                        school_year_id,
                        bimester,
                        user_average,
                        class_average,
                        total_absences
                    )
                    VALUES ($1, $2, $3, $4, $5, $6)
                    ON CONFLICT (account_id, school_year_id, bimester) DO UPDATE SET
                        user_average = EXCLUDED.user_average,
                        class_average = EXCLUDED.class_average,
                        total_absences = EXCLUDED.total_absences
                    "#,
                )
                .bind(account_id)
                .bind(school_year_id)
                .bind(bim_i16)
                .bind(Some(metric.user_average))
                .bind(Some(metric.class_average))
                .bind(total_absences_i32)
                .execute(&mut *tx)
                .await?;
            }

            let mut subjects_sorted: Vec<&FullGrades> = year_info.grades.iter().collect();
            subjects_sorted.sort_by(|a, b| a.subject_name.cmp(&b.subject_name));

            for subject in subjects_sorted {
                let subject_id: i64 = sqlx::query_scalar(
                    r#"
                    INSERT INTO subjects (school_year_id, abbreviation, name)
                    VALUES ($1, '', $2)
                    ON CONFLICT (name, school_year_id) DO UPDATE SET
                        name = EXCLUDED.name
                    RETURNING id
                    "#,
                )
                .bind(school_year_id)
                .bind(&subject.subject_name)
                .fetch_one(&mut *tx)
                .await?;

                let final_absences_i32 =
                    i32::try_from(subject.results.total_absences).map_err(|_| {
                        ServerError::invalid_params("Subject total_absences exceeds i32 range")
                    })?;

                sqlx::query(
                    r#"
                    INSERT INTO subject_final_results (
                        account_id,
                        subject_id,
                        final_grade,
                        total_absences
                    )
                    VALUES ($1, $2, $3, $4)
                    ON CONFLICT (account_id, subject_id) DO UPDATE SET
                        final_grade = EXCLUDED.final_grade,
                        total_absences = EXCLUDED.total_absences
                    "#,
                )
                .bind(account_id)
                .bind(subject_id)
                .bind(subject.results.grade)
                .bind(final_absences_i32)
                .execute(&mut *tx)
                .await?;

                for bim in &subject.bimesters {
                    if !(1..=4).contains(&bim.bimester) {
                        return Err(ServerError::invalid_params(format!(
                            "Invalid bimester {} for subject '{}': expected 1..=4",
                            bim.bimester, subject.subject_name
                        )));
                    }

                    let bim_i16 = i16::from(bim.bimester);
                    let absences_i32 = i32::try_from(bim.personal.absences).map_err(|_| {
                        ServerError::invalid_params("Grade absences exceeds i32 range")
                    })?;
                    let rec_code = bim
                        .personal
                        .recovery_code
                        .unwrap_or(RecoveryStatusCode::Nac);
                    let rec_msg = bim
                        .personal
                        .recovery_message
                        .unwrap_or_else(|| RecoveryMessage::from_title_or_code(None, rec_code));
                    let recovered = bim.personal.recovered.unwrap_or(false);
                    let approved = bim.personal.grade.is_some_and(|g| g >= 6.0) || recovered;

                    sqlx::query(
                        r#"
                        INSERT INTO grades (
                            account_id,
                            subject_id,
                            bimester,
                            grade,
                            average_grade,
                            absences,
                            recovery_code,
                            recovery_message,
                            approved,
                            recovered
                        )
                        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
                        ON CONFLICT (account_id, subject_id, bimester) DO UPDATE SET
                            grade = EXCLUDED.grade,
                            average_grade = EXCLUDED.average_grade,
                            absences = EXCLUDED.absences,
                            recovery_code = EXCLUDED.recovery_code,
                            recovery_message = EXCLUDED.recovery_message,
                            approved = EXCLUDED.approved,
                            recovered = EXCLUDED.recovered
                        "#,
                    )
                    .bind(account_id)
                    .bind(subject_id)
                    .bind(bim_i16)
                    .bind(bim.personal.grade)
                    .bind(bim.class.average_grade)
                    .bind(absences_i32)
                    .bind(rec_code.as_code_str())
                    .bind(recovery_message_str(rec_msg))
                    .bind(approved)
                    .bind(recovered)
                    .execute(&mut *tx)
                    .await?;
                }
            }
        }

        tx.commit().await?;
        Ok(())
    }

    pub async fn get_boletim(
        &self,
        account_id: i64,
    ) -> Result<Option<AllYearsResponse>, ServerError> {
        let year_rows = sqlx::query_as::<_, AccountSchoolYearRow>(
            r#"
            SELECT asy.school_year_id, sy.year, sy.title, asy.status
            FROM accounts_school_years asy
            JOIN school_years sy ON sy.id = asy.school_year_id
            WHERE asy.account_id = $1
            ORDER BY sy.year ASC, sy.title ASC, sy.id ASC
            "#,
        )
        .bind(account_id)
        .fetch_all(&self.pool)
        .await?;

        if year_rows.is_empty() {
            return Ok(None);
        }

        let bimester_rows = sqlx::query_as::<_, BimesterMetricRow>(
            r#"
            SELECT b.school_year_id, b.user_average, b.class_average, b.total_absences
            FROM bimesters b
            WHERE b.account_id = $1
            ORDER BY b.school_year_id ASC, b.bimester ASC
            "#,
        )
        .bind(account_id)
        .fetch_all(&self.pool)
        .await?;

        let grade_rows = sqlx::query_as::<_, GradeJoinedRow>(
            r#"
            SELECT
                s.school_year_id,
                g.subject_id,
                s.name AS subject_name,
                g.bimester,
                g.grade,
                g.average_grade,
                g.absences,
                g.recovery_code,
                g.recovery_message,
                g.recovered
            FROM grades g
            JOIN subjects s ON s.id = g.subject_id
            WHERE g.account_id = $1
            ORDER BY s.school_year_id ASC, s.name ASC, g.bimester ASC
            "#,
        )
        .bind(account_id)
        .fetch_all(&self.pool)
        .await?;

        let final_rows = sqlx::query_as::<_, SubjectFinalJoinedRow>(
            r#"
            SELECT
                s.school_year_id,
                sfr.subject_id,
                s.name AS subject_name,
                sfr.final_grade,
                sfr.total_absences
            FROM subject_final_results sfr
            JOIN subjects s ON s.id = sfr.subject_id
            WHERE sfr.account_id = $1
            ORDER BY s.school_year_id ASC, s.name ASC
            "#,
        )
        .bind(account_id)
        .fetch_all(&self.pool)
        .await?;

        let mut metrics_by_year: HashMap<i64, Vec<BimesterData>> = HashMap::new();
        for row in bimester_rows {
            metrics_by_year
                .entry(row.school_year_id)
                .or_default()
                .push(BimesterData {
                    user_average: row.user_average.unwrap_or(0.0),
                    class_average: row.class_average.unwrap_or(0.0),
                    total_absences: u32::try_from(row.total_absences.max(0)).unwrap_or(0),
                });
        }

        let mut finals_by_subject: HashMap<(i64, i64), (String, ResultData)> = HashMap::new();
        let mut subject_order_by_year: HashMap<i64, Vec<(i64, String)>> = HashMap::new();
        for row in final_rows {
            finals_by_subject.insert(
                (row.school_year_id, row.subject_id),
                (
                    row.subject_name.clone(),
                    ResultData {
                        grade: row.final_grade,
                        total_absences: u32::try_from(row.total_absences.max(0)).unwrap_or(0),
                    },
                ),
            );
            subject_order_by_year
                .entry(row.school_year_id)
                .or_default()
                .push((row.subject_id, row.subject_name));
        }

        let mut bims_by_subject: HashMap<(i64, i64), Vec<UnifiedBimesterData>> = HashMap::new();
        let mut fallback_names: HashMap<(i64, i64), String> = HashMap::new();
        for row in grade_rows {
            let rec_code = RecoveryStatusCode::from_cell_text(&row.recovery_code);
            let recovery = rec_code != RecoveryStatusCode::Nac;
            let rec_msg = RecoveryMessage::from_title_or_code(
                Some(row.recovery_message.as_str()),
                rec_code,
            );
            let recovered = if recovery { Some(row.recovered) } else { None };
            let bimester_u8 = u8::try_from(row.bimester).unwrap_or(1);

            fallback_names.insert((row.school_year_id, row.subject_id), row.subject_name);
            bims_by_subject
                .entry((row.school_year_id, row.subject_id))
                .or_default()
                .push(UnifiedBimesterData {
                    bimester: bimester_u8,
                    personal: PersonalBiInformation {
                        grade: row.grade,
                        absences: u32::try_from(row.absences.max(0)).unwrap_or(0),
                        recovery,
                        recovered,
                        recovery_code: Some(rec_code),
                        recovery_message: Some(rec_msg),
                    },
                    class: ClassBiInformation {
                        average_grade: row.average_grade,
                    },
                });
        }

        let mut years = Vec::with_capacity(year_rows.len());
        for y in year_rows {
            let year_u16 = u16::try_from(y.year.max(0)).map_err(|_| {
                ServerError::internal("Persisted school_year.year value out of u16 range")
            })?;

            let mut grades = Vec::new();
            if let Some(ordered_subjects) = subject_order_by_year.remove(&y.school_year_id) {
                for (subject_id, subject_name) in ordered_subjects {
                    let results = finals_by_subject
                        .remove(&(y.school_year_id, subject_id))
                        .map(|(_, r)| r)
                        .unwrap_or(ResultData {
                            grade: None,
                            total_absences: 0,
                        });
                    let bimesters = bims_by_subject
                        .remove(&(y.school_year_id, subject_id))
                        .unwrap_or_default();
                    grades.push(FullGrades {
                        subject_name,
                        results,
                        bimesters,
                    });
                }
            }

            let mut extra_subjects: Vec<((i64, i64), Vec<UnifiedBimesterData>)> = bims_by_subject
                .extract_if(|(yid, _), _| *yid == y.school_year_id)
                .collect();
            extra_subjects.sort_by(|a, b| {
                let na = fallback_names.get(&a.0).map(String::as_str).unwrap_or("");
                let nb = fallback_names.get(&b.0).map(String::as_str).unwrap_or("");
                na.cmp(nb)
            });
            for (key, bimesters) in extra_subjects {
                let subject_name = fallback_names.remove(&key).unwrap_or_default();
                grades.push(FullGrades {
                    subject_name,
                    results: ResultData {
                        grade: None,
                        total_absences: 0,
                    },
                    bimesters,
                });
            }
            grades.sort_by(|a, b| a.subject_name.cmp(&b.subject_name));

            let bimesters_metrics = metrics_by_year
                .remove(&y.school_year_id)
                .unwrap_or_default();

            years.push(YearInfo {
                title: y.title,
                year: year_u16,
                status: y.status,
                grades,
                bimesters_metrics,
            });
        }

        let user_current_year = years.len();
        Ok(Some(AllYearsResponse {
            warning: WarningValue::Bool(false),
            user_current_year,
            data: years,
        }))
    }
}
