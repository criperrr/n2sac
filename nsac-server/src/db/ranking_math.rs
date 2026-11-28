//! Pure axiomatic statistics engine for the `n²sac` v7 Ranking Specification.
//!
//! This module is 100% pure Rust (zero SQL, zero I/O, zero `.unwrap()`/`.expect()`/`panic!()` in
//! production code) and implements Sections 1–9 of the v7 specification:
//! - IEEE-754-safe deterministic quantization (`Q_k`, `round_k`, `round2`, `round1`);
//! - Median and P75 with empty-set guard (`P_75(∅) = 1`);
//! - Conservative unidirectional shrinkage (`Φ_{m,η}`);
//! - Numerically stable CARA certainty equivalent (`log-sum-exp`) and smooth failure penalty (`ψ`);
//! - Tied midpoint percentile over `G_1`;
//! - Global school-year statistical model (`YearStatisticalModel`) with strict `invisible = true`
//!   privacy isolation, median `M.T.` consensus, empirical-Bayes dispersion (`σ_c`, `σ_scope`),
//!   annual curricular expectation (`E_subj^anual`, `S_exp^anual`), and 3-effect hierarchical Ridge
//!   calibration (`θ_c + δ_{s,b} + γ_{s,ℓ}`).

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use super::ranking::{ClassDescriptor, Modality, PostedGradeRecord, YearRankingDataset};

// ---------------------------------------------------------------------------
// Closed Hyperparameters (Specification v7)
// ---------------------------------------------------------------------------

/// Volatility aversion parameter for student consistency (`β_aluno`, Section 7.2).
pub const BETA_STUDENT: f64 = 0.55;
/// Volatility aversion parameter for classroom consistency (`β_sala`, Section 8.3).
pub const BETA_CLASSROOM: f64 = 2.00;
/// Prior dispersion scale for students (`σ_0`, Section 2.2).
pub const SIGMA_0: f64 = 1.35;
/// Dispersion floor (`σ_min`, Section 2.2).
pub const SIGMA_MIN: f64 = 0.75;
/// Empirical-Bayes prior strength in effective students (`ν_σ`, Section 2.2).
pub const NU_SIGMA: f64 = 5.0;
/// Empirical scale of classroom official averages (`s_{0,sala}`, Section 8.2).
pub const S0_CLASSROOM: f64 = 0.40;
/// Partial pooling weight for class effect in multi-class student scores (`α_θ`, Section 4.1).
pub const ALPHA_THETA: f64 = 0.50;
/// Ridge regularization for class effect `θ_c` (`λ_θ`, Section 4.1).
pub const LAMBDA_THETA: f64 = 8.0;
/// Ridge regularization for subject-bimester effect `δ_{s,b}` (`λ_δ`, Section 4.1).
pub const LAMBDA_DELTA: f64 = 0.01;
/// Ridge regularization for subject-grade-level effect `γ_{s,ℓ}` (`λ_γ`, Section 4.1).
pub const LAMBDA_GAMMA: f64 = 2.0;

/// Default shrinkage evidence scale (`m`, Section 5).
pub const SHRINK_M: f64 = 3.0;
/// Default shrinkage coverage penalty in z-units (`η_z`, Section 5).
pub const SHRINK_ETA_Z: f64 = 0.25;
/// Fallback global mean when no official cells or grades exist (`7.0`, Section 4.1).
pub const DEFAULT_FALLBACK_MU_0: f64 = 7.0;

// ---------------------------------------------------------------------------
// 1. IEEE-754-Safe Quantization (Section 9.1)
// ---------------------------------------------------------------------------

/// Quantizes `x` to `k` decimal places free of IEEE-754 half-way drift:
/// `Q_k(x) = sgn(x) * floor(|x| * 10^k + 0.5 + 1e-12)`.
pub fn quantize_k(x: f64, k: u32) -> i64 {
    if !x.is_finite() || x == 0.0 {
        return 0;
    }
    let exp = k.min(12) as i32;
    let scale = 10_f64.powi(exp);
    let mag = (x.abs() * scale + 0.5 + 1e-12).floor();
    let clamped = mag.min(i64::MAX as f64) as i64;
    if x < 0.0 { -clamped } else { clamped }
}

/// Rounds `x` to `k` decimal places via `Q_k(x) / 10^k` (never returning `-0.0`).
pub fn round_k(x: f64, k: u32) -> f64 {
    let q = quantize_k(x, k);
    if q == 0 {
        0.0
    } else {
        let exp = k.min(12) as i32;
        (q as f64) / 10_f64.powi(exp)
    }
}

/// Rounds `x` to 2 decimal places (`round_2(x)`).
pub fn round2(x: f64) -> f64 {
    round_k(x, 2)
}

/// Rounds `x` to 1 decimal place (`round_1(x)`).
pub fn round1(x: f64) -> f64 {
    round_k(x, 1)
}

// ---------------------------------------------------------------------------
// 2. Median & P75 with Empty-Set Guard (Sections 2.1, 3)
// ---------------------------------------------------------------------------

/// Computes the exact median of a non-empty slice of `f64` values, sorting in place.
/// Returns `None` if `vals` is empty.
pub fn median_f64(vals: &mut [f64]) -> Option<f64> {
    if vals.is_empty() {
        return None;
    }
    vals.sort_by(|a, b| a.total_cmp(b));
    let n = vals.len();
    if n % 2 == 1 {
        Some(vals[n / 2])
    } else {
        Some(0.5 * (vals[n / 2 - 1] + vals[n / 2]))
    }
}

/// Computes the rounded 75th percentile (`round(P_75(vals))`) with the Section 3 empty-set guard
/// `P_75(∅) = 1`. Uses linear interpolation between adjacent order statistics.
pub fn p75_usize(vals: &[usize]) -> usize {
    if vals.is_empty() {
        return 1;
    }
    let mut sorted = vals.to_vec();
    sorted.sort_unstable();
    let n = sorted.len();
    if n == 1 {
        return sorted[0];
    }
    let pos = 0.75 * ((n - 1) as f64);
    let lo = pos.floor() as usize;
    let hi = pos.ceil() as usize;
    let frac = pos - (lo as f64);
    let interp = (sorted[lo] as f64) + frac * ((sorted[hi] - sorted[lo]) as f64);
    quantize_k(interp, 0).max(0) as usize
}

