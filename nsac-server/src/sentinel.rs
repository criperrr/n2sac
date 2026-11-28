//! Sentinel Polling heuristic engine and runtime coordinator (M3).

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Datelike, Utc};
use nsac_core::error::{AppError, ErrorCode};
use nsac_core::filters::normalize_text;
use nsac_core::models::{AllYearsResponse, NsacSessionCookies, RecoveryStatusCode, UnifiedBimesterData};
use nsac_core::sentinel::compute_class_grade_hash;

use crate::config::SentinelConfig;
use crate::db::{
    AccountSummary, GradePostingEvent, NewGradePostingEvent, SentinelClassState, SentinelRepo,
    compute_strong_etag,
};
use crate::error::ServerError;
use crate::fcm::{FcmClient, FcmDeliveryOutcome, StudentGradeHighlight};
use crate::http::nsac_routes::canonicalize_boletim;
use crate::ports::HeuristicObserver;
use crate::state::AppState;

pub const DAYS_IN_YEAR: u16 = 365;

/// Computes the circular distance between two days of the year (`1..=365`),
/// wrapping across New Year boundaries (e.g., day 365 vs day 2 -> distance 2).
pub fn circular_day_distance(day_a: u16, day_b: u16) -> u16 {
    let norm_a = ((day_a.max(1) - 1) % DAYS_IN_YEAR) + 1;
    let norm_b = ((day_b.max(1) - 1) % DAYS_IN_YEAR) + 1;
    let diff = norm_a.abs_diff(norm_b);
    diff.min(DAYS_IN_YEAR.saturating_sub(diff))
}

/// Computes historical grade-posting certainty in `[0.0, 0.85]` based on past
/// `GradePostingEvent` records strictly within `±radius_days` of `now`'s day-of-year.
///
/// Out-of-season events outside `±radius_days` contribute `0.0`, guaranteeing
/// outlier resistance against isolated grade corrections months away.
pub fn compute_historical_certainty(
    events: &[GradePostingEvent],
    now: DateTime<Utc>,
    radius_days: u16,
) -> f64 {
    let now_doy = u16::try_from(now.ordinal()).unwrap_or(1);
    let denom = f64::from(radius_days) + 1.0;

    let mut weight_sum = 0.0_f64;
    let mut matched_any = false;

    for ev in events {
        let ev_doy = u16::try_from(ev.day_of_year.max(1)).unwrap_or(1);
        let d = circular_day_distance(ev_doy, now_doy);
        if d <= radius_days {
            matched_any = true;
            let w = 1.0 - (f64::from(d) / denom) * 0.4;
            weight_sum += w;
        }
    }

    if !matched_any {
        return 0.0;
    }

    let cluster_score = (weight_sum / 3.0).min(1.0);
    (0.30 + 0.55 * cluster_score).clamp(0.30, 0.85)
}

/// Detects the "Post-Exam-Week Early Posting Wave" on the **latest active bimester**
/// of the most recent school year.
///
/// Older bimesters (e.g., 2nd/3rd) may have a few unposted grades due to make-up exams
/// (*reposição*) and are ignored. When the highest bimester with at least 1 posted grade
/// has only a minority of subjects posted (`1%..=45%`), exam week has just ended and
/// grade-posting probability over the next 7 days is high (`[0.55, 0.80]`).
pub fn compute_early_posting_wave_certainty(boletim: &AllYearsResponse) -> f64 {
    let Some(latest_year) = boletim.data.iter().max_by_key(|y| y.year) else {
        return 0.0;
    };
    let total_subjects = latest_year.grades.len();
    if total_subjects < 3 {
        return 0.0;
    }

    for target_bim in (1u8..=4u8).rev() {
        let posted_count = latest_year
            .grades
            .iter()
            .filter(|subj| {
                subj.bimesters.iter().any(|b| {
                    b.bimester == target_bim
                        && (b.class.average_grade.is_some_and(|v| v.is_finite() && v > 0.0)
                            || b.personal.grade.is_some_and(|v| v.is_finite() && v > 0.0))
                })
            })
            .count();

        if posted_count == 0 {
            continue;
        }

        // Found the latest active bimester.
        let ratio = posted_count as f64 / total_subjects as f64;
        if ratio <= 0.45 {
            // Peak certainty when the first 5%..=25% of grades have just started dropping.
            let intensity = 1.0 - ((ratio - 0.15).abs() / 0.35).clamp(0.0, 1.0);
            return (0.55 + 0.25 * intensity).clamp(0.55, 0.80);
        }
        return 0.0;
    }

    0.0
}

/// Maps heuristic certainty and hot-mode state into the next polling interval in seconds.
pub fn compute_interval_from_certainty(
    certainty: f64,
    hot_active: bool,
    config: &SentinelConfig,
) -> i64 {
    let cold = i64::try_from(config.cold_baseline_secs.max(1)).unwrap_or(604_800);
    let hist_raw = i64::try_from(config.historical_interval_secs.max(1)).unwrap_or(7_200);
    let hist = hist_raw.min(cold);
    let hot_min = i64::try_from(config.hot_min_interval_secs.max(1)).unwrap_or(600);

    if hot_active {
        return hot_min;
    }

    if !certainty.is_finite() || certainty <= 0.0 {
        return cold;
    }

    let api_threshold = config.api_volume_weight.clamp(0.0, 1.0) + 0.02;
    if certainty <= api_threshold {
        if cold <= hist {
            return cold;
        }
        let span = (cold - hist).max(0) as f64;
        let weight_denom = config.api_volume_weight.max(0.01);
        let ratio = (certainty / weight_denom).clamp(0.0, 1.0);
        let delta = (span * 0.35 * ratio).round() as i64;
        let reduced = cold.saturating_sub(delta);
        return reduced.max(hist + 1).min(cold);
    }

    // Historical window active (`certainty > api_volume_weight + 0.02`, typically in `[0.30, 0.85]`):
    // Interpolate from `hist` down to `target_low`, staying strictly `<= hist` and `> hot_min` (when `hist > hot_min`).
    let min_hist_floor = if hist > hot_min { hot_min + 1 } else { hist };
    let target_low = (hist / 2).max(min_hist_floor).min(hist);
    let norm = ((certainty - 0.30) / 0.55).clamp(0.0, 1.0);
    let span = (hist - target_low).max(0) as f64;
    let interpolated = hist.saturating_sub((span * norm).round() as i64);

    if hist < cold {
        interpolated.clamp(min_hist_floor, hist)
    } else {
        hist
    }
}

