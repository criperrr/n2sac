//! Sentinel Polling heuristic engine and runtime coordinator (M3).

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Datelike, Utc};
use nsac_core::error::{AppError, ErrorCode};
use nsac_core::filters::normalize_text;
use nsac_core::models::{AllYearsResponse, NsacSessionCookies};
use nsac_core::sentinel::compute_class_grade_hash;

use crate::config::SentinelConfig;
use crate::db::{
    AccountSummary, GradePostingEvent, NewGradePostingEvent, SentinelClassState, SentinelRepo,
};
use crate::error::ServerError;
use crate::fcm::{FcmClient, FcmDeliveryOutcome};
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
    let cold = i64::try_from(config.cold_baseline_secs.max(1)).unwrap_or(86_400);
    let hist_raw = i64::try_from(config.historical_interval_secs.max(1)).unwrap_or(7_200);
    let hist = hist_raw.min(cold);
    let hot_min = i64::try_from(config.hot_min_interval_secs.max(1)).unwrap_or(120);

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

/// Diffs two canonical boletins for a class to extract newly posted or updated class-wide
/// `class.average_grade` (M.T.) bimester entries.
pub fn diff_class_grade_events(
    class_code: &str,
    prev: Option<&AllYearsResponse>,
    curr: &AllYearsResponse,
    now: DateTime<Utc>,
) -> Vec<NewGradePostingEvent> {
    let day_of_year = i16::try_from(now.ordinal()).unwrap_or(1);
    let month = i16::try_from(now.month()).unwrap_or(1);
    let day = i16::try_from(now.day()).unwrap_or(1);

    let mut prev_map: HashMap<(u16, String, u8), f64> = HashMap::new();
    if let Some(prev_boletim) = prev {
        for year_info in &prev_boletim.data {
            for subject in &year_info.grades {
                let subj_norm = normalize_text(&subject.subject_name)
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ");
                for bim in &subject.bimesters {
                    if let Some(mt) = bim.class.average_grade
                        && mt.is_finite()
                        && mt > 0.0
                    {
                        prev_map.insert((year_info.year, subj_norm.clone(), bim.bimester), mt);
                    }
                }
            }
        }
    }

    let mut events = Vec::new();
    let mut fallback_candidate: Option<NewGradePostingEvent> = None;

    for year_info in &curr.data {
        let school_year = i32::from(year_info.year);
        for subject in &year_info.grades {
            let subj_norm = normalize_text(&subject.subject_name)
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            for bim in &subject.bimesters {
                let bimester = i16::from(bim.bimester);
                if let Some(curr_mt) = bim.class.average_grade
                    && curr_mt.is_finite()
                {
                    if fallback_candidate.is_none() {
                        fallback_candidate = Some(NewGradePostingEvent {
                            class_code: class_code.to_string(),
                            school_year,
                            bimester,
                            subject_name: subject.subject_name.clone(),
                            day_of_year,
                            month,
                            day,
                            posted_at: now,
                        });
                    }

                    if curr_mt > 0.0 {
                        let key = (year_info.year, subj_norm.clone(), bim.bimester);
                        let changed = match prev_map.get(&key) {
                            Some(&prev_mt) => (curr_mt - prev_mt).abs() > 1e-4,
                            None => true,
                        };
                        if changed {
                            events.push(NewGradePostingEvent {
                                class_code: class_code.to_string(),
                                school_year,
                                bimester,
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
    }

    if events.is_empty() {
        if let Some(candidate) = fallback_candidate {
            events.push(candidate);
        } else if let Some(first_year) = curr.data.last() {
            let school_year = i32::from(first_year.year);
            let (subject_name, bimester) = first_year
                .grades
                .first()
                .map(|g| {
                    let bim = g.bimesters.first().map_or(1_i16, |b| i16::from(b.bimester));
                    (g.subject_name.clone(), bim)
                })
                .unwrap_or_else(|| ("GERAL".to_string(), 1));
            events.push(NewGradePostingEvent {
                class_code: class_code.to_string(),
                school_year,
                bimester,
                subject_name,
                day_of_year,
                month,
                day,
                posted_at: now,
            });
        }
    }

    events
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
        let baseline = i64::try_from(self.config.cold_baseline_secs.max(1)).unwrap_or(86_400);
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
        let new_events = diff_class_grade_events(
            &class_state.class_code,
            prev_boletim.as_ref(),
            &sentinel_boletim,
            now,
        );
        state
            .sentinel()
            .insert_posting_events_batch(&new_events)
            .await?;

        let hot_dur = i64::try_from(state.config.sentinel.hot_duration_secs.max(1)).unwrap_or(7_200);
        let hot_int =
            i64::try_from(state.config.sentinel.hot_min_interval_secs.max(1)).unwrap_or(120);

        class_state.last_class_hash = Some(new_hash);
        class_state.last_change_detected_at = Some(now);
        class_state.hot_until = Some(now + chrono::Duration::seconds(hot_dur));
        class_state.current_certainty = 1.0;
        class_state.current_interval_secs = hot_int;
        class_state.next_check_at = now + chrono::Duration::seconds(hot_int);
        state.sentinel().upsert_class_state(&class_state).await?;

        crate::discord::broadcast_discord_class_grade_update(
            &state.db,
            &class_state.class_code,
            &new_events,
            Some(&sentinel_boletim),
        )
        .await;

        let mut unique_subjects = BTreeSet::new();
        let mut unique_bimesters = BTreeSet::new();
        for ev in &new_events {
            unique_subjects.insert(ev.subject_name.clone());
            if let Ok(b_u8) = u8::try_from(ev.bimester) {
                unique_bimesters.insert(b_u8);
            }
        }
        let subjects: Vec<String> = unique_subjects.into_iter().collect();
        let bimesters: Vec<u8> = unique_bimesters.into_iter().collect();

        // Persist the sentinel student's already-fetched boletim first
        state
            .boletim()
            .upsert_boletim(chosen_sentinel_id, &sentinel_boletim)
            .await?;
        state.caches.invalidate_boletim(chosen_sentinel_id).await;
        report.fanout_students_updated += 1;

        // Fan-out to remaining students in the class and send opt-in private Discord DMs
        for student in &active_accounts {
            if student.id == chosen_sentinel_id {
                if let Some(ref did) = student.discord_id {
                    let _ = crate::discord::send_discord_personal_grade_dm(
                        &state.db,
                        student.id,
                        did,
                        &class_state.class_code,
                        &new_events,
                        &sentinel_boletim,
                    )
                    .await;
                }
            } else {
                match fetch_account_boletim_with_refresh(state, student.id, now).await {
                    Ok(mut student_boletim) => {
                        canonicalize_boletim(&mut student_boletim);
                        state
                            .boletim()
                            .upsert_boletim(student.id, &student_boletim)
                            .await?;
                        state.caches.invalidate_boletim(student.id).await;
                        report.fanout_students_updated += 1;

                        if let Some(ref did) = student.discord_id {
                            let _ = crate::discord::send_discord_personal_grade_dm(
                                &state.db,
                                student.id,
                                did,
                                &class_state.class_code,
                                &new_events,
                                &student_boletim,
                            )
                            .await;
                        }
                    }
                    Err(err) => {
                        tracing::warn!(
                            class_code = %class_state.class_code,
                            student_account_id = student.id,
                            error_code = %err.code_str(),
                            "Failed to refresh student boletim during class fan-out"
                        );
                    }
                }
            }
        }

        // Dispatch FCM push notifications to all active students in the class with a token
        for student in &active_accounts {
            if let Some(token) = student.fcm_token.as_deref().filter(|t| !t.is_empty()) {
                match fcm
                    .send_grade_notification(token, &class_state.class_code, &subjects, &bimesters)
                    .await
                {
                    Ok(FcmDeliveryOutcome::Delivered { .. }) => {
                        report.fcm_delivered += 1;
                    }
                    Ok(FcmDeliveryOutcome::Unregistered) => {
                        let _ = state
                            .accounts()
                            .update_fcm_token(student.id, None, now)
                            .await?;
                        report.fcm_unregistered_cleared += 1;
                    }
                    Ok(FcmDeliveryOutcome::TransientFailure { .. }) | Err(_) => {
                        report.fcm_failed += 1;
                    }
                    Ok(FcmDeliveryOutcome::Disabled) => {}
                }
            }
        }
    }

    Ok(report)
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
    }
}