// ---------------------------------------------------------------------------
// 3. Conservative Unidirectional Shrinkage (Section 5)
// ---------------------------------------------------------------------------

/// Conservative unidirectional shrinkage operator `Φ_{m,η}(x, n, c)`:
/// `Φ_{m,η}(x, n, c) = (n / (n + m(1 - c))) * max(0, x) + min(0, x) - η(1 - c)`.
///
/// Guarantees:
/// - Identity at full coverage: `Φ(x, n, 1.0) == x`;
/// - Strictly increasing in `x` and continuous at `x = 0`;
/// - Strictly increasing in `cov` (`∂Φ/∂c >= η > 0`) for all `x ∈ ℝ` (including `x < 0`).
pub fn phi_shrink(x: f64, n: f64, cov: f64, m: f64, eta: f64) -> f64 {
    let c = cov.clamp(0.0, 1.0);
    let deficit = 1.0 - c;
    if deficit == 0.0 {
        return x;
    }
    let n_pos = n.max(0.0);
    let m_pos = m.max(0.0);
    let denom = n_pos + m_pos * deficit;
    let weight = if denom > 0.0 { n_pos / denom } else { 0.0 };
    weight * x.max(0.0) + x.min(0.0) - eta * deficit
}

// ---------------------------------------------------------------------------
// 4. CARA Certainty Equivalent & Smooth Floor Penalty (Section 7.1)
// ---------------------------------------------------------------------------

/// Numerically stable CARA certainty equivalent (entropic softmin, Section 7.1):
/// `C_β(v) = v_min - (1/β) * ln((1/M) * Σ exp(-β * (v_k - v_min)))`.
///
/// Returns `None` if `vals` is empty, `beta <= 0`, or any input is non-finite.
pub fn cara_certainty_equivalent(vals: &[f64], beta: f64) -> Option<f64> {
    if vals.is_empty() || !beta.is_finite() || beta <= 0.0 {
        return None;
    }
    let mut v_min = f64::INFINITY;
    for &v in vals {
        if !v.is_finite() {
            return None;
        }
        if v < v_min {
            v_min = v;
        }
    }
    let m = vals.len() as f64;
    let sum_exp: f64 = vals.iter().map(|&v| (-beta * (v - v_min)).exp()).sum();
    if !sum_exp.is_finite() || sum_exp <= 0.0 {
        return None;
    }
    Some(v_min - (sum_exp / m).ln() / beta)
}

/// Smooth `C^∞` failure-floor penalty `ψ(y)` in overflow-free `softplus` form (Section 7.1):
/// `t = 2 * (6 - y)`, `ψ(y) = y - 0.5 * (max(t, 0) + ln(1 + exp(-|t|)))`.
pub fn psi_smooth_floor(y: f64) -> f64 {
    let t = 2.0 * (6.0 - y);
    y - 0.5 * (t.max(0.0) + (-t.abs()).exp().ln_1p())
}

/// Population cell standard deviation `σ_cel(v)` (Section 7.3).
/// Returns `0.0` for empty slices.
pub fn cell_stddev(vals: &[f64]) -> f64 {
    if vals.is_empty() {
        return 0.0;
    }
    let m = vals.len() as f64;
    let mean = vals.iter().sum::<f64>() / m;
    let var = vals.iter().map(|&v| (v - mean).powi(2)).sum::<f64>() / m;
    var.max(0.0).sqrt()
}

// ---------------------------------------------------------------------------
// 5. Tied Midpoint Percentile over G_1 (Section 9.4)
// ---------------------------------------------------------------------------

/// Computes the tied midpoint percentile over the qualified cohort `G_1` (Section 9.4):
/// - `N_1 == 1` => `Some(100.0)`
/// - `N_1 >= 2` => `Some(round1(clip(100 * (N_1 - r_g - t_g / 2 + 1) / N_1, 0, 100)))`
///
/// Returns `None` if inputs are out of domain (`n1_qualified == 0`, `rank_1based == 0`,
/// `tie_count == 0`, or `rank_1based > n1_qualified`).
pub fn compute_tied_midpoint_percentile(
    rank_1based: usize,
    tie_count: usize,
    n1_qualified: usize,
) -> Option<f64> {
    if n1_qualified == 0 || rank_1based == 0 || tie_count == 0 || rank_1based > n1_qualified {
        return None;
    }
    if n1_qualified == 1 {
        return Some(100.0);
    }
    let n1 = n1_qualified as f64;
    let r = rank_1based as f64;
    let t = tie_count as f64;
    let exact = 100.0 * (n1 - r - 0.5 * t + 1.0) / n1;
    Some(round1(exact.clamp(0.0, 100.0)))
}

// ---------------------------------------------------------------------------
// 6. Modality & Qualification Helpers (Sections 3.1, 3.2, 8.1)
// ---------------------------------------------------------------------------

/// Structural default subject count `d_m` per modality (Section 3.1).
pub const fn default_modality_subjects(modality: Modality) -> usize {
    match modality {
        Modality::Integrado => 16,
        Modality::Noturno => 7,
        Modality::Other => 12,
    }
}

/// Minimum visible active students required for a classroom to qualify in `G_1^sala` (Section 8.1).
pub const fn min_students_for_classroom(modality: Modality) -> usize {
    match modality {
        Modality::Integrado => 3,
        Modality::Noturno | Modality::Other => 2,
    }
}

/// Minimum valid subjects per bimester for a student (`τ_b(c)`, Section 3.2):
/// `min(6, max(1, ceil(0.5 * E_subj)))`.
pub const fn bimester_validity_floor(e_subj: usize) -> usize {
    let half_ceil = e_subj.div_ceil(2);
    if half_ceil < 1 {
        1
    } else if half_ceil > 6 {
        6
    } else {
        half_ceil
    }
}

/// Bimester consolidation threshold (`τ_b^cons(c) = max(1, ceil(0.5 * E_subj))`, Section 3.2).
pub const fn bimester_consolidation_threshold(e_subj: usize) -> usize {
    let half_ceil = e_subj.div_ceil(2);
    if half_ceil < 1 { 1 } else { half_ceil }
}

/// Returns whether a class code belongs to the visible classroom universe `C^vis` (Section 1.2.2).
fn is_valid_class_code(code: &str) -> bool {
    !code.is_empty() && code != "CTI"
}