/// Diffs two canonical boletins for a class to extract **newly posted** class-wide
/// `class.average_grade` (M.T.) bimester entries (`None` / `<= 0.0` -> `> 0.0`).
///
/// Recalculations of an already-posted `M.T.` (e.g., `7.5 -> 7.7` caused by a single student's
/// recovery `SAT`/`INS`/`NC`, make-up exam, or grade edit) are intentionally NOT emitted as new
/// class-wide posting events, preventing duplicate `grade_posting_events` and class-wide re-announcements.
pub fn diff_class_grade_events(
    class_code: &str,
    prev: Option<&AllYearsResponse>,
    curr: &AllYearsResponse,
    now: DateTime<Utc>,
) -> Vec<NewGradePostingEvent> {
    let day_of_year = i16::try_from(now.ordinal()).unwrap_or(1);
    let month = i16::try_from(now.month()).unwrap_or(1);
    let day = i16::try_from(now.day()).unwrap_or(1);

    let mut prev_posted: BTreeSet<(u16, String, u8)> = BTreeSet::new();
    if let Some(prev_boletim) = prev {
        for year_info in &prev_boletim.data {
            for subject in &year_info.grades {
                let subj_norm = normalize_subject_key(&subject.subject_name);
                for bim in &subject.bimesters {
                    if let Some(mt) = bim.class.average_grade
                        && mt.is_finite()
                        && mt > 0.0
                    {
                        prev_posted.insert((year_info.year, subj_norm.clone(), bim.bimester));
                    }
                }
            }
        }
    }

    let mut events = Vec::new();

    for year_info in &curr.data {
        let school_year = i32::from(year_info.year);
        for subject in &year_info.grades {
            let subj_norm = normalize_subject_key(&subject.subject_name);
            for bim in &subject.bimesters {
                if let Some(curr_mt) = bim.class.average_grade
                    && curr_mt.is_finite()
                    && curr_mt > 0.0
                {
                    let key = (year_info.year, subj_norm.clone(), bim.bimester);
                    if !prev_posted.contains(&key) {
                        events.push(NewGradePostingEvent {
                            class_code: class_code.to_string(),
                            school_year,
                            bimester: i16::from(bim.bimester),
                            subject_name: subject.subject_name.clone(),
                            day_of_year,
                            month,
                            day,
                            posted_at: now,
                        });
                    }
                }
            }
        }
    }

    events
}