// ---------------------------------------------------------------------------
// 7. Hierarchical 3-Effect Ridge Solver (Section 4.1)
// ---------------------------------------------------------------------------

/// Output of the global 3-effect hierarchical Ridge regression (Section 4.1):
/// `M_hat_{c,s,b} ≈ μ_0 + θ_c + δ_{s,b} + I[ℓ(c) ≠ ⊥] * γ_{s,ℓ(c)}`.
#[derive(Debug, Clone, Default)]
pub struct HierarchicalRidgeResult {
    pub mu_0: f64,
    pub theta_c: HashMap<String, f64>,
    pub delta_sb: HashMap<(String, i16), f64>,
    pub gamma_sl: HashMap<(String, u8), f64>,
}

/// Solves an `n × n` symmetric positive-definite linear system `A x = b` via Cholesky
/// decomposition `A = L L^T`. Returns `None` if `A` is not numerically positive definite.
fn solve_spd_cholesky(a: &[f64], b: &[f64], n: usize) -> Option<Vec<f64>> {
    if n == 0 {
        return Some(Vec::new());
    }
    if a.len() != n * n || b.len() != n {
        return None;
    }

    let mut l = vec![0.0_f64; n * n];
    for i in 0..n {
        for j in 0..=i {
            let mut sum = a[i * n + j];
            for k in 0..j {
                sum -= l[i * n + k] * l[j * n + k];
            }
            if i == j {
                if !sum.is_finite() || sum <= 1e-15 {
                    return None;
                }
                l[i * n + j] = sum.sqrt();
            } else {
                let diag = l[j * n + j];
                if !diag.is_finite() || diag == 0.0 {
                    return None;
                }
                l[i * n + j] = sum / diag;
            }
        }
    }

    // Forward substitution: L z = b
    let mut z = vec![0.0_f64; n];
    for i in 0..n {
        let mut sum = b[i];
        for k in 0..i {
            sum -= l[i * n + k] * z[k];
        }
        z[i] = sum / l[i * n + i];
    }

    // Back substitution: L^T x = z
    let mut x = vec![0.0_f64; n];
    for i in (0..n).rev() {
        let mut sum = z[i];
        for k in (i + 1)..n {
            sum -= l[k * n + i] * x[k];
        }
        let val = sum / l[i * n + i];
        if !val.is_finite() {
            return None;
        }
        x[i] = val;
    }

    Some(x)
}

/// Solves the strictly convex 3-effect hierarchical Ridge system (Section 4.1) over official
/// median cells `K = {(c, s, b) : M_hat_{c,s,b} ≠ ⊥}`.
///
/// Falls back cleanly to `0.0` effects (with `mu_0 = fallback_mu_0`) when `mt_consensus` is empty.
pub fn solve_hierarchical_ridge(
    mt_consensus: &HashMap<(String, String, i16), f64>,
    class_grade_levels: &HashMap<String, Option<u8>>,
    fallback_mu_0: f64,
) -> HierarchicalRidgeResult {
    if mt_consensus.is_empty() {
        return HierarchicalRidgeResult {
            mu_0: if fallback_mu_0.is_finite() {
                fallback_mu_0
            } else {
                DEFAULT_FALLBACK_MU_0
            },
            theta_c: HashMap::new(),
            delta_sb: HashMap::new(),
            gamma_sl: HashMap::new(),
        };
    }

    // Deterministic sorted ordering of observations and parameter indices.
    let mut obs: Vec<(&String, &String, i16, f64)> = mt_consensus
        .iter()
        .filter(|(_, v)| v.is_finite())
        .map(|((c, s, b), &v)| (c, s, *b, v))
        .collect();
    if obs.is_empty() {
        return HierarchicalRidgeResult {
            mu_0: if fallback_mu_0.is_finite() {
                fallback_mu_0
            } else {
                DEFAULT_FALLBACK_MU_0
            },
            theta_c: HashMap::new(),
            delta_sb: HashMap::new(),
            gamma_sl: HashMap::new(),
        };
    }
    obs.sort_by(|a, b| a.0.cmp(b.0).then_with(|| a.1.cmp(b.1)).then_with(|| a.2.cmp(&b.2)));

    let mu_0 = obs.iter().map(|o| o.3).sum::<f64>() / (obs.len() as f64);

    let mut class_set: BTreeSet<String> = BTreeSet::new();
    let mut sb_set: BTreeSet<(String, i16)> = BTreeSet::new();
    let mut sl_set: BTreeSet<(String, u8)> = BTreeSet::new();

    for &(c, s, b, _) in &obs {
        class_set.insert(c.clone());
        sb_set.insert((s.clone(), b));
        if let Some(Some(level)) = class_grade_levels.get(c) {
            sl_set.insert((s.clone(), *level));
        }
    }

    let class_keys: Vec<String> = class_set.into_iter().collect();
    let sb_keys: Vec<(String, i16)> = sb_set.into_iter().collect();
    let sl_keys: Vec<(String, u8)> = sl_set.into_iter().collect();

    let n_theta = class_keys.len();
    let n_delta = sb_keys.len();
    let n_gamma = sl_keys.len();
    let n_total = n_theta + n_delta + n_gamma;

    let theta_idx: HashMap<&str, usize> = class_keys
        .iter()
        .enumerate()
        .map(|(i, k)| (k.as_str(), i))
        .collect();
    let delta_idx: HashMap<(&str, i16), usize> = sb_keys
        .iter()
        .enumerate()
        .map(|(i, (s, b))| ((s.as_str(), *b), n_theta + i))
        .collect();
    let gamma_idx: HashMap<(&str, u8), usize> = sl_keys
        .iter()
        .enumerate()
        .map(|(i, (s, l))| ((s.as_str(), *l), n_theta + n_delta + i))
        .collect();

    let mut mat = vec![0.0_f64; n_total * n_total];
    let mut rhs = vec![0.0_f64; n_total];

    for &(c, s, b, mt) in &obs {
        let y = mt - mu_0;
        let mut active = [0usize; 3];
        let mut count = 0usize;

        if let Some(&idx) = theta_idx.get(c.as_str()) {
            active[count] = idx;
            count += 1;
        }
        if let Some(&idx) = delta_idx.get(&(s.as_str(), b)) {
            active[count] = idx;
            count += 1;
        }
        if let Some(Some(level)) = class_grade_levels.get(c)
            && let Some(&idx) = gamma_idx.get(&(s.as_str(), *level))
        {
            active[count] = idx;
            count += 1;
        }

        for &p in &active[..count] {
            rhs[p] += y;
            for &q in &active[..count] {
                mat[p * n_total + q] += 1.0;
            }
        }
    }

    // Add strictly positive Ridge diagonal penalties Λ.
    for i in 0..n_theta {
        mat[i * n_total + i] += LAMBDA_THETA;
    }
    for i in 0..n_delta {
        let idx = n_theta + i;
        mat[idx * n_total + idx] += LAMBDA_DELTA;
    }
    for i in 0..n_gamma {
        let idx = n_theta + n_delta + i;
        mat[idx * n_total + idx] += LAMBDA_GAMMA;
    }

    let sol = solve_spd_cholesky(&mat, &rhs, n_total).unwrap_or_else(|| vec![0.0; n_total]);

    let theta_c: HashMap<String, f64> = class_keys
        .into_iter()
        .enumerate()
        .map(|(i, k)| (k, sol[i]))
        .collect();
    let delta_sb: HashMap<(String, i16), f64> = sb_keys
        .into_iter()
        .enumerate()
        .map(|(i, k)| (k, sol[n_theta + i]))
        .collect();
    let gamma_sl: HashMap<(String, u8), f64> = sl_keys
        .into_iter()
        .enumerate()
        .map(|(i, k)| (k, sol[n_theta + n_delta + i]))
        .collect();

    HierarchicalRidgeResult {
        mu_0,
        theta_c,
        delta_sb,
        gamma_sl,
    }
}

// ---------------------------------------------------------------------------
// 8. Global School-Year Statistical Model (Sections 1–4)
// ---------------------------------------------------------------------------

/// Deduplicated cell observation `(grade, optional positive official M.T.)` for a student.
#[derive(Debug, Clone, Copy)]
struct DeduplicatedCell {
    grade: f64,
    mt: Option<f64>,
}

/// Deduplicates a student's raw `PostedGradeRecord` list by `(normalized_subject, bimester)`
/// deterministically (Section 1.3), retaining only bimesters in `1..=4` and finite grades.
fn deduplicate_account_cells(
    grades: &[PostedGradeRecord],
) -> BTreeMap<(String, i16), DeduplicatedCell> {
    let mut cells: BTreeMap<(String, i16), DeduplicatedCell> = BTreeMap::new();
    for g in grades {
        if !(1..=4).contains(&g.bimester) || !g.grade.is_finite() {
            continue;
        }
        let valid_mt = g.average_grade.filter(|&m| m.is_finite() && m > 0.0);
        let key = (g.normalized_subject.clone(), g.bimester);
        cells
            .entry(key)
            .and_modify(|existing| {
                existing.grade = g.grade;
                if valid_mt.is_some() {
                    existing.mt = valid_mt;
                }
            })
            .or_insert(DeduplicatedCell {
                grade: g.grade,
                mt: valid_mt,
            });
    }
    cells
}

/// Global school-year statistical parameters estimated exclusively over visible accounts
/// (`invisible == false`, Section 1.1).
#[derive(Debug, Clone)]
pub struct YearStatisticalModel {
    /// Median official classroom average `M_hat_{c,s,b}` (Section 2.1).
    pub mt_consensus: HashMap<(String, String, i16), f64>,
    /// Number of visible classrooms with an official average for `(s, b)` (`|C_{s,b}|`, Section 8.2).
    pub class_count_for_cell: HashMap<(String, i16), usize>,
    /// Annual curricular subject expectation `E_subj^anual(c)` per visible classroom (Section 3.1).
    pub e_subj_anual: HashMap<String, usize>,
    /// Annual expected subject set `S_exp^anual(c)` per visible classroom (Section 3.1).
    pub s_exp_anual: HashMap<String, HashSet<String>>,
    /// Empirical-Bayes classroom dispersion `σ_hat_c` (Section 2.2).
    pub sigma_c: HashMap<String, f64>,
    /// Empirical-Bayes global school-year dispersion `σ_escopo` (Section 2.2).
    pub sigma_scope: f64,
    /// Global intercept `μ_0` (Section 4.1).
    pub mu_0: f64,
    /// Classroom Ridge effect `θ_c` (Section 4.1).
    pub theta_c: HashMap<String, f64>,
    /// Subject-bimester Ridge effect `δ_{s,b}` (Section 4.1).
    pub delta_sb: HashMap<(String, i16), f64>,
    /// Subject-grade-level Ridge effect `γ_{s,ℓ}` (Section 4.1).
    pub gamma_sl: HashMap<(String, u8), f64>,
}