fn normalize_subject_key(subject_name: &str) -> String {
    normalize_text(subject_name)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Removes events whose `(school_year, bimester, normalized_subject_name)` was already
/// persisted in `grade_posting_events` for this class, guaranteeing idempotency across
/// restarts or temporary NSAC unpost/repost cycles.
fn filter_unrecorded_class_events(
    candidates: Vec<NewGradePostingEvent>,
    existing: &[GradePostingEvent],
) -> Vec<NewGradePostingEvent> {
    if candidates.is_empty() || existing.is_empty() {
        return candidates;
    }

    let recorded: BTreeSet<(i32, i16, String)> = existing
        .iter()
        .map(|ev| (ev.school_year, ev.bimester, normalize_subject_key(&ev.subject_name)))
        .collect();

    candidates
        .into_iter()
        .filter(|ev| {
            !recorded.contains(&(
                ev.school_year,
                ev.bimester,
                normalize_subject_key(&ev.subject_name),
            ))
        })
        .collect()
}

fn effective_recovery_code(bi: &UnifiedBimesterData) -> RecoveryStatusCode {
    bi.personal.recovery_code.unwrap_or(RecoveryStatusCode::Nac)
}

fn is_personal_bimester_posted(bi: &UnifiedBimesterData) -> bool {
    let Some(g) = bi.personal.grade else {
        return false;
    };
    if !g.is_finite() {
        return false;
    }
    let rec_code = effective_recovery_code(bi);
    let has_recovery = bi.personal.recovery || rec_code != RecoveryStatusCode::Nac;
    if g.abs() <= 1e-9 && bi.class.average_grade.unwrap_or(0.0).abs() <= 1e-9 && !has_recovery {
        return false;
    }
    true
}

#[derive(Debug, Clone, Copy)]
struct PersonalBimesterSnapshot {
    grade: Option<f64>,
    recovery: bool,
    recovery_code: RecoveryStatusCode,
    recovered: Option<bool>,
}

/// Diffs a single student's previous vs. current boletim and returns highlights **exclusively**
/// for bimesters where **that student's own** personal grade or recovery state (`SAT`, `INS`,
/// `NC`, `NAC`) actually changed.
///
/// Class-wide `M.T.` (`class.average_grade`) shifts alone never produce a highlight, so one
/// student's recovery or grade adjustment never triggers notifications for classmates.
pub fn diff_personal_grade_highlights(
    prev: Option<&AllYearsResponse>,
    curr: &AllYearsResponse,
) -> Vec<StudentGradeHighlight> {
    let Some(prev_boletim) = prev else {
        return Vec::new();
    };

    let mut prev_map: HashMap<(u16, String, u8), PersonalBimesterSnapshot> = HashMap::new();
    for year_info in &prev_boletim.data {
        for subject in &year_info.grades {
            let subj_norm = normalize_subject_key(&subject.subject_name);
            for bi in &subject.bimesters {
                if !is_personal_bimester_posted(bi) {
                    continue;
                }
                prev_map.insert(
                    (year_info.year, subj_norm.clone(), bi.bimester),
                    PersonalBimesterSnapshot {
                        grade: bi.personal.grade,
                        recovery: bi.personal.recovery,
                        recovery_code: effective_recovery_code(bi),
                        recovered: bi.personal.recovered,
                    },
                );
            }
        }
    }

    let mut out = Vec::new();
    let mut seen = BTreeSet::new();

    for year_info in curr.data.iter().rev() {
        for subject in &year_info.grades {
            let subj_norm = normalize_subject_key(&subject.subject_name);
            for bi in &subject.bimesters {
                if !is_personal_bimester_posted(bi) {
                    continue;
                }

                let curr_rec_code = effective_recovery_code(bi);
                let curr_rec = bi.personal.recovery || curr_rec_code != RecoveryStatusCode::Nac;
                let key = (year_info.year, subj_norm.clone(), bi.bimester);

                let changed = match prev_map.get(&key) {
                    None => true,
                    Some(old) => {
                        let grade_changed = match (old.grade, bi.personal.grade) {
                            (Some(a), Some(b)) => (a - b).abs() > 1e-4,
                            (None, None) => false,
                            _ => true,
                        };
                        let recovery_changed = old.recovery != curr_rec
                            || old.recovery_code != curr_rec_code
                            || old.recovered != bi.personal.recovered;
                        grade_changed || recovery_changed
                    }
                };

                if !changed {
                    continue;
                }

                if !seen.insert((year_info.year, subj_norm.clone(), bi.bimester)) {
                    continue;
                }

                let previous_grade = subject
                    .bimesters
                    .iter()
                    .filter(|b| b.bimester < bi.bimester && is_personal_bimester_posted(b))
                    .rev()
                    .find_map(|b| b.personal.grade);

                out.push(StudentGradeHighlight {
                    subject: subject.subject_name.clone(),
                    bimester: bi.bimester,
                    year: i32::from(year_info.year),
                    personal_grade: bi.personal.grade,
                    class_average: bi.class.average_grade,
                    previous_grade,
                    recovery: curr_rec,
                });
            }
        }
    }

    out
}

fn subjects_and_bimesters_from_highlights(
    highlights: &[StudentGradeHighlight],
) -> (Vec<String>, Vec<u8>) {
    let mut unique_subjects = BTreeSet::new();
    let mut unique_bimesters = BTreeSet::new();
    for h in highlights {
        unique_subjects.insert(h.subject.clone());
        unique_bimesters.insert(h.bimester);
    }
    (
        unique_subjects.into_iter().collect(),
        unique_bimesters.into_iter().collect(),
    )
}

#[derive(Debug, Clone)]
pub struct SentinelHeuristic {
    sentinel_repo: SentinelRepo,
    config: SentinelConfig,
}

impl SentinelHeuristic {
    pub fn new(sentinel_repo: SentinelRepo, config: SentinelConfig) -> Self {
        Self {
            sentinel_repo,
            config,
        }
    }

    pub fn arc(sentinel_repo: SentinelRepo, config: SentinelConfig) -> Arc<Self> {
        Arc::new(Self::new(sentinel_repo, config))
    }
}

#[async_trait::async_trait]
impl HeuristicObserver for SentinelHeuristic {
    async fn initialize_class_on_login(
        &self,
        class_code: &str,
        class_hash: &str,
        account_id: i64,
        now: DateTime<Utc>,
    ) -> Result<(), ServerError> {
        let events = self.sentinel_repo.list_events_for_class(class_code).await?;
        let hist_certainty =
            compute_historical_certainty(&events, now, self.config.historical_radius_days);
        let interval_secs = compute_interval_from_certainty(hist_certainty, false, &self.config);

        if let Some(mut existing) = self.sentinel_repo.get_class_state(class_code).await? {
            if existing.last_class_hash.is_none() {
                existing.last_class_hash = Some(class_hash.to_string());
            }
            if existing.active_sentinel_account_id.is_none() {
                existing.active_sentinel_account_id = Some(account_id);
            }
            if existing.hot_until.is_none_or(|h| h <= now) {
                existing.current_certainty = existing.current_certainty.max(hist_certainty);
                existing.current_interval_secs =
                    compute_interval_from_certainty(existing.current_certainty, false, &self.config);
                existing.next_check_at =
                    now + chrono::Duration::seconds(existing.current_interval_secs.max(1));
            }
            existing.updated_at = now;
            self.sentinel_repo.upsert_class_state(&existing).await?;
        } else {
            let state = SentinelClassState {
                class_code: class_code.to_string(),
                last_class_hash: Some(class_hash.to_string()),
                last_checked_at: None,
                last_change_detected_at: None,
                hot_until: None,
                next_check_at: now + chrono::Duration::seconds(interval_secs.max(1)),
                current_certainty: hist_certainty,
                current_interval_secs: interval_secs,
                consecutive_errors: 0,
                last_api_bump_at: None,
                active_sentinel_account_id: Some(account_id),
                updated_at: now,
            };
            self.sentinel_repo.upsert_class_state(&state).await?;
        }

        Ok(())
    }

    async fn record_api_activity(
        &self,
        class_code: &str,
        now: DateTime<Utc>,
    ) -> Result<u64, ServerError> {
        let baseline = i64::try_from(self.config.cold_baseline_secs.max(1)).unwrap_or(604_800);
        let _ = self
            .sentinel_repo
            .ensure_class_initialized(class_code, baseline, now)
            .await?;

        let cooldown_secs = i64::try_from(self.config.api_cooldown_secs).unwrap_or(1_800);
        let bumped_opt = self
            .sentinel_repo
            .try_record_api_bump(class_code, self.config.api_volume_weight, cooldown_secs, now)
            .await?;

        if let Some(mut bumped) = bumped_opt {
            let hot_active = bumped.hot_until.is_some_and(|h| h > now);
            bumped.current_interval_secs =
                compute_interval_from_certainty(bumped.current_certainty, hot_active, &self.config);
            let candidate_next =
                now + chrono::Duration::seconds(bumped.current_interval_secs.max(1));
            if candidate_next < bumped.next_check_at {
                bumped.next_check_at = candidate_next;
            }
            bumped.updated_at = now;
            self.sentinel_repo.upsert_class_state(&bumped).await?;

            return Ok(
                u64::try_from(bumped.current_interval_secs.max(1))
                    .unwrap_or(self.config.cold_baseline_secs),
            );
        }

        Ok(self.get_poll_hint_secs(Some(class_code), now).await)
    }

    async fn get_poll_hint_secs(&self, class_code: Option<&str>, _now: DateTime<Utc>) -> u64 {
        let Some(code) = class_code else {
            return self.config.cold_baseline_secs;
        };

        match self.sentinel_repo.get_class_state(code).await {
            Ok(Some(state)) => u64::try_from(state.current_interval_secs.max(1))
                .unwrap_or(self.config.cold_baseline_secs),
            Ok(None) | Err(_) => self.config.cold_baseline_secs,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct PollCycleReport {
    pub classes_checked: usize,
    pub classes_changed: usize,
    pub classes_errored: usize,
    pub fanout_students_updated: usize,
    pub fcm_delivered: usize,
    pub fcm_unregistered_cleared: usize,
    pub fcm_failed: usize,
}

async fn fetch_account_boletim_with_refresh(
    state: &AppState,
    account_id: i64,
    now: DateTime<Utc>,
) -> Result<AllYearsResponse, ServerError> {
    let secrets = state
        .accounts()
        .get_decrypted_credentials(&state.crypto, account_id)
        .await?
        .ok_or_else(|| ServerError::user_not_linked("Sentinel account not found"))?;

    if !secrets.active {
        return Err(ServerError::forbidden("Sentinel account is inactive"));
    }

    if let Some(cookies) = secrets.cookies {
        let attempt: Result<(AllYearsResponse, Option<NsacSessionCookies>), AppError> = {
            let _permit = state.semaphore.acquire().await?;
            state.nsac.fetch_boletim(&cookies).await
        };

        match attempt {
            Ok((boletim, maybe_rotated)) => {
                if let Some(ref rotated) = maybe_rotated {
                    state
                        .accounts()
                        .update_cookies(&state.crypto, account_id, rotated, now)
                        .await?;
                }
                return Ok(boletim);
            }
            Err(err) if err.code == ErrorCode::NsacAuthExpired => {}
            Err(err) => return Err(ServerError::from(err)),
        }
    }

    let fresh_cookies = {
        let _permit = state.semaphore.acquire().await?;
        state
            .nsac
            .login(&secrets.nsac_email, secrets.password.as_str())
            .await?
    };

    state
        .accounts()
        .update_cookies(&state.crypto, account_id, &fresh_cookies, now)
        .await?;

    let retry = {
        let _permit = state.semaphore.acquire().await?;
        state.nsac.fetch_boletim(&fresh_cookies).await
    };

    match retry {
        Ok((boletim, maybe_rotated)) => {
            if let Some(ref rotated) = maybe_rotated {
                state
                    .accounts()
                    .update_cookies(&state.crypto, account_id, rotated, now)
                    .await?;
            }
            Ok(boletim)
        }
        Err(err) => Err(ServerError::from(err)),
    }
}

fn parse_ini_grade(class_code: &str) -> Option<u8> {
    match class_code {
        "INI1" => Some(1),
        "INI2" => Some(2),
        "INI3" => Some(3),
        _ => None,
    }
}

/// Best-effort lightweight probe to discover an `INI{1..=3}[AB]` subclass anchor
/// from the sentinel account's `ficha-avaliacao` PDF when none exists yet in
/// `auxiliary_snapshots`.
///
/// Errors are strictly logged at `DEBUG` level and never fail the Sentinel tick.
async fn maybe_probe_ini_ab_anchor(state: &AppState, class_code: &str, account_id: i64) {
    let Some(grade) = parse_ini_grade(class_code) else {
        return;
    };

    match state.snapshots().has_ini_ab_anchor_for_grade(grade).await {
        Ok(true) => return,
        Ok(false) => {}
        Err(err) => {
            tracing::debug!(
                class_code,
                account_id,
                error = %err,
                "Failed to check existing INI A/B anchor; skipping sentinel ficha probe"
            );
            return;
        }
    }

    let mut cookies = match state
        .accounts()
        .get_decrypted_credentials(&state.crypto, account_id)
        .await
    {
        Ok(Some(secrets)) if secrets.active => match secrets.cookies {
            Some(c) => c,
            None => return,
        },
        Ok(_) => return,
        Err(err) => {
            tracing::debug!(
                class_code,
                account_id,
                error = %err,
                "Failed to load sentinel cookies for INI A/B ficha probe"
            );
            return;
        }
    };

    let expected_prefix = format!("INI{grade}");
    for bim in 1u8..=4u8 {
        let attempt = {
            let permit = match state.semaphore.acquire().await {
                Ok(permit) => permit,
                Err(err) => {
                    tracing::debug!(
                        class_code,
                        account_id,
                        bimester = bim,
                        error = %err,
                        "Failed to acquire NSAC semaphore for INI A/B ficha probe"
                    );
                    return;
                }
            };
            let res = state.nsac.fetch_ficha_avaliacao_pdf(&cookies, bim).await;
            drop(permit);
            res
        };

        match attempt {
            Ok((Some(pdf), maybe_rotated)) => {
                if let Some(rotated) = maybe_rotated {
                    cookies = rotated;
                    if let Err(err) = state
                        .accounts()
                        .update_cookies(&state.crypto, account_id, &cookies, state.clock.now())
                        .await
                    {
                        tracing::debug!(
                            class_code,
                            account_id,
                            error = %err,
                            "Failed to persist rotated cookies during INI A/B ficha probe"
                        );
                    }
                }

                if let Some(ini_ab) = crate::db::ranking::extract_ini_ab_from_pdf(&pdf)
                    && ini_ab.starts_with(&expected_prefix)
                {
                    let hint_payload = serde_json::json!({
                        "class_code": ini_ab,
                        "bimester": bim,
                    });
                    match compute_strong_etag(&hint_payload) {
                        Ok(etag) => {
                            let now = state.clock.now();
                            match state
                                .snapshots()
                                .upsert(account_id, "ficha_class_hint", &hint_payload, &etag, now)
                                .await
                            {
                                Ok(_) => {
                                    state.caches.invalidate_all_rankings().await;
                                    tracing::info!(
                                        class_code,
                                        account_id,
                                        bimester = bim,
                                        discovered_subclass = %ini_ab,
                                        "Sentinel discovered INI A/B anchor from ficha-avaliacao PDF"
                                    );
                                }
                                Err(err) => {
                                    tracing::debug!(
                                        class_code,
                                        account_id,
                                        bimester = bim,
                                        error = %err,
                                        "Failed to upsert ficha_class_hint snapshot"
                                    );
                                }
                            }
                        }
                        Err(err) => {
                            tracing::debug!(
                                class_code,
                                account_id,
                                bimester = bim,
                                error = %err,
                                "Failed to compute etag for ficha_class_hint snapshot"
                            );
                        }
                    }
                    return;
                }
            }
            Ok((None, maybe_rotated)) => {
                if let Some(rotated) = maybe_rotated {
                    cookies = rotated;
                    let _ = state
                        .accounts()
                        .update_cookies(&state.crypto, account_id, &cookies, state.clock.now())
                        .await;
                }
            }
            Err(err) => {
                tracing::debug!(
                    class_code,
                    account_id,
                    bimester = bim,
                    error = %err,
                    "Sentinel INI A/B ficha probe request failed"
                );
                if matches!(
                    err.code,
                    ErrorCode::NsacAuthExpired | ErrorCode::NsacUnavailable
                ) {
                    return;
                }
            }
        }
    }
}

fn order_sentinel_candidates(
    active_accounts: &[AccountSummary],
    preferred_id: Option<i64>,
) -> Vec<AccountSummary> {
    let mut ordered = Vec::with_capacity(active_accounts.len());
    if let Some(pref) = preferred_id {
        for acc in active_accounts {
            if acc.id == pref {
                ordered.push(acc.clone());
            }
        }
        for acc in active_accounts {
            if acc.id != pref {
                ordered.push(acc.clone());
            }
        }
    } else {
        ordered.extend_from_slice(active_accounts);
    }
    ordered
}

pub async fn poll_due_classes_once(
    state: &AppState,
    fcm: &FcmClient,
) -> Result<PollCycleReport, ServerError> {
    let now = state.clock.now();
    let due_classes = state.sentinel().list_due_classes(now).await?;
    let mut report = PollCycleReport::default();

    for mut class_state in due_classes {
        let active_accounts = state
            .accounts()
            .list_active_by_class(&class_state.class_code)
            .await?;
        if active_accounts.is_empty() {
            continue;
        }

        let candidates =
            order_sentinel_candidates(&active_accounts, class_state.active_sentinel_account_id);

        let mut sentinel_outcome: Option<(i64, AllYearsResponse)> = None;
        for candidate in &candidates {
            match fetch_account_boletim_with_refresh(state, candidate.id, now).await {
                Ok(mut boletim) => {
                    canonicalize_boletim(&mut boletim);
                    sentinel_outcome = Some((candidate.id, boletim));
                    break;
                }
                Err(err) => match err.error_code() {
                    ErrorCode::InvalidNsacCredentials
                    | ErrorCode::NsacAuthExpired
                    | ErrorCode::SessionRefreshError
                    | ErrorCode::Forbidden
                    | ErrorCode::UserNotLinked => {
                        tracing::warn!(
                            class_code = %class_state.class_code,
                            candidate_account_id = candidate.id,
                            error_code = %err.code_str(),
                            "Sentinel candidate failed authentication; failing over to next student in class"
                        );
                        continue;
                    }
                    ErrorCode::NsacUnavailable => {
                        tracing::warn!(
                            class_code = %class_state.class_code,
                            candidate_account_id = candidate.id,
                            "NSAC portal unavailable during sentinel check; entering exponential backoff"
                        );
                        break;
                    }
                    _ => {
                        tracing::warn!(
                            class_code = %class_state.class_code,
                            candidate_account_id = candidate.id,
                            error_code = %err.code_str(),
                            "Sentinel candidate encountered unexpected error"
                        );
                        break;
                    }
                },
            }
        }

        let Some((chosen_sentinel_id, sentinel_boletim)) = sentinel_outcome else {
            class_state.consecutive_errors = class_state.consecutive_errors.saturating_add(1);
            let shift = u32::try_from(class_state.consecutive_errors.saturating_sub(1))
                .unwrap_or(0)
                .min(6);
            let base_backoff =
                i64::try_from(state.config.sentinel.midnight_backoff_secs.max(1)).unwrap_or(1_800);
            let backoff = base_backoff.saturating_mul(1_i64 << shift);
            class_state.next_check_at = now + chrono::Duration::seconds(backoff.max(1));
            class_state.updated_at = now;
            state.sentinel().upsert_class_state(&class_state).await?;
            report.classes_errored += 1;
            continue;
        };

        maybe_probe_ini_ab_anchor(state, &class_state.class_code, chosen_sentinel_id).await;

        report.classes_checked += 1;
        let prev_boletim = state.boletim().get_boletim(chosen_sentinel_id).await?;
        let new_hash = compute_class_grade_hash(&sentinel_boletim);
        let hash_changed = class_state.last_class_hash.as_deref() != Some(new_hash.as_str());

        class_state.consecutive_errors = 0;
        class_state.last_checked_at = Some(now);
        class_state.active_sentinel_account_id = Some(chosen_sentinel_id);
        class_state.updated_at = now;

        if !hash_changed {
            state
                .boletim()
                .upsert_boletim(chosen_sentinel_id, &sentinel_boletim)
                .await?;
            state.caches.invalidate_boletim(chosen_sentinel_id).await;

            if class_state.hot_until.is_some_and(|h| h <= now) {
                class_state.hot_until = None;
            }
            let hot_active = class_state.hot_until.is_some_and(|h| h > now);
            if !hot_active {
                let events = state
                    .sentinel()
                    .list_events_for_class(&class_state.class_code)
                    .await?;
                let hist_cert = compute_historical_certainty(
                    &events,
                    now,
                    state.config.sentinel.historical_radius_days,
                );
                let early_wave_cert = compute_early_posting_wave_certainty(&sentinel_boletim);
                class_state.current_certainty = hist_cert.max(early_wave_cert);
                class_state.current_interval_secs = compute_interval_from_certainty(
                    class_state.current_certainty,
                    false,
                    &state.config.sentinel,
                );
            } else {
                class_state.current_interval_secs = compute_interval_from_certainty(
                    class_state.current_certainty,
                    true,
                    &state.config.sentinel,
                );
            }
            class_state.next_check_at =
                now + chrono::Duration::seconds(class_state.current_interval_secs.max(1));
            state.sentinel().upsert_class_state(&class_state).await?;
            continue;
        }

        // Class hash changed!
        report.classes_changed += 1;
        let existing_events = state
            .sentinel()
            .list_events_for_class(&class_state.class_code)
            .await?;
        let new_events = filter_unrecorded_class_events(
            diff_class_grade_events(
                &class_state.class_code,
                prev_boletim.as_ref(),
                &sentinel_boletim,
                now,
            ),
            &existing_events,
        );
        let has_new_class_postings = !new_events.is_empty();

        if has_new_class_postings {
            state
                .sentinel()
                .insert_posting_events_batch(&new_events)
                .await?;
        }

        class_state.last_class_hash = Some(new_hash);
        if has_new_class_postings {
            let hot_dur =
                i64::try_from(state.config.sentinel.hot_duration_secs.max(1)).unwrap_or(7_200);
            let hot_int =
                i64::try_from(state.config.sentinel.hot_min_interval_secs.max(1)).unwrap_or(600);
            class_state.last_change_detected_at = Some(now);
            class_state.hot_until = Some(now + chrono::Duration::seconds(hot_dur));
            class_state.current_certainty = 1.0;
            class_state.current_interval_secs = hot_int;
            class_state.next_check_at = now + chrono::Duration::seconds(hot_int);
        } else {
            if class_state.hot_until.is_some_and(|h| h <= now) {
                class_state.hot_until = None;
            }
            let hot_active = class_state.hot_until.is_some_and(|h| h > now);
            if !hot_active {
                let hist_cert = compute_historical_certainty(
                    &existing_events,
                    now,
                    state.config.sentinel.historical_radius_days,
                );
                let early_wave_cert = compute_early_posting_wave_certainty(&sentinel_boletim);
                class_state.current_certainty = hist_cert.max(early_wave_cert);
                class_state.current_interval_secs = compute_interval_from_certainty(
                    class_state.current_certainty,
                    false,
                    &state.config.sentinel,
                );
            } else {
                class_state.current_interval_secs = compute_interval_from_certainty(
                    class_state.current_certainty,
                    true,
                    &state.config.sentinel,
                );
            }
            class_state.next_check_at =
                now + chrono::Duration::seconds(class_state.current_interval_secs.max(1));
        }
        state.sentinel().upsert_class_state(&class_state).await?;

        let mut unique_subjects = BTreeSet::new();
        let mut unique_bimesters = BTreeSet::new();
        for ev in &new_events {
            unique_subjects.insert(ev.subject_name.clone());
            if let Ok(b_u8) = u8::try_from(ev.bimester) {
                unique_bimesters.insert(b_u8);
            }
        }
        let class_subjects: Vec<String> = unique_subjects.into_iter().collect();
        let class_bimesters: Vec<u8> = unique_bimesters.into_iter().collect();

        // Persist the sentinel student's already-fetched boletim first
        if let Err(err) = state
            .boletim()
            .upsert_boletim(chosen_sentinel_id, &sentinel_boletim)
            .await
        {
            tracing::warn!(
                class_code = %class_state.class_code,
                student_account_id = chosen_sentinel_id,
                error = %err,
                "Failed to persist sentinel student's updated boletim"
            );
        } else {
            state.caches.invalidate_boletim(chosen_sentinel_id).await;
            report.fanout_students_updated += 1;
        }

        // Fan-out to each student: update DB and dispatch high-priority FCM ONLY when that
        // specific student's personal grade or individual recovery state (SAT/INS/NC) changed.
        // Never notify a student solely because another classmate's recovery or edit shifted M.T.
        let mut class_fcm_sent = 0usize;
        for student in &active_accounts {
            let (personal_highlights, fetch_succeeded) = if student.id == chosen_sentinel_id {
                (
                    diff_personal_grade_highlights(prev_boletim.as_ref(), &sentinel_boletim),
                    true,
                )
            } else {
                let prev_student_boletim = state
                    .boletim()
                    .get_boletim(student.id)
                    .await
                    .ok()
                    .flatten();
                match fetch_account_boletim_with_refresh(state, student.id, now).await {
                    Ok(mut student_boletim) => {
                        canonicalize_boletim(&mut student_boletim);
                        let highlights = diff_personal_grade_highlights(
                            prev_student_boletim.as_ref(),
                            &student_boletim,
                        );
                        match state
                            .boletim()
                            .upsert_boletim(student.id, &student_boletim)
                            .await
                        {
                            Ok(()) => {
                                state.caches.invalidate_boletim(student.id).await;
                                report.fanout_students_updated += 1;
                            }
                            Err(err) => {
                                tracing::warn!(
                                    class_code = %class_state.class_code,
                                    student_account_id = student.id,
                                    error = %err,
                                    "Failed to persist student boletim during fan-out"
                                );
                            }
                        }
                        (highlights, true)
                    }
                    Err(err) => {
                        tracing::warn!(
                            class_code = %class_state.class_code,
                            student_account_id = student.id,
                            error_code = %err.code_str(),
                            "Failed to refresh student boletim during class fan-out"
                        );
                        (Vec::new(), false)
                    }
                }
            };

            let should_notify =
                !personal_highlights.is_empty() || (!fetch_succeeded && has_new_class_postings);
            if !should_notify {
                continue;
            }

            let (student_subjects, student_bimesters) = if !personal_highlights.is_empty() {
                subjects_and_bimesters_from_highlights(&personal_highlights)
            } else {
                (class_subjects.clone(), class_bimesters.clone())
            };

            // Immediately dispatch high-priority FCM push for this student (if enabled in prefs).
            if let Some(token) = student.fcm_token.as_deref().filter(|t| !t.is_empty()) {
                let grades_push_ok = state
                    .classrooms()
                    .is_grades_push_enabled(student.id)
                    .await
                    .unwrap_or(true);
                if !grades_push_ok {
                    continue;
                }
                match fcm
                    .send_grade_notification_detailed(
                        token,
                        &class_state.class_code,
                        &student_subjects,
                        &student_bimesters,
                        &personal_highlights,
                    )
                    .await
                {
                    Ok(FcmDeliveryOutcome::Delivered { .. }) => {
                        report.fcm_delivered += 1;
                        class_fcm_sent += 1;
                    }
                    Ok(FcmDeliveryOutcome::Unregistered) => {
                        let _ = state
                            .accounts()
                            .update_fcm_token(student.id, None, now)
                            .await;
                        report.fcm_unregistered_cleared += 1;
                    }
                    Ok(FcmDeliveryOutcome::TransientFailure { .. }) | Err(_) => {
                        report.fcm_failed += 1;
                    }
                    Ok(FcmDeliveryOutcome::Disabled) => {}
                }
            }
        }

        tracing::info!(
            class_code = %class_state.class_code,
            sentinel_account_id = chosen_sentinel_id,
            new_class_events = new_events.len(),
            fcm_sent = class_fcm_sent,
            "Sentinel detected class M.T. hash change and completed individual-diff fan-out"
        );
    }

    if report.fanout_students_updated > 0 {
        state.caches.invalidate_all_rankings().await;
    }

    Ok(report)
}

/// Immediately handles a class-wide M.T. hash change discovered by a student's manual web/app
/// refresh, using the pre-upsert `prev_boletim` snapshot so only students whose own personal
/// grade or individual recovery state changed receive FCM push notifications.
pub async fn handle_manual_refresh_class_change(
    state: AppState,
    class_code: String,
    refreshed_account_id: i64,
    prev_boletim: Option<AllYearsResponse>,
    fresh_boletim: AllYearsResponse,
    fresh_hash: String,
) {
    let now = state.clock.now();
    let Ok(Some(mut class_state)) = state.sentinel().get_class_state(&class_code).await else {
        return;
    };
    if class_state.last_class_hash.as_deref() == Some(fresh_hash.as_str()) {
        return;
    }

    let existing_events = state
        .sentinel()
        .list_events_for_class(&class_code)
        .await
        .unwrap_or_default();
    let new_events = filter_unrecorded_class_events(
        diff_class_grade_events(&class_code, prev_boletim.as_ref(), &fresh_boletim, now),
        &existing_events,
    );
    let has_new_class_postings = !new_events.is_empty();

    if has_new_class_postings
        && let Err(err) = state
            .sentinel()
            .insert_posting_events_batch(&new_events)
            .await
    {
        tracing::warn!(
            class_code = %class_code,
            error = %err,
            "Failed to persist grade posting events from manual refresh"
        );
    }

    class_state.consecutive_errors = 0;
    class_state.last_checked_at = Some(now);
    class_state.last_class_hash = Some(fresh_hash);
    if has_new_class_postings {
        let hot_dur =
            i64::try_from(state.config.sentinel.hot_duration_secs.max(1)).unwrap_or(7_200);
        let hot_int =
            i64::try_from(state.config.sentinel.hot_min_interval_secs.max(1)).unwrap_or(120);
        class_state.last_change_detected_at = Some(now);
        class_state.hot_until = Some(now + chrono::Duration::seconds(hot_dur));
        class_state.current_certainty = 1.0;
        class_state.current_interval_secs = hot_int;
        class_state.next_check_at = now + chrono::Duration::seconds(hot_int);
    }
    class_state.updated_at = now;
    if let Err(err) = state.sentinel().upsert_class_state(&class_state).await {
        tracing::warn!(
            class_code = %class_code,
            error = %err,
            "Failed to update sentinel class state from manual refresh"
        );
        return;
    }

    tracing::info!(
        class_code = %class_code,
        refreshed_account_id,
        new_class_events = new_events.len(),
        "Class grade hash change detected via manual web/app refresh — running individual-diff FCM fan-out"
    );

    let Ok(active_accounts) = state.accounts().list_active_by_class(&class_code).await else {
        return;
    };

    let mut unique_subjects = BTreeSet::new();
    let mut unique_bimesters = BTreeSet::new();
    for ev in &new_events {
        unique_subjects.insert(ev.subject_name.clone());
        if let Ok(b_u8) = u8::try_from(ev.bimester) {
            unique_bimesters.insert(b_u8);
        }
    }
    let class_subjects: Vec<String> = unique_subjects.into_iter().collect();
    let class_bimesters: Vec<u8> = unique_bimesters.into_iter().collect();

    for student in &active_accounts {
        let (personal_highlights, fetch_succeeded) = if student.id == refreshed_account_id {
            (
                diff_personal_grade_highlights(prev_boletim.as_ref(), &fresh_boletim),
                true,
            )
        } else {
            let prev_student_boletim = state
                .boletim()
                .get_boletim(student.id)
                .await
                .ok()
                .flatten();
            if let Ok(mut student_boletim) =
                fetch_account_boletim_with_refresh(&state, student.id, now).await
            {
                canonicalize_boletim(&mut student_boletim);
                let highlights = diff_personal_grade_highlights(
                    prev_student_boletim.as_ref(),
                    &student_boletim,
                );
                if state
                    .boletim()
                    .upsert_boletim(student.id, &student_boletim)
                    .await
                    .is_ok()
                {
                    state.caches.invalidate_boletim(student.id).await;
                }
                (highlights, true)
            } else {
                (Vec::new(), false)
            }
        };

        let should_notify =
            !personal_highlights.is_empty() || (!fetch_succeeded && has_new_class_postings);
        if !should_notify {
            continue;
        }

        let (student_subjects, student_bimesters) = if !personal_highlights.is_empty() {
            subjects_and_bimesters_from_highlights(&personal_highlights)
        } else {
            (class_subjects.clone(), class_bimesters.clone())
        };

        if let Some(token) = student.fcm_token.as_deref().filter(|t| !t.is_empty()) {
            let grades_push_ok = state
                .classrooms()
                .is_grades_push_enabled(student.id)
                .await
                .unwrap_or(true);
            if !grades_push_ok {
                continue;
            }
            if let Ok(FcmDeliveryOutcome::Unregistered) = state
                .fcm
                .send_grade_notification_detailed(
                    token,
                    &class_code,
                    &student_subjects,
                    &student_bimesters,
                    &personal_highlights,
                )
                .await
            {
                let _ = state
                    .accounts()
                    .update_fcm_token(student.id, None, now)
                    .await;
            }
        }
    }

    state.caches.invalidate_all_rankings().await;
}

pub fn spawn_sentinel_worker(
    state: AppState,
    fcm: FcmClient,
) -> tokio::task::JoinHandle<()> {
    if !state.config.sentinel.enabled {
        return tokio::spawn(async {});
    }

    let tick = Duration::from_millis(state.config.sentinel.tick_ms.max(50));
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(tick).await;
            if let Err(err) = poll_due_classes_once(&state, &fcm).await {
                tracing::warn!(
                    error_code = %err.code_str(),
                    "Sentinel background poll cycle failed"
                );
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;
    use nsac_core::models::{
        AllYearsResponse, ClassBiInformation, FullGrades, PersonalBiInformation, ResultData,
        UnifiedBimesterData, WarningValue, YearInfo,
    };

    use super::*;

    fn test_config() -> SentinelConfig {
        SentinelConfig {
            enabled: true,
            tick_ms: 100,
            cold_baseline_secs: 86_400,
            historical_radius_days: 4,
            historical_interval_secs: 7_200,
            hot_min_interval_secs: 120,
            hot_max_interval_secs: 180,
            hot_duration_secs: 7_200,
            api_volume_weight: 0.15,
            api_cooldown_secs: 1_800,
            midnight_backoff_secs: 1_800,
            jitter_max_ms: 0,
        }
    }

    #[test]
    fn test_circular_day_distance_year_wrap() {
        assert_eq!(circular_day_distance(100, 100), 0);
        assert_eq!(circular_day_distance(100, 104), 4);
        assert_eq!(circular_day_distance(365, 2), 2);
        assert_eq!(circular_day_distance(2, 365), 2);
        assert_eq!(circular_day_distance(364, 1), 2);
    }

    #[test]
    fn test_historical_certainty_outlier_resistance_and_cluster() {
        let now = Utc
            .with_ymd_and_hms(2025, 4, 24, 12, 0, 0)
            .single()
            .unwrap_or_default();
        let now_doy = i16::try_from(now.ordinal()).unwrap_or(114);

        let outlier = GradePostingEvent {
            id: 1,
            class_code: "INF3".to_string(),
            school_year: 2024,
            bimester: 1,
            subject_name: "MATEMATICA".to_string(),
            day_of_year: now_doy + 90,
            month: 7,
            day: 23,
            posted_at: now,
        };
        assert_eq!(compute_historical_certainty(&[outlier], now, 4), 0.0);

        let in_window_1 = GradePostingEvent {
            id: 2,
            class_code: "INF3".to_string(),
            school_year: 2023,
            bimester: 1,
            subject_name: "PORTUGUES".to_string(),
            day_of_year: now_doy - 1,
            month: 4,
            day: 23,
            posted_at: now,
        };
        let in_window_2 = GradePostingEvent {
            id: 3,
            class_code: "INF3".to_string(),
            school_year: 2024,
            bimester: 1,
            subject_name: "FISICA".to_string(),
            day_of_year: now_doy + 1,
            month: 4,
            day: 25,
            posted_at: now,
        };

        let cert = compute_historical_certainty(&[in_window_1, in_window_2], now, 4);
        assert!((0.45..=0.85).contains(&cert), "unexpected certainty {cert}");
    }

    #[test]
    fn test_compute_interval_tiers() {
        let cfg = test_config();

        // Cold
        assert_eq!(compute_interval_from_certainty(0.0, false, &cfg), 86_400);

        // Secondary API bump only (0.15) -> reduced below cold, strictly > historical (7_200)
        let api_only = compute_interval_from_certainty(0.15, false, &cfg);
        assert!(api_only < 86_400);
        assert!(api_only > 7_200);

        // Historical window active (certainty >= 0.30) -> <= 7_200 and > 120
        let hist_active = compute_interval_from_certainty(0.55, false, &cfg);
        assert!(hist_active <= 7_200);
        assert!(hist_active > 120);

        // Hot mode active -> 120
        assert_eq!(compute_interval_from_certainty(1.0, true, &cfg), 120);
    }

    #[test]
    fn test_diff_class_grade_events_detects_new_bimester_mt() {
        let now = Utc::now();
        let make_boletim = |mt1: Option<f64>, mt2: Option<f64>| AllYearsResponse {
            warning: WarningValue::Bool(false),
            user_current_year: 1,
            data: vec![YearInfo {
                title: "INF3".to_string(),
                year: 2025,
                status: "Cursando".to_string(),
                grades: vec![FullGrades {
                    subject_name: "MATEMATICA".to_string(),
                    results: ResultData {
                        grade: None,
                        total_absences: 0,
                    },
                    bimesters: vec![
                        UnifiedBimesterData {
                            bimester: 1,
                            personal: PersonalBiInformation {
                                grade: mt1,
                                absences: 0,
                                recovery: false,
                                recovered: None,
                                recovery_code: None,
                                recovery_message: None,
                            },
                            class: ClassBiInformation { average_grade: mt1 },
                        },
                        UnifiedBimesterData {
                            bimester: 2,
                            personal: PersonalBiInformation {
                                grade: mt2,
                                absences: 0,
                                recovery: false,
                                recovered: None,
                                recovery_code: None,
                                recovery_message: None,
                            },
                            class: ClassBiInformation { average_grade: mt2 },
                        },
                    ],
                }],
                bimesters_metrics: Vec::new(),
            }],
        };

        let prev = make_boletim(Some(7.5), None);
        let curr = make_boletim(Some(7.5), Some(8.2));
        let diff = diff_class_grade_events("INF3", Some(&prev), &curr, now);
        assert_eq!(diff.len(), 1);
        assert_eq!(diff[0].bimester, 2);
        assert_eq!(diff[0].subject_name, "MATEMATICA");

        // Recalculation of an already-posted M.T. (e.g. 8.2 -> 8.3 after a single student's
        // recovery or correction) must NOT re-emit a class-wide posting event.
        let recalc = make_boletim(Some(7.5), Some(8.3));
        let diff_recalc = diff_class_grade_events("INF3", Some(&curr), &recalc, now);
        assert!(
            diff_recalc.is_empty(),
            "M.T. recalculation must not re-announce already-posted subjects"
        );
    }

    #[test]
    fn test_diff_personal_grade_highlights_ignores_class_mt_only_and_detects_individual_recovery() {
        let make_student = |personal_grade: Option<f64>,
                            class_mt: Option<f64>,
                            rec_code: RecoveryStatusCode,
                            recovered: Option<bool>| {
            let recovery = rec_code != RecoveryStatusCode::Nac;
            AllYearsResponse {
                warning: WarningValue::Bool(false),
                user_current_year: 1,
                data: vec![YearInfo {
                    title: "INI3".to_string(),
                    year: 2026,
                    status: "Cursando".to_string(),
                    grades: vec![FullGrades {
                        subject_name: "Língua Portuguesa e Literatura".to_string(),
                        results: ResultData {
                            grade: None,
                            total_absences: 0,
                        },
                        bimesters: vec![UnifiedBimesterData {
                            bimester: 4,
                            personal: PersonalBiInformation {
                                grade: personal_grade,
                                absences: 0,
                                recovery,
                                recovered,
                                recovery_code: Some(rec_code),
                                recovery_message: None,
                            },
                            class: ClassBiInformation {
                                average_grade: class_mt,
                            },
                        }],
                    }],
                    bimesters_metrics: Vec::new(),
                }],
            }
        };

        // Classmate A: personal grade 8.0 unchanged, only class M.T. shifted 7.0 -> 7.2
        // because another student recovered -> MUST produce 0 highlights.
        let mate_prev = make_student(Some(8.0), Some(7.0), RecoveryStatusCode::Nac, None);
        let mate_curr = make_student(Some(8.0), Some(7.2), RecoveryStatusCode::Nac, None);
        assert!(
            diff_personal_grade_highlights(Some(&mate_prev), &mate_curr).is_empty(),
            "Class M.T. shift alone must never notify classmates"
        );

        // Student B: recovered with SAT (grade 4.5 -> 6.0, NAC -> SAT) -> MUST notify Student B.
        let b_prev = make_student(Some(4.5), Some(7.0), RecoveryStatusCode::Nac, None);
        let b_curr = make_student(Some(6.0), Some(7.2), RecoveryStatusCode::Sat, Some(true));
        let b_hl = diff_personal_grade_highlights(Some(&b_prev), &b_curr);
        assert_eq!(b_hl.len(), 1);
        assert_eq!(b_hl[0].personal_grade, Some(6.0));
        assert!(b_hl[0].recovery);

        // Student C: recovery status changed NAC -> INS or NAC -> NC even with same numeric grade -> MUST notify Student C.
        let c_prev = make_student(Some(4.0), Some(7.0), RecoveryStatusCode::Nac, None);
        let c_ins = make_student(Some(4.0), Some(7.0), RecoveryStatusCode::Ins, Some(false));
        let c_hl = diff_personal_grade_highlights(Some(&c_prev), &c_ins);
        assert_eq!(c_hl.len(), 1);
        assert!(c_hl[0].recovery);

        let c_nc = make_student(Some(4.0), Some(7.0), RecoveryStatusCode::Nc, Some(false));
        let c_nc_hl = diff_personal_grade_highlights(Some(&c_prev), &c_nc);
        assert_eq!(c_nc_hl.len(), 1);
        assert!(c_nc_hl[0].recovery);
    }
}