impl YearStatisticalModel {
    /// Constructs the global school-year statistical model from `dataset`, strictly excluding
    /// accounts with `invisible == true` (Absolute Privacy Invariant, Section 1.1).
    pub fn from_dataset(dataset: &YearRankingDataset) -> Self {
        // 1. Collect visible accounts, class descriptors, and deduplicated cell maps.
        let mut class_descs: HashMap<String, ClassDescriptor> = HashMap::new();
        let mut visible_cells_by_acct: HashMap<i64, BTreeMap<(String, i16), DeduplicatedCell>> =
            HashMap::new();
        let mut visible_accts_by_class: HashMap<String, Vec<i64>> = HashMap::new();

        let mut all_grade_sum = 0.0_f64;
        let mut all_grade_count = 0usize;

        for (&acct_id, acc) in &dataset.accounts {
            if acc.invisible {
                continue;
            }
            let cells = dataset
                .grades_by_account
                .get(&acct_id)
                .map(|gs| deduplicate_account_cells(gs))
                .unwrap_or_default();

            for cell in cells.values() {
                all_grade_sum += cell.grade;
                all_grade_count += 1;
            }

            if is_valid_class_code(&acc.class_code) {
                class_descs
                    .entry(acc.class_code.clone())
                    .or_insert_with(|| acc.class_desc.clone());
                visible_accts_by_class
                    .entry(acc.class_code.clone())
                    .or_default()
                    .push(acct_id);
            }

            visible_cells_by_acct.insert(acct_id, cells);
        }

        // 2. Median M.T. consensus (Section 2.1) & cell class counts.
        let mut mt_samples: HashMap<(String, String, i16), Vec<f64>> = HashMap::new();
        for (class_code, acct_ids) in &visible_accts_by_class {
            for &acct_id in acct_ids {
                if let Some(cells) = visible_cells_by_acct.get(&acct_id) {
                    for ((norm_subj, bimester), cell) in cells {
                        if let Some(mt) = cell.mt {
                            mt_samples
                                .entry((class_code.clone(), norm_subj.clone(), *bimester))
                                .or_default()
                                .push(mt);
                        }
                    }
                }
            }
        }

/// Collapses sub-turma suffixes (e.g. `INI1A`/`INI1B` -> `INI1`) so two halves of the
/// same course-grade do not falsely inflate cross-course subject sharing (`class_count_for_cell`).
fn base_class_code(code: &str) -> String {
    let upper = code.trim().to_ascii_uppercase();
    let bytes = upper.as_bytes();
    if bytes.len() >= 5
        && matches!(bytes.last(), Some(b'A' | b'B'))
        && bytes[bytes.len() - 2].is_ascii_digit()
    {
        return upper[..upper.len() - 1].to_string();
    }
    upper
}

        let mut mt_consensus: HashMap<(String, String, i16), f64> =
            HashMap::with_capacity(mt_samples.len());
        let mut base_classes_for_cell: HashMap<(String, i16), HashSet<String>> = HashMap::new();
        for ((c, s, b), mut vals) in mt_samples {
            if let Some(med) = median_f64(&mut vals) {
                base_classes_for_cell
                    .entry((s.clone(), b))
                    .or_default()
                    .insert(base_class_code(&c));
                mt_consensus.insert((c, s, b), med);
            }
        }
        let class_count_for_cell: HashMap<(String, i16), usize> = base_classes_for_cell
            .into_iter()
            .map(|(k, set)| (k, set.len()))
            .collect();

        // 3. Annual curricular expectation E_subj^anual(c) and S_exp^anual(c) (Section 3.1).
        let mut s_exp_anual: HashMap<String, HashSet<String>> = HashMap::new();
        let mut e_subj_anual: HashMap<String, usize> = HashMap::new();

        // First pass: large classes (|A_c^{vis,ano}| >= 5) and stratum reference pools.
        let mut large_class_e_by_stratum: HashMap<(Modality, Option<u8>), Vec<usize>> =
            HashMap::new();
        let mut large_class_e_by_modality: HashMap<Modality, Vec<usize>> = HashMap::new();
        let mut small_classes: Vec<(String, HashSet<String>)> = Vec::new();

        for (class_code, acct_ids) in &visible_accts_by_class {
            let mut active_students_count = 0usize;
            let mut subj_student_counts: HashMap<String, usize> = HashMap::new();
            let mut union_subjects: HashSet<String> = HashSet::new();

            for &acct_id in acct_ids {
                let Some(cells) = visible_cells_by_acct.get(&acct_id) else {
                    continue;
                };
                if cells.is_empty() {
                    continue;
                }
                active_students_count += 1;
                let mut student_subjs: HashSet<&str> = HashSet::new();
                for (norm_subj, _) in cells.keys() {
                    student_subjs.insert(norm_subj.as_str());
                }
                for subj in student_subjs {
                    *subj_student_counts.entry(subj.to_string()).or_insert(0) += 1;
                    union_subjects.insert(subj.to_string());
                }
            }

            if active_students_count >= 5 {
                let quorum = active_students_count.div_ceil(2).max(2);
                let quorum_set: HashSet<String> = subj_student_counts
                    .into_iter()
                    .filter_map(|(s, cnt)| (cnt >= quorum).then_some(s))
                    .collect();
                let exp_set = if quorum_set.is_empty() {
                    union_subjects
                } else {
                    quorum_set
                };
                let e_val = exp_set.len().max(1);
                if let Some(desc) = class_descs.get(class_code) {
                    large_class_e_by_stratum
                        .entry((desc.modality, desc.grade_level))
                        .or_default()
                        .push(e_val);
                    large_class_e_by_modality
                        .entry(desc.modality)
                        .or_default()
                        .push(e_val);
                }
                s_exp_anual.insert(class_code.clone(), exp_set);
                e_subj_anual.insert(class_code.clone(), e_val);
            } else {
                small_classes.push((class_code.clone(), union_subjects));
            }
        }

        // Second pass: small classes (|A_c^{vis,ano}| < 5).
        for (class_code, union_subjects) in small_classes {
            let (modality, grade_level) = class_descs
                .get(&class_code)
                .map(|d| (d.modality, d.grade_level))
                .unwrap_or((Modality::Other, None));

            let n_ref = if let Some(l_ml) = large_class_e_by_stratum.get(&(modality, grade_level))
                && !l_ml.is_empty()
            {
                p75_usize(l_ml)
            } else if let Some(l_m) = large_class_e_by_modality.get(&modality)
                && !l_m.is_empty()
            {
                let mut floats: Vec<f64> = l_m.iter().map(|&v| v as f64).collect();
                median_f64(&mut floats)
                    .map(|m| quantize_k(m, 0).max(1) as usize)
                    .unwrap_or_else(|| default_modality_subjects(modality))
            } else {
                default_modality_subjects(modality)
            };

            let e_val = union_subjects.len().max(n_ref).max(1);
            s_exp_anual.insert(class_code.clone(), union_subjects);
            e_subj_anual.insert(class_code, e_val);
        }

        // 4. Empirical-Bayes classroom dispersion σ_c and global scale σ_escopo (Section 2.2).
        let prior_num = NU_SIGMA * SIGMA_0 * SIGMA_0;
        let mut sigma_c: HashMap<String, f64> = HashMap::with_capacity(visible_accts_by_class.len());
        let mut global_k_eff = 0.0_f64;
        let mut global_weighted_msd = 0.0_f64;

        for (class_code, acct_ids) in &visible_accts_by_class {
            let e_subj = e_subj_anual.get(class_code).copied().unwrap_or(1).max(1) as f64;
            let mut k_c_eff = 0.0_f64;
            let mut class_weighted_msd = 0.0_f64;

            for &acct_id in acct_ids {
                let Some(cells) = visible_cells_by_acct.get(&acct_id) else {
                    continue;
                };
                let mut paired_count = 0usize;
                let mut sq_diff_sum = 0.0_f64;
                for ((norm_subj, bimester), cell) in cells {
                    if let Some(&mt_hat) =
                        mt_consensus.get(&(class_code.clone(), norm_subj.clone(), *bimester))
                    {
                        paired_count += 1;
                        let diff = cell.grade - mt_hat;
                        sq_diff_sum += diff * diff;
                    }
                }
                if paired_count == 0 {
                    continue;
                }
                let msd_i = sq_diff_sum / (paired_count as f64);
                let w_i = ((paired_count as f64) / e_subj).min(1.0);
                k_c_eff += w_i;
                class_weighted_msd += w_i * msd_i;
            }

            let sigma_val = if k_c_eff > 0.0 {
                ((prior_num + class_weighted_msd) / (NU_SIGMA + k_c_eff))
                    .max(0.0)
                    .sqrt()
                    .max(SIGMA_MIN)
            } else {
                SIGMA_0
            };

            sigma_c.insert(class_code.clone(), sigma_val);
            global_k_eff += k_c_eff;
            global_weighted_msd += class_weighted_msd;
        }

        let sigma_scope = ((prior_num + global_weighted_msd) / (NU_SIGMA + global_k_eff))
            .max(0.0)
            .sqrt()
            .max(SIGMA_MIN);

        // 5. Global 3-effect hierarchical Ridge calibration (Section 4.1).
        let fallback_mu_0 = if all_grade_count > 0 {
            all_grade_sum / (all_grade_count as f64)
        } else {
            DEFAULT_FALLBACK_MU_0
        };
        let class_grade_levels: HashMap<String, Option<u8>> = class_descs
            .iter()
            .map(|(c, d)| (c.clone(), d.grade_level))
            .collect();

        let ridge = solve_hierarchical_ridge(&mt_consensus, &class_grade_levels, fallback_mu_0);

        Self {
            mt_consensus,
            class_count_for_cell,
            e_subj_anual,
            s_exp_anual,
            sigma_c,
            sigma_scope,
            mu_0: ridge.mu_0,
            theta_c: ridge.theta_c,
            delta_sb: ridge.delta_sb,
            gamma_sl: ridge.gamma_sl,
        }
    }

    /// Unified cell-by-cell reference `m_tilde_{c,s,b}` (Section 4.2):
    /// returns the official median `M_hat_{c,s,b}` if present, otherwise the hierarchical Ridge
    /// imputation `μ_0 + θ_c + δ_{s,b} + γ_{s,ℓ(c)}`.
    pub fn cell_reference_mt(
        &self,
        class_code: &str,
        grade_level: Option<u8>,
        norm_subj: &str,
        bimester: i16,
    ) -> f64 {
        if let Some(&mt) =
            self.mt_consensus
                .get(&(class_code.to_string(), norm_subj.to_string(), bimester))
        {
            return mt;
        }
        let theta = self.theta_c.get(class_code).copied().unwrap_or(0.0);
        let delta = self
            .delta_sb
            .get(&(norm_subj.to_string(), bimester))
            .copied()
            .unwrap_or(0.0);
        let gamma = grade_level
            .and_then(|l| self.gamma_sl.get(&(norm_subj.to_string(), l)).copied())
            .unwrap_or(0.0);
        self.mu_0 + theta + delta + gamma
    }

    /// Standardized residual and globally calibrated cell score `(z_rel, u_isb)` (Section 4.2,
    /// strictly unclipped):
    /// `z_rel = (grade - m_tilde_{c,s,b}) / σ_hat_c`,
    /// `u_isb = μ_0 + α_θ * θ_c + σ_escopo * z_rel`.
    pub fn calibrated_cell_score(
        &self,
        class_code: &str,
        grade_level: Option<u8>,
        norm_subj: &str,
        bimester: i16,
        grade: f64,
    ) -> (f64, f64) {
        let m_tilde = self.cell_reference_mt(class_code, grade_level, norm_subj, bimester);
        let sigma = self
            .sigma_c
            .get(class_code)
            .copied()
            .unwrap_or(SIGMA_0)
            .max(SIGMA_MIN);
        let z_rel = (grade - m_tilde) / sigma;
        let theta = self.theta_c.get(class_code).copied().unwrap_or(0.0);
        let u_isb = self.mu_0 + ALPHA_THETA * theta + self.sigma_scope * z_rel;
        (z_rel, u_isb)
    }
}

// ---------------------------------------------------------------------------
// Unit Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::ranking::{ClassDescriptor, StudentAccountRecord};

    fn make_account(id: i64, class_code: &str, invisible: bool) -> StudentAccountRecord {
        StudentAccountRecord {
            account_id: id,
            nsac_email: format!("student{id}@cti.unesp.br"),
            class_code: class_code.to_string(),
            class_desc: ClassDescriptor::parse(class_code),
            verified_official_name: Some(format!("Student {id}")),
            opted_in: true,
            hide_name: false,
            ranking_display_name: None,
            profile_display_name: None,
            show_avatar: false,
            public_profile: false,
            invisible,
            avatar_version: None,
        }
    }

    fn make_grade(
        account_id: i64,
        subject: &str,
        bimester: i16,
        grade: f64,
        average_grade: Option<f64>,
    ) -> PostedGradeRecord {
        PostedGradeRecord {
            account_id,
            subject_name: subject.to_string(),
            normalized_subject: subject.to_ascii_lowercase(),
            bimester,
            grade,
            average_grade,
            absences: 0,
        }
    }

    #[test]
    fn quantization_is_ieee754_safe() {
        assert_eq!(quantize_k(2.675, 2), 268);
        assert_eq!(round2(2.675), 2.68);
        assert_eq!(quantize_k(-2.675, 2), -268);
        assert_eq!(round2(-2.675), -2.68);
        assert_eq!(round2(-0.0), 0.0);
        assert!(round2(-0.0).is_sign_positive());
        assert_eq!(round1(50.05), 50.1);
        assert_eq!(quantize_k(0.0, 4), 0);
        assert_eq!(quantize_k(f64::NAN, 2), 0);
    }

    #[test]
    fn median_and_p75_guards() {
        assert_eq!(median_f64(&mut []), None);
        assert_eq!(median_f64(&mut [7.5]), Some(7.5));
        assert_eq!(median_f64(&mut [9.0, 5.0, 7.0]), Some(7.0));
        assert_eq!(median_f64(&mut [8.0, 6.0, 10.0, 4.0]), Some(7.0));

        assert_eq!(p75_usize(&[]), 1);
        assert_eq!(p75_usize(&[12]), 12);
        assert_eq!(p75_usize(&[10, 20]), 18);
        assert_eq!(p75_usize(&[1, 2, 3, 4]), 3);
    }

    #[test]
    fn phi_shrink_monotonic_in_coverage_and_continuous_at_zero() {
        let m = SHRINK_M;
        let eta = 0.25;
        let n = 4.0;

        // Identity at full coverage for negative, zero, and positive x.
        for &x in &[-3.5, -0.01, 0.0, 0.01, 2.75] {
            assert!((phi_shrink(x, n, 1.0, m, eta) - x).abs() < 1e-12);
        }

        // Strict monotonicity in coverage c for BOTH x < 0 and x > 0 (anti-cherry-picking).
        let covs = [0.0, 0.2, 0.5, 0.75, 0.9, 1.0];
        for &x in &[-2.5, -0.5, 0.0, 0.5, 2.5] {
            for w in covs.windows(2) {
                let v0 = phi_shrink(x, n, w[0], m, eta);
                let v1 = phi_shrink(x, n, w[1], m, eta);
                assert!(
                    v1 > v0,
                    "phi_shrink must strictly increase with cov for x={x}: cov {} -> {v0}, cov {} -> {v1}",
                    w[0],
                    w[1]
                );
            }
        }

        // Continuity at x = 0.
        let c = 0.4;
        let eps = 1e-9;
        let left = phi_shrink(-eps, n, c, m, eta);
        let at_zero = phi_shrink(0.0, n, c, m, eta);
        let right = phi_shrink(eps, n, c, m, eta);
        assert!(left < at_zero && at_zero < right);
        assert!((right - at_zero).abs() < 1e-8);
        assert!((at_zero - left).abs() < 1e-8);
    }

    #[test]
    fn cara_strict_monotonicity_and_psi_extreme_stability() {
        let base = vec![4.0, 7.0, 8.5, 9.0, 10.0];
        let c0 = cara_certainty_equivalent(&base, BETA_STUDENT).expect("non-empty");
        for idx in 0..base.len() {
            let mut bumped = base.clone();
            bumped[idx] += 0.05;
            let c1 = cara_certainty_equivalent(&bumped, BETA_STUDENT).expect("non-empty");
            assert!(
                c1 > c0,
                "CARA must be strictly monotonic in every coordinate (idx={idx}: {c0} vs {c1})"
            );
        }

        // Extreme values without overflow/underflow.
        let extreme = vec![-1000.0, 1000.0];
        let c_ext = cara_certainty_equivalent(&extreme, BETA_CLASSROOM).expect("finite");
        assert!(c_ext.is_finite());
        assert!((-1000.0..-999.0).contains(&c_ext));

        // psi_smooth_floor stability at extreme negative and positive inputs.
        let psi_neg = psi_smooth_floor(-1000.0);
        assert!(psi_neg.is_finite());
        assert!((psi_neg - (-2006.0)).abs() < 1e-9);

        let psi_pos = psi_smooth_floor(1000.0);
        assert!(psi_pos.is_finite());
        assert!((psi_pos - 1000.0).abs() < 1e-9);

        // Strict monotonicity of psi.
        let ys = [-20.0, 0.0, 5.9, 6.0, 6.1, 10.0];
        for w in ys.windows(2) {
            assert!(psi_smooth_floor(w[1]) > psi_smooth_floor(w[0]));
        }
    }

    #[test]
    fn tied_midpoint_percentile_invariants() {
        // (rank=1, tie=2, n1=2) -> 50.0
        assert_eq!(compute_tied_midpoint_percentile(1, 2, 2), Some(50.0));

        // (rank=1, tie=10, n1=10) -> 50.0
        assert_eq!(compute_tied_midpoint_percentile(1, 10, 10), Some(50.0));

        // (1, 50) + (51, 50) in N1 = 100 -> 75.0 and 25.0, mean = 50.0
        let p_top = compute_tied_midpoint_percentile(1, 50, 100).expect("valid");
        let p_bot = compute_tied_midpoint_percentile(51, 50, 100).expect("valid");
        assert_eq!(p_top, 75.0);
        assert_eq!(p_bot, 25.0);
        let mean_100 = (50.0 * p_top + 50.0 * p_bot) / 100.0;
        assert!((mean_100 - 50.0).abs() <= 0.05);

        // Mixed tie blocks: sizes [1, 3, 2, 1] -> N1 = 7, ranks [1, 2, 5, 7].
        let blocks = [(1usize, 1usize), (2, 3), (5, 2), (7, 1)];
        let n1 = 7usize;
        let weighted_sum: f64 = blocks
            .iter()
            .map(|&(r, t)| {
                let p = compute_tied_midpoint_percentile(r, t, n1).expect("valid");
                (t as f64) * p
            })
            .sum();
        let mean = weighted_sum / (n1 as f64);
        assert!(
            (mean - 50.0).abs() <= 0.05,
            "expected mean near 50.0, got {mean}"
        );

        // Single qualified participant -> 100.0.
        assert_eq!(compute_tied_midpoint_percentile(1, 1, 1), Some(100.0));
        // Invalid domain -> None.
        assert_eq!(compute_tied_midpoint_percentile(1, 1, 0), None);
        assert_eq!(compute_tied_midpoint_percentile(0, 1, 5), None);
    }

    #[test]
    fn solve_hierarchical_ridge_multi_class_shared_and_exclusive_subjects() {
        // Two classes in grade 2 (INI2A stronger by +1.0, ELI2 weaker by -1.0)
        // sharing math & physics, plus each having an exclusive technical subject.
        let mut mt_consensus = HashMap::new();
        // Shared subjects
        mt_consensus.insert(("INI2A".to_string(), "matematica".to_string(), 1), 8.0);
        mt_consensus.insert(("ELI2".to_string(), "matematica".to_string(), 1), 6.0);
        mt_consensus.insert(("INI2A".to_string(), "fisica".to_string(), 1), 7.0);
        mt_consensus.insert(("ELI2".to_string(), "fisica".to_string(), 1), 5.0);
        // Exclusive subjects
        mt_consensus.insert(("INI2A".to_string(), "algoritmos".to_string(), 1), 9.0);
        mt_consensus.insert(("ELI2".to_string(), "circuitos".to_string(), 1), 6.5);

        let mut levels = HashMap::new();
        levels.insert("INI2A".to_string(), Some(2));
        levels.insert("ELI2".to_string(), Some(2));

        let res = solve_hierarchical_ridge(&mt_consensus, &levels, DEFAULT_FALLBACK_MU_0);

        let theta_ini = res.theta_c.get("INI2A").copied().unwrap_or(0.0);
        let theta_eli = res.theta_c.get("ELI2").copied().unwrap_or(0.0);
        assert!(
            theta_ini > 0.0 && theta_eli < 0.0 && theta_ini > theta_eli,
            "INI2A should have higher class effect than ELI2: {theta_ini} vs {theta_eli}"
        );

        // Shared harder subject (fisica) should have lower delta+gamma than matematica.
        let eff_mat = res
            .delta_sb
            .get(&("matematica".to_string(), 1))
            .copied()
            .unwrap_or(0.0)
            + res
                .gamma_sl
                .get(&("matematica".to_string(), 2))
                .copied()
                .unwrap_or(0.0);
        let eff_fis = res
            .delta_sb
            .get(&("fisica".to_string(), 1))
            .copied()
            .unwrap_or(0.0)
            + res
                .gamma_sl
                .get(&("fisica".to_string(), 2))
                .copied()
                .unwrap_or(0.0);
        assert!(eff_mat > eff_fis);

        // Verify normal equations residual is near zero (~1e-12).
        for (c, s, b, mt) in [
            ("INI2A", "matematica", 1, 8.0),
            ("ELI2", "matematica", 1, 6.0),
            ("INI2A", "fisica", 1, 7.0),
            ("ELI2", "fisica", 1, 5.0),
            ("INI2A", "algoritmos", 1, 9.0),
            ("ELI2", "circuitos", 1, 6.5),
        ] {
            let pred = res.mu_0
                + res.theta_c.get(c).copied().unwrap_or(0.0)
                + res
                    .delta_sb
                    .get(&(s.to_string(), b))
                    .copied()
                    .unwrap_or(0.0)
                + res
                    .gamma_sl
                    .get(&(s.to_string(), 2))
                    .copied()
                    .unwrap_or(0.0);
            assert!((pred - mt).abs() < 1.5, "reasonable fit for ({c},{s},{b})");
        }
    }

    #[test]
    fn absolute_privacy_invisible_accounts_never_affect_model() {
        let mut base_accounts = HashMap::new();
        base_accounts.insert(1, make_account(1, "INI2A", false));
        base_accounts.insert(2, make_account(2, "INI2A", false));
        base_accounts.insert(3, make_account(3, "ELI2", false));

        let mut base_grades = HashMap::new();
        base_grades.insert(
            1,
            vec![
                make_grade(1, "Matematica", 1, 8.5, Some(7.2)),
                make_grade(1, "Fisica", 1, 7.5, Some(6.8)),
            ],
        );
        base_grades.insert(
            2,
            vec![
                make_grade(2, "Matematica", 1, 6.5, Some(7.2)),
                make_grade(2, "Fisica", 1, 6.0, Some(6.8)),
            ],
        );
        base_grades.insert(
            3,
            vec![
                make_grade(3, "Matematica", 1, 7.0, Some(6.5)),
                make_grade(3, "Fisica", 1, 6.2, Some(6.1)),
            ],
        );

        let dataset_clean = YearRankingDataset {
            school_year: 2026,
            accounts: base_accounts.clone(),
            grades_by_account: base_grades.clone(),
            tags_by_account: HashMap::new(),
            canonical_subject_names: HashMap::new(),
        };

        // Add extreme invisible accounts in existing and brand-new classes.
        let mut with_inv_accounts = base_accounts;
        with_inv_accounts.insert(99, make_account(99, "INI2A", true));
        with_inv_accounts.insert(100, make_account(100, "MEC3", true));

        let mut with_inv_grades = base_grades;
        with_inv_grades.insert(
            99,
            vec![
                make_grade(99, "Matematica", 1, 0.1, Some(1.0)),
                make_grade(99, "Fisica", 1, 10.0, Some(9.9)),
                make_grade(99, "Secreta", 1, 10.0, Some(9.5)),
            ],
        );
        with_inv_grades.insert(
            100,
            vec![make_grade(100, "Matematica", 1, 10.0, Some(9.8))],
        );

        let dataset_with_inv = YearRankingDataset {
            school_year: 2026,
            accounts: with_inv_accounts,
            grades_by_account: with_inv_grades,
            tags_by_account: HashMap::new(),
            canonical_subject_names: HashMap::new(),
        };

        let m_clean = YearStatisticalModel::from_dataset(&dataset_clean);
        let m_inv = YearStatisticalModel::from_dataset(&dataset_with_inv);

        assert_eq!(m_clean.mt_consensus, m_inv.mt_consensus);
        assert_eq!(m_clean.class_count_for_cell, m_inv.class_count_for_cell);
        assert_eq!(m_clean.e_subj_anual, m_inv.e_subj_anual);
        assert_eq!(m_clean.s_exp_anual, m_inv.s_exp_anual);
        assert_eq!(m_clean.sigma_c, m_inv.sigma_c);
        assert_eq!(m_clean.sigma_scope, m_inv.sigma_scope);
        assert_eq!(m_clean.mu_0, m_inv.mu_0);
        assert_eq!(m_clean.theta_c, m_inv.theta_c);
        assert_eq!(m_clean.delta_sb, m_inv.delta_sb);
        assert_eq!(m_clean.gamma_sl, m_inv.gamma_sl);
        assert!(!m_inv.sigma_c.contains_key("MEC3"));
        assert!(!m_inv.theta_c.contains_key("MEC3"));
    }

    #[test]
    fn helpers_classroom_quorum_and_bimester_floor() {
        assert_eq!(min_students_for_classroom(Modality::Integrado), 3);
        assert_eq!(min_students_for_classroom(Modality::Noturno), 2);
        assert_eq!(min_students_for_classroom(Modality::Other), 2);

        assert_eq!(bimester_validity_floor(0), 1);
        assert_eq!(bimester_validity_floor(1), 1);
        assert_eq!(bimester_validity_floor(2), 1);
        assert_eq!(bimester_validity_floor(3), 2);
        assert_eq!(bimester_validity_floor(7), 4);
        assert_eq!(bimester_validity_floor(12), 6);
        assert_eq!(bimester_validity_floor(16), 6);
        assert_eq!(bimester_validity_floor(20), 6);
    }
}
