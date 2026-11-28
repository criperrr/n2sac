use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, PgPool};

use crate::db::ranking_math::{
    ALPHA_THETA, BETA_CLASSROOM, BETA_STUDENT, S0_CLASSROOM, SHRINK_ETA_Z, SHRINK_M, SIGMA_0,
    YearStatisticalModel, bimester_consolidation_threshold, bimester_validity_floor,
    cara_certainty_equivalent, cell_stddev, compute_tied_midpoint_percentile,
    default_modality_subjects, median_f64, min_students_for_classroom, p75_usize, phi_shrink,
    psi_smooth_floor, quantize_k, round_k, round1, round2,
};
use crate::error::ServerError;

const NOTURNO_COMMON_CORE_NOTE: &str = "Turmas do noturno têm grade técnica diferente da BNCC; use 'Destaque vs. Turma' para comparar todos os cursos.";
const INSUFFICIENT_COMMON_SUBJECTS_NOTE: &str = "Poucas matérias em comum entre todas as turmas deste recorte; exibindo todas as matérias com calibração justa.";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CourseFamily {
    Informatica,
    Eletronica,
    Mecanica,
    Other,
}

impl CourseFamily {
    pub const fn label_pt(self) -> &'static str {
        match self {
            Self::Informatica => "Informática (INI + INF)",
            Self::Eletronica => "Eletrônica (ELI + ELE)",
            Self::Mecanica => "Mecânica (MEI + MEC)",
            Self::Other => "Curso",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Modality {
    Integrado,
    Noturno,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassDescriptor {
    pub code: String,
    pub course_family: CourseFamily,
    pub modality: Modality,
    pub grade_level: Option<u8>,
}

/// Extracts a canonical CTI class code token (e.g. `INI3`, `INI1A`, `ELI2`, `INF1`, `MEC2`)
/// from either a clean code (`"INI3"`) or a descriptive title (`"3º Ano - Informática (INI3)"`).
fn extract_cti_class_token(raw: &str) -> Option<String> {
    let upper = raw.trim().to_ascii_uppercase();
    for token in upper.split(|c: char| !c.is_ascii_alphanumeric()) {
        if token.len() < 4 || token.len() > 6 {
            continue;
        }
        let prefix = &token[..3];
        if !matches!(prefix, "INI" | "INF" | "ELI" | "ELE" | "MEI" | "MEC") {
            continue;
        }
        let rest = &token[3..];
        let mut chars = rest.chars();
        if let Some(d) = chars.next()
            && ('1'..='3').contains(&d)
            && chars.all(|c| c.is_ascii_alphabetic())
        {
            return Some(token.to_string());
        }
    }
    None
}

/// Extracts a canonical INI A/B subclass token (`INI1A`, `INI1B`, `INI2A`, `INI2B`, `INI3A`, `INI3B`)
/// from free text. Only Informatics Integrado (`INI1`..`INI3`) has A/B subclasses at CTI.
pub fn extract_ini_ab_token(raw: &str) -> Option<String> {
    let upper = raw.to_ascii_uppercase();
    for token in upper.split(|c: char| !c.is_ascii_alphanumeric()) {
        let bytes = token.as_bytes();
        if bytes.len() == 5
            && &bytes[..3] == b"INI"
            && (b'1'..=b'3').contains(&bytes[3])
            && matches!(bytes[4], b'A' | b'B')
        {
            return Some(token.to_string());
        }
    }
    None
}

/// Scans a raw NSAC ficha-avaliacao PDF (including zlib-compressed FlateDecode streams produced by
/// dompdf) for an `INI[1-3][AB]` subclass token.
pub fn extract_ini_ab_from_pdf(pdf_bytes: &[u8]) -> Option<String> {
    use std::io::Read;

    // 1. Check uncompressed ASCII bytes first.
    if let Some(tok) = extract_ini_ab_token(&String::from_utf8_lossy(pdf_bytes)) {
        return Some(tok);
    }

    // 2. Decompress PDF `stream ... endstream` FlateDecode blocks (bounded to 512 KiB each).
    const MAX_STREAM_OUT: u64 = 512 * 1024;
    const STREAM_MARKER: &[u8] = b"stream";
    const ENDSTREAM_MARKER: &[u8] = b"endstream";

    let mut pos = 0usize;
    while pos < pdf_bytes.len() {
        let Some(rel_start) = pdf_bytes[pos..]
            .windows(STREAM_MARKER.len())
            .position(|w| w == STREAM_MARKER)
        else {
            break;
        };
        let mut s_start = pos + rel_start + STREAM_MARKER.len();
        if pdf_bytes.get(s_start) == Some(&b'\r') {
            s_start += 1;
        }
        if pdf_bytes.get(s_start) == Some(&b'\n') {
            s_start += 1;
        } else {
            pos = s_start;
            continue;
        }
        let Some(rel_end) = pdf_bytes[s_start..]
            .windows(ENDSTREAM_MARKER.len())
            .position(|w| w == ENDSTREAM_MARKER)
        else {
            break;
        };
        let mut s_end = s_start + rel_end;
        while s_end > s_start && matches!(pdf_bytes[s_end - 1], b'\r' | b'\n') {
            s_end -= 1;
        }

        let compressed = &pdf_bytes[s_start..s_end];
        let mut decoder = flate2::read::ZlibDecoder::new(compressed).take(MAX_STREAM_OUT);
        let mut decompressed = Vec::new();
        if decoder.read_to_end(&mut decompressed).is_ok()
            && !decompressed.is_empty()
            && let Some(tok) = extract_ini_ab_token(&String::from_utf8_lossy(&decompressed))
        {
            return Some(tok);
        }

        pos = s_start + rel_end + ENDSTREAM_MARKER.len();
    }

    None
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IniSubLabel {
    A,
    B,
}

impl IniSubLabel {
    const fn opposite(self) -> Self {
        match self {
            Self::A => Self::B,
            Self::B => Self::A,
        }
    }

    fn format_code(self, grade_level: u8) -> String {
        match self {
            Self::A => format!("INI{grade_level}A"),
            Self::B => format!("INI{grade_level}B"),
        }
    }
}

fn parse_ini_grade_and_sub(code: &str) -> Option<(u8, Option<IniSubLabel>)> {
    let bytes = code.as_bytes();
    if bytes.len() < 4 || &bytes[..3] != b"INI" || !(b'1'..=b'3').contains(&bytes[3]) {
        return None;
    }
    let grade = bytes[3] - b'0';
    let sub = match bytes.get(4) {
        None => None,
        Some(b'A') if bytes.len() == 5 => Some(IniSubLabel::A),
        Some(b'B') if bytes.len() == 5 => Some(IniSubLabel::B),
        _ => return None,
    };
    Some((grade, sub))
}

/// Resolves `INI1`..`INI3` accounts into `INIxA` / `INIxB` subclasses using visible anchor students
/// (whose `ficha-avaliacao` or class code explicitly identifies `INIxA`/`INIxB`) and median `M.T.`
/// signature matching (`±0.01` tolerance).
///
/// Privacy invariant: `invisible` accounts NEVER act as anchors and NEVER contribute to `M.T.`
/// signatures; their own `class_code` is resolved privately against the frozen visible signatures.
pub fn resolve_ini_ab_subclasses(
    accounts: &mut HashMap<i64, StudentAccountRecord>,
    grades_by_account: &HashMap<i64, Vec<PostedGradeRecord>>,
    ficha_anchors: &HashMap<i64, String>,
) {
    const MT_TOL: f64 = 0.01;

    // Collect valid M.T. maps per INI account: (normalized_subject, bimester) -> average M.T.
    let mut ini_grade_by_acct: HashMap<i64, u8> = HashMap::new();
    let mut explicit_sub_by_acct: HashMap<i64, IniSubLabel> = HashMap::new();

    for (&acct_id, acc) in accounts.iter() {
        let Some((grade, code_sub)) = parse_ini_grade_and_sub(&acc.class_code) else {
            continue;
        };
        ini_grade_by_acct.insert(acct_id, grade);

        // Anchor from ficha_avaliacao applies ONLY if its grade level matches this year's INI grade.
        let ficha_sub = ficha_anchors
            .get(&acct_id)
            .and_then(|c| parse_ini_grade_and_sub(c))
            .filter(|&(fg, _)| fg == grade)
            .and_then(|(_, sub)| sub);

        if let Some(sub) = ficha_sub.or(code_sub) {
            explicit_sub_by_acct.insert(acct_id, sub);
        }
    }

    if ini_grade_by_acct.is_empty() {
        return;
    }

    // Build per-account M.T. vector only for INI accounts.
    let mut mt_by_acct: HashMap<i64, HashMap<(String, i16), f64>> = HashMap::new();
    for &acct_id in ini_grade_by_acct.keys() {
        if let Some(gs) = grades_by_account.get(&acct_id) {
            let mut accum: HashMap<(String, i16), (f64, usize)> = HashMap::new();
            for g in gs {
                if (1..=4).contains(&g.bimester)
                    && let Some(mt) = g.average_grade
                    && mt.is_finite()
                    && mt > 0.0
                {
                    let slot = accum
                        .entry((g.normalized_subject.clone(), g.bimester))
                        .or_insert((0.0, 0));
                    slot.0 += mt;
                    slot.1 += 1;
                }
            }
            let map: HashMap<(String, i16), f64> = accum
                .into_iter()
                .map(|(k, (s, c))| (k, s / (c as f64)))
                .collect();
            mt_by_acct.insert(acct_id, map);
        }
    }

    // Resolve each grade level 1..=3 independently.
    let mut resolved_codes: HashMap<i64, String> = HashMap::new();

    for grade in 1u8..=3u8 {
        let grade_accts: Vec<i64> = ini_grade_by_acct
            .iter()
            .filter_map(|(&id, &g)| (g == grade).then_some(id))
            .collect();
        if grade_accts.is_empty() {
            continue;
        }

        // Visible anchors ONLY with actual M.T. grades in this school_year (invisible accounts
        // never leak into signatures, and alumni from previous years with 0 grades never create empty anchors).
        let mut anchors_a: Vec<i64> = Vec::new();
        let mut anchors_b: Vec<i64> = Vec::new();
        for &id in &grade_accts {
            if accounts.get(&id).is_some_and(|a| a.invisible) {
                continue;
            }
            if !mt_by_acct.get(&id).is_some_and(|m| !m.is_empty()) {
                continue;
            }
            match explicit_sub_by_acct.get(&id) {
                Some(IniSubLabel::A) => anchors_a.push(id),
                Some(IniSubLabel::B) => anchors_b.push(id),
                None => {}
            }
        }

        if anchors_a.is_empty() && anchors_b.is_empty() {
            // No visible anchors -> keep INIx unified.
            continue;
        }

        let build_sig = |anchor_ids: &[i64]| -> HashMap<(String, i16), f64> {
            let mut per_cell: HashMap<(String, i16), Vec<f64>> = HashMap::new();
            for &aid in anchor_ids {
                if let Some(mt_map) = mt_by_acct.get(&aid) {
                    for (k, &v) in mt_map {
                        per_cell.entry(k.clone()).or_default().push(v);
                    }
                }
            }
            per_cell
                .into_iter()
                .filter_map(|(k, mut vals)| median_f64(&mut vals).map(|m| (k, m)))
                .collect()
        };

        let sig_a = build_sig(&anchors_a);
        let sig_b = build_sig(&anchors_b);
        let has_a = !anchors_a.is_empty();
        let has_b = !anchors_b.is_empty();

        for &id in &grade_accts {
            // 1. Explicit anchor (including visible anchors; invisible explicit keeps its own label privately).
            if let Some(&sub) = explicit_sub_by_acct.get(&id) {
                resolved_codes.insert(id, sub.format_code(grade));
                continue;
            }

            let Some(mt_map) = mt_by_acct.get(&id) else {
                continue;
            };
            if mt_map.is_empty() {
                continue;
            }

            let resolved_sub = match (has_a, has_b) {
                (true, true) => {
                    // Case 3: both A and B anchors exist.
                    let mut disc_total = 0usize;
                    let mut disc_hit_a = 0usize;
                    let mut disc_hit_b = 0usize;
                    for (k, &v) in mt_map {
                        if let (Some(&ma), Some(&mb)) = (sig_a.get(k), sig_b.get(k))
                            && (ma - mb).abs() > MT_TOL
                        {
                            disc_total += 1;
                            if (v - ma).abs() <= MT_TOL {
                                disc_hit_a += 1;
                            }
                            if (v - mb).abs() <= MT_TOL {
                                disc_hit_b += 1;
                            }
                        }
                    }
                    if disc_total > 0 {
                        if disc_hit_a > disc_hit_b && disc_hit_a * 2 > disc_total {
                            Some(IniSubLabel::A)
                        } else if disc_hit_b > disc_hit_a && disc_hit_b * 2 > disc_total {
                            Some(IniSubLabel::B)
                        } else {
                            None
                        }
                    } else {
                        let count_match = |sig: &HashMap<(String, i16), f64>| -> (usize, usize) {
                            let mut total = 0usize;
                            let mut hits = 0usize;
                            for (k, &v) in mt_map {
                                if let Some(&m) = sig.get(k) {
                                    total += 1;
                                    if (v - m).abs() <= MT_TOL {
                                        hits += 1;
                                    }
                                }
                            }
                            (hits, total)
                        };
                        let (ha, ta) = count_match(&sig_a);
                        let (hb, tb) = count_match(&sig_b);
                        let a_maj = ta > 0 && ha * 2 > ta && (tb == 0 || hb * 2 < tb);
                        let b_maj = tb > 0 && hb * 2 > tb && (ta == 0 || ha * 2 < ta);
                        match (a_maj, b_maj) {
                            (true, false) => Some(IniSubLabel::A),
                            (false, true) => Some(IniSubLabel::B),
                            _ => None,
                        }
                    }
                }
                (true, false) | (false, true) => {
                    // Case 4: single anchor label L exists.
                    let (anchor_label, sig) = if has_a {
                        (IniSubLabel::A, &sig_a)
                    } else {
                        (IniSubLabel::B, &sig_b)
                    };
                    let mut total = 0usize;
                    let mut hits = 0usize;
                    for (k, &v) in mt_map {
                        if let Some(&m) = sig.get(k) {
                            total += 1;
                            if (v - m).abs() <= MT_TOL {
                                hits += 1;
                            }
                        }
                    }
                    if total == 0 {
                        None
                    } else if hits * 2 > total {
                        Some(anchor_label)
                    } else {
                        let misses = total - hits;
                        if total >= 2 && misses * 2 > total {
                            Some(anchor_label.opposite())
                        } else {
                            None
                        }
                    }
                }
                (false, false) => None,
            };

            if let Some(sub) = resolved_sub {
                resolved_codes.insert(id, sub.format_code(grade));
            }
        }
    }

    for (id, new_code) in resolved_codes {
        if let Some(acc) = accounts.get_mut(&id) {
            acc.class_desc = ClassDescriptor::parse(&new_code);
            acc.class_code = new_code;
        }
    }
}

impl ClassDescriptor {
    pub fn parse(class_code: &str) -> Self {
        let code = extract_cti_class_token(class_code)
            .unwrap_or_else(|| class_code.trim().to_ascii_uppercase());
        let alpha_prefix: String = code
            .chars()
            .take_while(|c| c.is_ascii_alphabetic())
            .collect();
        let prefix3: String = alpha_prefix.chars().take(3).collect();

        let course_family = match prefix3.as_str() {
            "INI" | "INF" => CourseFamily::Informatica,
            "ELI" | "ELE" => CourseFamily::Eletronica,
            "MEI" | "MEC" => CourseFamily::Mecanica,
            _ => CourseFamily::Other,
        };

        let modality = match prefix3.as_str() {
            "INI" | "ELI" | "MEI" => Modality::Integrado,
            "INF" | "ELE" | "MEC" => Modality::Noturno,
            _ => Modality::Other,
        };

        let grade_level = if alpha_prefix.is_empty() {
            None
        } else {
            code.chars()
                .find(|c| c.is_ascii_digit())
                .and_then(|c| c.to_digit(10))
                .and_then(|d| u8::try_from(d).ok())
                .filter(|&d| (1..=3).contains(&d))
        };

        Self {
            code,
            course_family,
            modality,
            grade_level,
        }
    }

    pub fn prefix3(&self) -> String {
        self.code
            .chars()
            .take_while(|c| c.is_ascii_alphabetic())
            .take(3)
            .collect()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RankingScope {
    Class,
    GradeLevel,
    Course,
    School,
    Friends,
}

impl RankingScope {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "class" => Some(Self::Class),
            "grade_level" | "gradelevel" | "year_level" => Some(Self::GradeLevel),
            "course" => Some(Self::Course),
            "school" => Some(Self::School),
            "friends" => Some(Self::Friends),
            _ => None,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Class => "class",
            Self::GradeLevel => "grade_level",
            Self::Course => "course",
            Self::School => "school",
            Self::Friends => "friends",
        }
    }

    pub const fn is_multi_class(self) -> bool {
        !matches!(self, Self::Class)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RankingSortBy {
    PersonalAverage,
    RawAverage,
    DeltaVsClass,
    Momentum,
    Consistency,
}

impl RankingSortBy {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "personal_average" | "average" | "media" | "score_cti" => Some(Self::PersonalAverage),
            "raw_average" | "raw" | "media_bruta" => Some(Self::RawAverage),
            "delta_vs_class" | "delta" => Some(Self::DeltaVsClass),
            "momentum" | "evolucao" => Some(Self::Momentum),
            "consistency" | "consistencia" => Some(Self::Consistency),
            _ => None,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PersonalAverage => "personal_average",
            Self::RawAverage => "raw_average",
            Self::DeltaVsClass => "delta_vs_class",
            Self::Momentum => "momentum",
            Self::Consistency => "consistency",
        }
    }
}

#[derive(Debug, Clone)]
pub struct RankingQuery {
    pub scope: RankingScope,
    pub sort_by: RankingSortBy,
    pub bimester: Option<i16>,
    pub subject_filter: Option<String>,
    pub common_only: Option<bool>,
    pub compare_with_account_id: Option<i64>,
}

impl Default for RankingQuery {
    fn default() -> Self {
        Self {
            scope: RankingScope::Class,
            sort_by: RankingSortBy::PersonalAverage,
            bimester: None,
            subject_filter: None,
            common_only: None,
            compare_with_account_id: None,
        }
    }
}

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

#[derive(Debug, Clone, Serialize)]
pub struct AccountTagBadge {
    pub slug: String,
    pub label: String,
    pub description: String,
    pub color_hex: String,
    pub priority: i16,
}

#[derive(FromRow)]
struct AccountTagSqlRow {
    account_id: i64,
    slug: String,
    label: String,
    description: String,
    color_hex: String,
    priority: i16,
}

#[derive(FromRow)]
struct DatasetAccountSqlRow {
    account_id: i64,
    nsac_email: String,
    current_class_code: Option<String>,
    year_class_code: Option<String>,
    verified_official_name: Option<String>,
    opted_in: bool,
    hide_name: bool,
    ranking_display_name: Option<String>,
    profile_display_name: Option<String>,
    show_avatar: bool,
    public_profile: bool,
    invisible: bool,
    avatar_version: Option<i64>,
}

#[derive(FromRow)]
struct DatasetGradeSqlRow {
    account_id: i64,
    subject_name: String,
    bimester: i16,
    grade: f64,
    average_grade: Option<f64>,
    absences: i32,
}

#[derive(FromRow)]
struct FichaAnchorSqlRow {
    account_id: i64,
    ficha_class_code: Option<String>,
}

#[derive(Debug, Clone)]
pub struct StudentAccountRecord {
    pub account_id: i64,
    pub nsac_email: String,
    pub class_code: String,
    pub class_desc: ClassDescriptor,
    pub verified_official_name: Option<String>,
    pub opted_in: bool,
    pub hide_name: bool,
    pub ranking_display_name: Option<String>,
    pub profile_display_name: Option<String>,
    pub show_avatar: bool,
    pub public_profile: bool,
    pub invisible: bool,
    pub avatar_version: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct PostedGradeRecord {
    pub account_id: i64,
    pub subject_name: String,
    pub normalized_subject: String,
    pub bimester: i16,
    pub grade: f64,
    pub average_grade: Option<f64>,
    pub absences: i32,
}

#[derive(Debug, Clone)]
pub struct YearRankingDataset {
    pub school_year: i32,
    pub accounts: HashMap<i64, StudentAccountRecord>,
    pub grades_by_account: HashMap<i64, Vec<PostedGradeRecord>>,
    pub tags_by_account: HashMap<i64, Vec<AccountTagBadge>>,
    pub canonical_subject_names: HashMap<String, String>,
}

#[derive(Debug, Clone, Default)]
pub struct RequesterSocialContext {
    pub blocked_ids: HashSet<i64>,
    pub friend_ids: HashSet<i64>,
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
    /// Admin-assigned platform tags and elected representative badge (empty for anonymous entries).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<AccountTagBadge>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_provisional: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub coverage_ratio: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub momentum_delta: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub consistency_stddev: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_tied: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_friend: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub calibrated_score: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub consistency_score: Option<f64>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_common: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub my_class_average: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub peer_class_average: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SubjectSpotlight {
    pub name: String,
    pub grade: f64,
    pub class_average: f64,
    pub delta: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct RankingNerdStats {
    pub school_mu_0: f64,
    pub school_sigma: f64,
    pub class_theta: f64,
    pub class_sigma: f64,
    pub class_mean_mt: f64,
    pub z_score: f64,
    pub coverage_2d_pct: f64,
    pub active_bimesters: usize,
    pub ridge_active: bool,
    pub consistency_cara_beta: f64,
    pub multi_class_calibrated: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct MyBenchmarkSummary {
    pub class_code: String,
    pub personal_average: f64,
    pub class_average: f64,
    pub delta_vs_class: f64,
    pub subjects_count: i64,
    pub posted_bimesters_count: i64,
    pub total_absences: i64,
    pub subjects_above_class: usize,
    pub subjects_below_class: usize,
    pub is_provisional: bool,
    pub coverage_ratio: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub momentum_delta: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub consistency_stddev: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub standout_subject: Option<SubjectSpotlight>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub focus_subject: Option<SubjectSpotlight>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hypothetical_rank: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hypothetical_total: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub calibrated_score: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub leader_target_average: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headroom_pct: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nerd_stats: Option<RankingNerdStats>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CohortHighlightItem {
    pub kind: String,
    pub title: String,
    pub subject_or_label: String,
    pub value_text: String,
    pub subtext: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account_id: Option<i64>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope_label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sort_by: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub common_only: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub common_subjects: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub common_subjects_note: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub qualified_participants: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_subjects_count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub my_benchmark: Option<MyBenchmarkSummary>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub highlights: Vec<CohortHighlightItem>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub available_subjects: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub available_years: Vec<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub my_class_code: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PeerComparisonDetail {
    pub peer_entry: LeaderboardEntry,
    pub my_wins: usize,
    pub peer_wins: usize,
    pub ties: usize,
    pub subjects: Vec<SubjectComparisonRow>,
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

    /// Returns all school years the account has attended (newest first), e.g. `[2026, 2025, 2024]`.
    pub async fn available_school_years_for_account(
        &self,
        account_id: i64,
    ) -> Result<Vec<i32>, ServerError> {
        let years: Vec<i32> = sqlx::query_scalar(
            r#"
            SELECT DISTINCT sy.year
            FROM accounts_school_years asy
            JOIN school_years sy ON sy.id = asy.school_year_id
            WHERE asy.account_id = $1
            ORDER BY sy.year DESC
            "#,
        )
        .bind(account_id)
        .fetch_all(&self.pool)
        .await?;

        Ok(years)
    }

    pub async fn latest_school_year_for_account(&self, account_id: i64) -> Result<i32, ServerError> {
        let years = self.available_school_years_for_account(account_id).await?;
        Ok(years
            .into_iter()
            .next()
            .unwrap_or_else(|| Utc::now().format("%Y").to_string().parse().unwrap_or(2026)))
    }

    /// Loads the complete cacheable ranking dataset for `school_year` using 3 flat parallel queries
    /// (zero TOAST blobs fetched, historical class codes resolved from `school_years.title`).
    pub async fn load_year_dataset(&self, school_year: i32) -> Result<YearRankingDataset, ServerError> {
        let accounts_fut = sqlx::query_as::<_, DatasetAccountSqlRow>(
            r#"
            SELECT
                a.id AS account_id,
                a.nsac_email,
                a.class_code AS current_class_code,
                hist.title AS year_class_code,
                a.verified_official_name,
                COALESCE(rp.opted_in, FALSE) AS opted_in,
                COALESCE(rp.hide_name, FALSE) AS hide_name,
                rp.display_name AS ranking_display_name,
                ap.display_name AS profile_display_name,
                COALESCE(rp.show_avatar, FALSE) AS show_avatar,
                COALESCE(rp.public_profile, FALSE) AS public_profile,
                COALESCE(rp.invisible, FALSE) AS invisible,
                CASE WHEN ap.avatar_data_url IS NOT NULL
                     THEN EXTRACT(EPOCH FROM ap.updated_at)::bigint END AS avatar_version
            FROM accounts a
            LEFT JOIN account_ranking_preferences rp ON rp.account_id = a.id
            LEFT JOIN account_profiles ap ON ap.account_id = a.id
            LEFT JOIN LATERAL (
                SELECT sy.title
                FROM accounts_school_years asy
                JOIN school_years sy ON sy.id = asy.school_year_id
                WHERE asy.account_id = a.id AND sy.year = $1
                ORDER BY
                    (SELECT COUNT(*)
                     FROM grades g
                     JOIN subjects s ON s.id = g.subject_id
                     WHERE g.account_id = a.id
                       AND s.school_year_id = sy.id
                       AND g.grade IS NOT NULL) DESC,
                    (UPPER(TRIM(sy.title)) = UPPER(TRIM(COALESCE(a.class_code, '')))) DESC,
                    asy.id DESC
                LIMIT 1
            ) hist ON TRUE
            WHERE a.active = TRUE
            "#,
        )
        .bind(school_year)
        .fetch_all(&self.pool);

        let grades_fut = sqlx::query_as::<_, DatasetGradeSqlRow>(
            r#"
            SELECT
                g.account_id,
                s.name AS subject_name,
                g.bimester,
                g.grade::float8 AS grade,
                g.average_grade::float8 AS average_grade,
                g.absences
            FROM grades g
            JOIN subjects s ON s.id = g.subject_id
            JOIN school_years sy ON sy.id = s.school_year_id
            WHERE sy.year = $1
              AND g.grade IS NOT NULL
              AND NOT (
                  g.grade = 0
                  AND COALESCE(g.average_grade, 0) = 0
                  AND COALESCE(NULLIF(TRIM(g.recovery_code), ''), 'NAC') = 'NAC'
              )
            "#,
        )
        .bind(school_year)
        .fetch_all(&self.pool);

        let tags_fut = sqlx::query_as::<_, AccountTagSqlRow>(
            r#"
            SELECT
                at.account_id,
                td.slug,
                td.label,
                td.description,
                td.color_hex,
                td.priority
            FROM account_tags at
            JOIN tag_definitions td ON td.slug = at.tag_slug
            UNION ALL
            SELECT
                cr.account_id,
                'representative'::text AS slug,
                'Representante'::text AS label,
                'Representante eleito pela turma.'::text AS description,
                '#F59E0B'::text AS color_hex,
                20::smallint AS priority
            FROM class_representatives cr
            JOIN class_rooms r ON r.id = cr.room_id
            WHERE r.school_year = $1
            ORDER BY priority ASC, label ASC
            "#,
        )
        .bind(school_year)
        .fetch_all(&self.pool);

        let ficha_anchors_fut = sqlx::query_as::<_, FichaAnchorSqlRow>(
            r#"
            SELECT DISTINCT ON (s.account_id)
                s.account_id,
                UPPER((regexp_match(s.payload_json::text, 'INI[1-3][AB]', 'i'))[1]) AS ficha_class_code
            FROM auxiliary_snapshots s
            WHERE (s.route_key LIKE 'ficha_detail_%' OR s.route_key = 'ficha_class_hint')
              AND s.payload_json::text ~* 'INI[1-3][AB]'
            ORDER BY s.account_id, s.updated_at DESC
            "#,
        )
        .fetch_all(&self.pool);

        let (account_rows, grade_rows, tag_rows, ficha_anchor_rows) =
            tokio::try_join!(accounts_fut, grades_fut, tags_fut, ficha_anchors_fut)?;

        let mut accounts = HashMap::with_capacity(account_rows.len());
        for row in account_rows {
            let class_code = row
                .year_class_code
                .as_deref()
                .and_then(extract_cti_class_token)
                .or_else(|| {
                    row.current_class_code
                        .as_deref()
                        .and_then(extract_cti_class_token)
                })
                .or_else(|| {
                    row.year_class_code
                        .as_deref()
                        .or(row.current_class_code.as_deref())
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_ascii_uppercase)
                })
                .unwrap_or_else(|| "CTI".to_string());
            let class_desc = ClassDescriptor::parse(&class_code);
            accounts.insert(
                row.account_id,
                StudentAccountRecord {
                    account_id: row.account_id,
                    nsac_email: row.nsac_email,
                    class_code,
                    class_desc,
                    verified_official_name: row.verified_official_name,
                    opted_in: row.opted_in,
                    hide_name: row.hide_name,
                    ranking_display_name: row.ranking_display_name,
                    profile_display_name: row.profile_display_name,
                    show_avatar: row.show_avatar,
                    public_profile: row.public_profile,
                    invisible: row.invisible,
                    avatar_version: row.avatar_version,
                },
            );
        }

        let mut grades_by_account: HashMap<i64, Vec<PostedGradeRecord>> = HashMap::new();
        let mut canonical_subject_names: HashMap<String, String> = HashMap::new();
        for g in grade_rows {
            let clean_name = g.subject_name.split_whitespace().collect::<Vec<_>>().join(" ");
            let normalized = normalize_subject_key(&clean_name);
            if normalized.is_empty() {
                continue;
            }
            canonical_subject_names
                .entry(normalized.clone())
                .or_insert_with(|| clean_name.clone());
            grades_by_account
                .entry(g.account_id)
                .or_default()
                .push(PostedGradeRecord {
                    account_id: g.account_id,
                    subject_name: clean_name,
                    normalized_subject: normalized,
                    bimester: g.bimester,
                    grade: g.grade,
                    average_grade: g.average_grade,
                    absences: g.absences,
                });
        }

        let mut tags_by_account: HashMap<i64, Vec<AccountTagBadge>> = HashMap::new();
        for tr in tag_rows {
            let entry_tags = tags_by_account.entry(tr.account_id).or_default();
            if !entry_tags.iter().any(|t| t.slug == tr.slug) {
                entry_tags.push(AccountTagBadge {
                    slug: tr.slug,
                    label: tr.label,
                    description: tr.description,
                    color_hex: tr.color_hex,
                    priority: tr.priority,
                });
            }
        }

        let mut ficha_anchors: HashMap<i64, String> = HashMap::new();
        for row in ficha_anchor_rows {
            if let Some(code) = row.ficha_class_code.as_deref().and_then(extract_ini_ab_token) {
                ficha_anchors.insert(row.account_id, code);
            }
        }
        resolve_ini_ab_subclasses(&mut accounts, &grades_by_account, &ficha_anchors);

        Ok(YearRankingDataset {
            school_year,
            accounts,
            grades_by_account,
            tags_by_account,
            canonical_subject_names,
        })
    }

    /// Loads per-requester social edges (both-direction blocks + mutual accepted friends).
    pub async fn load_requester_social_context(
        &self,
        requester_account_id: i64,
    ) -> Result<RequesterSocialContext, ServerError> {
        let blocks_fut = sqlx::query_scalar::<_, i64>(
            r#"
            SELECT CASE WHEN blocker_id = $1 THEN blocked_id ELSE blocker_id END
            FROM social_blocks
            WHERE blocker_id = $1 OR blocked_id = $1
            "#,
        )
        .bind(requester_account_id)
        .fetch_all(&self.pool);

        let friends_fut = sqlx::query_scalar::<_, i64>(
            r#"
            SELECT f1.followee_id
            FROM social_follows f1
            JOIN social_follows f2
              ON f2.follower_id = f1.followee_id
             AND f2.followee_id = f1.follower_id
            WHERE f1.follower_id = $1
              AND f1.status = 'accepted'
              AND f2.status = 'accepted'
            "#,
        )
        .bind(requester_account_id)
        .fetch_all(&self.pool);

        let (blocked, friends) = tokio::try_join!(blocks_fut, friends_fut)?;
        Ok(RequesterSocialContext {
            blocked_ids: blocked.into_iter().collect(),
            friend_ids: friends.into_iter().collect(),
        })
    }

    /// Convenience uncached wrapper (used by DB integration tests & direct callers).
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
        let (my_prefs, dataset, social) = tokio::try_join!(
            self.get_preferences(requester_account_id),
            self.load_year_dataset(school_year),
            self.load_requester_social_context(requester_account_id),
        )?;

        let query = RankingQuery {
            scope: RankingScope::parse(scope).unwrap_or(RankingScope::Class),
            sort_by: RankingSortBy::PersonalAverage,
            bimester,
            subject_filter: subject_contains
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(ToString::to_string),
            common_only: None,
            compare_with_account_id,
        };

        Ok(build_comparison_summary(
            &dataset,
            requester_account_id,
            requester_class_code,
            my_prefs,
            &social,
            &query,
        ))
    }
}

pub(crate) fn normalize_subject_key(name: &str) -> String {
    let collapsed = name.split_whitespace().collect::<Vec<_>>().join(" ");
    nsac_core::filters::normalize_text(&collapsed)
}

const fn ceil_40pct(n: usize) -> usize {
    (2 * n).div_ceil(5)
}

const fn ceil_75pct(n: usize) -> usize {
    (3 * n).div_ceil(4)
}

fn is_valid_class_code(code: &str) -> bool {
    !code.is_empty() && code != "CTI"
}

fn matches_subject_filter(norm_subj: &str, filter: Option<&str>) -> bool {
    filter.is_none_or(|f| norm_subj == f)
}

fn matches_common_set(norm_subj: &str, common_set: Option<&HashSet<String>>) -> bool {
    common_set.is_none_or(|cs| cs.contains(norm_subj))
}

fn resolve_student_display_label(acc: &StudentAccountRecord, is_me: bool) -> String {
    if acc.hide_name {
        let tag = format!("Aluno #{:03}", (acc.account_id * 37 + 13) % 997);
        return if is_me {
            format!("{tag} (Você • Oculto)")
        } else {
            tag
        };
    }

    for candidate in [
        acc.ranking_display_name.as_deref(),
        acc.profile_display_name.as_deref(),
        acc.verified_official_name.as_deref(),
    ] {
        if let Some(clean) = candidate.map(str::trim).filter(|s| !s.is_empty()) {
            return clean.to_string();
        }
    }

    let derived = derive_official_name_from_email(&acc.nsac_email);
    if derived.is_empty() {
        format!("Aluno #{}", acc.account_id)
    } else {
        derived
    }
}

/// Deduplicated cell observation `(grade, absences, subject_name)` for a student (Section 1.3).
#[derive(Debug, Clone)]
struct StudentCell {
    grade: f64,
    absences: i64,
    subject_name: String,
}

fn deduplicate_student_cells(grades: &[PostedGradeRecord]) -> BTreeMap<(String, i16), StudentCell> {
    let mut cells: BTreeMap<(String, i16), StudentCell> = BTreeMap::new();
    for g in grades {
        if !(1..=4).contains(&g.bimester) || !g.grade.is_finite() {
            continue;
        }
        let key = (g.normalized_subject.clone(), g.bimester);
        cells
            .entry(key)
            .and_modify(|existing| {
                existing.grade = g.grade;
                existing.absences = i64::from(g.absences);
                existing.subject_name.clone_from(&g.subject_name);
            })
            .or_insert_with(|| StudentCell {
                grade: g.grade,
                absences: i64::from(g.absences),
                subject_name: g.subject_name.clone(),
            });
    }
    cells
}

/// Collects deduplicated cell maps exclusively for visible accounts (`invisible == false`, Section 1.1).
fn collect_visible_cells(
    dataset: &YearRankingDataset,
) -> HashMap<i64, BTreeMap<(String, i16), StudentCell>> {
    let mut out = HashMap::with_capacity(dataset.accounts.len());
    for (&acct_id, acc) in &dataset.accounts {
        if acc.invisible {
            continue;
        }
        if let Some(grades) = dataset.grades_by_account.get(&acct_id) {
            let cells = deduplicate_student_cells(grades);
            if !cells.is_empty() {
                out.insert(acct_id, cells);
            }
        }
    }
    out
}

/// Base curricular expectation before `common_set` / `subject_filter` specialization (Section 3.1).
struct BaseCurricularExpectation {
    bimester: Option<i16>,
    s_exp_base: HashMap<String, HashSet<String>>,
    e_subj_base: HashMap<String, usize>,
    vis_data_accts_by_class: HashMap<String, Vec<i64>>,
    vis_data_accts_by_stratum: HashMap<(Modality, Option<u8>), Vec<i64>>,
    vis_data_accts_by_modality: HashMap<Modality, Vec<i64>>,
    all_vis_data_accts: Vec<i64>,
    large_annual_modalities: HashSet<Modality>,
    union_anual_count_by_class: HashMap<String, usize>,
    class_descs: HashMap<String, ClassDescriptor>,
}

impl BaseCurricularExpectation {
    fn build(
        dataset: &YearRankingDataset,
        model: &YearStatisticalModel,
        vis_cells: &HashMap<i64, BTreeMap<(String, i16), StudentCell>>,
        bimester: Option<i16>,
    ) -> Self {
        let mut class_descs: HashMap<String, ClassDescriptor> = HashMap::new();
        let mut vis_ano_accts_by_class: HashMap<String, Vec<i64>> = HashMap::new();
        let mut vis_data_accts_by_class: HashMap<String, Vec<i64>> = HashMap::new();
        let mut vis_data_accts_by_stratum: HashMap<(Modality, Option<u8>), Vec<i64>> =
            HashMap::new();
        let mut vis_data_accts_by_modality: HashMap<Modality, Vec<i64>> = HashMap::new();
        let mut all_vis_data_accts: Vec<i64> = Vec::new();
        let mut s_ref_count_by_acct: HashMap<i64, usize> = HashMap::new();

        for (&acct_id, acc) in &dataset.accounts {
            if acc.invisible || !is_valid_class_code(&acc.class_code) {
                continue;
            }
            let Some(cells) = vis_cells.get(&acct_id) else {
                continue;
            };
            if cells.is_empty() {
                continue;
            }
            class_descs
                .entry(acc.class_code.clone())
                .or_insert_with(|| acc.class_desc.clone());
            vis_ano_accts_by_class
                .entry(acc.class_code.clone())
                .or_default()
                .push(acct_id);

            let ref_subjs: HashSet<&str> = cells
                .keys()
                .filter(|(_, b)| bimester.is_none_or(|b0| *b == b0))
                .map(|(s, _)| s.as_str())
                .collect();
            if !ref_subjs.is_empty() {
                s_ref_count_by_acct.insert(acct_id, ref_subjs.len());
                vis_data_accts_by_class
                    .entry(acc.class_code.clone())
                    .or_default()
                    .push(acct_id);
                vis_data_accts_by_stratum
                    .entry((acc.class_desc.modality, acc.class_desc.grade_level))
                    .or_default()
                    .push(acct_id);
                vis_data_accts_by_modality
                    .entry(acc.class_desc.modality)
                    .or_default()
                    .push(acct_id);
                all_vis_data_accts.push(acct_id);
            }
        }

        let mut large_annual_modalities: HashSet<Modality> = HashSet::new();
        let mut union_anual_count_by_class: HashMap<String, usize> = HashMap::new();
        for (class_code, acct_ids) in &vis_ano_accts_by_class {
            if acct_ids.len() >= 5
                && let Some(desc) = class_descs.get(class_code)
            {
                large_annual_modalities.insert(desc.modality);
            }
            let mut union_set: HashSet<&str> = HashSet::new();
            for &id in acct_ids {
                if let Some(cells) = vis_cells.get(&id) {
                    for (s, _) in cells.keys() {
                        union_set.insert(s.as_str());
                    }
                }
            }
            union_anual_count_by_class.insert(class_code.clone(), union_set.len());
        }

        let mut s_exp_base: HashMap<String, HashSet<String>> = HashMap::new();
        let mut e_subj_base: HashMap<String, usize> = HashMap::new();

        for (class_code, desc) in &class_descs {
            let e_anual = model
                .e_subj_anual
                .get(class_code)
                .copied()
                .unwrap_or_else(|| default_modality_subjects(desc.modality))
                .max(1);

            if bimester.is_none() {
                let s_anual = model
                    .s_exp_anual
                    .get(class_code)
                    .cloned()
                    .unwrap_or_default();
                s_exp_base.insert(class_code.clone(), s_anual);
                e_subj_base.insert(class_code.clone(), e_anual);
            } else {
                let e_min_bim = 6usize.min(e_anual);
                let data_accts = vis_data_accts_by_class
                    .get(class_code)
                    .map(Vec::as_slice)
                    .unwrap_or(&[]);

                let mut subj_counts: HashMap<String, usize> = HashMap::new();
                let mut union_subjs: HashSet<String> = HashSet::new();
                for &acct_id in data_accts {
                    if let Some(cells) = vis_cells.get(&acct_id) {
                        let mut student_subjs: HashSet<&str> = HashSet::new();
                        for (s, b) in cells.keys() {
                            if bimester.is_none_or(|b0| *b == b0) {
                                student_subjs.insert(s.as_str());
                            }
                        }
                        for s in student_subjs {
                            *subj_counts.entry(s.to_string()).or_insert(0) += 1;
                            union_subjs.insert(s.to_string());
                        }
                    }
                }

                if data_accts.len() >= 5 {
                    let quorum = data_accts.len().div_ceil(2).max(2);
                    let quorum_set: HashSet<String> = subj_counts
                        .into_iter()
                        .filter_map(|(s, c)| (c >= quorum).then_some(s))
                        .collect();
                    let exp_set = if quorum_set.is_empty() {
                        union_subjs
                    } else {
                        quorum_set
                    };
                    let e_val = exp_set.len().max(e_min_bim).max(1);
                    s_exp_base.insert(class_code.clone(), exp_set);
                    e_subj_base.insert(class_code.clone(), e_val);
                } else {
                    let ref_pool = if let Some(lvl) = desc.grade_level
                        && let Some(pool) = vis_data_accts_by_stratum.get(&(desc.modality, Some(lvl)))
                        && pool.len() >= 5
                    {
                        pool.as_slice()
                    } else {
                        vis_data_accts_by_modality
                            .get(&desc.modality)
                            .map(Vec::as_slice)
                            .unwrap_or(&[])
                    };
                    let counts: Vec<usize> = ref_pool
                        .iter()
                        .filter_map(|id| s_ref_count_by_acct.get(id).copied())
                        .collect();
                    let p75_capped = p75_usize(&counts).min(e_anual);
                    let e_val = union_subjs.len().max(e_min_bim).max(p75_capped).max(1);
                    s_exp_base.insert(class_code.clone(), union_subjs);
                    e_subj_base.insert(class_code.clone(), e_val);
                }
            }
        }

        Self {
            bimester,
            s_exp_base,
            e_subj_base,
            vis_data_accts_by_class,
            vis_data_accts_by_stratum,
            vis_data_accts_by_modality,
            all_vis_data_accts,
            large_annual_modalities,
            union_anual_count_by_class,
            class_descs,
        }
    }
}

#[derive(Debug, Clone)]
struct ClassCurricularInfo {
    s_exp: HashSet<String>,
    e_subj_eff: usize,
    b_exp: BTreeSet<i16>,
    tau_b_eff: usize,
    e_cel: usize,
    use_strict_set: bool,
}

impl ClassCurricularInfo {
    fn tau_cov(&self) -> f64 {
        if self.e_cel >= 4 { 0.75 } else { 1.0 }
    }

    fn tau_cov_mom(&self) -> f64 {
        if self.e_subj_eff >= 4 { 0.75 } else { 1.0 }
    }
}

#[derive(Debug, Clone)]
struct StratumPoolInfo {
    pool: Vec<i64>,
    quorum: usize,
    b_ref: BTreeSet<i16>,
    is_bootstrap: bool,
    e_subj_boot_fallback: usize,
}

/// Finalized Section 3 curricular context for a given `(bimester, subject_filter, common_set)`.
struct CurricularContext {
    bimester: Option<i16>,
    subject_filter_norm: Option<String>,
    common_set: Option<HashSet<String>>,
    per_class: HashMap<String, ClassCurricularInfo>,
    strata: HashMap<(Modality, Option<u8>), StratumPoolInfo>,
    global_b_obs: BTreeSet<i16>,
}

impl CurricularContext {
    fn finalize(
        base: &BaseCurricularExpectation,
        dataset: &YearRankingDataset,
        vis_cells: &HashMap<i64, BTreeMap<(String, i16), StudentCell>>,
        subject_filter_norm: Option<&str>,
        common_set: Option<&HashSet<String>>,
    ) -> Self {
        let bimester = base.bimester;
        let has_common = common_set.is_some();
        let has_subj_filter = subject_filter_norm.is_some();

        // 1. Specialize S_exp(c) and E_subj(c) (Section 3.1.2).
        let mut s_exp_map: HashMap<String, HashSet<String>> = HashMap::new();
        let mut e_subj_map: HashMap<String, usize> = HashMap::new();
        let mut use_strict_map: HashMap<String, bool> = HashMap::new();

        for class_code in base.class_descs.keys() {
            let is_large = base
                .vis_data_accts_by_class
                .get(class_code)
                .is_some_and(|v| v.len() >= 5);
            let use_strict = is_large || has_common || has_subj_filter;
            use_strict_map.insert(class_code.clone(), use_strict);

            if let Some(cs) = common_set {
                s_exp_map.insert(class_code.clone(), cs.clone());
                e_subj_map.insert(class_code.clone(), cs.len().max(1));
            } else if let Some(sf) = subject_filter_norm {
                let mut set = HashSet::new();
                set.insert(sf.to_string());
                s_exp_map.insert(class_code.clone(), set);
                e_subj_map.insert(class_code.clone(), 1);
            } else {
                let s_set = base.s_exp_base.get(class_code).cloned().unwrap_or_default();
                let e_val = base.e_subj_base.get(class_code).copied().unwrap_or(1).max(1);
                s_exp_map.insert(class_code.clone(), s_set);
                e_subj_map.insert(class_code.clone(), e_val);
            }
        }

        // 2. Compute n_{i,b}^{reg} for every visible account with active data (Section 3.2).
        let mut n_reg_by_acct: HashMap<i64, [usize; 5]> = HashMap::new();
        let mut b_obs_by_modality: HashMap<Modality, BTreeSet<i16>> = HashMap::new();
        let mut global_b_obs: BTreeSet<i16> = BTreeSet::new();

        for &acct_id in &base.all_vis_data_accts {
            let Some(acc) = dataset.accounts.get(&acct_id) else {
                continue;
            };
            let Some(cells) = vis_cells.get(&acct_id) else {
                continue;
            };
            let use_strict = use_strict_map
                .get(&acc.class_code)
                .copied()
                .unwrap_or(false);
            let s_exp = s_exp_map.get(&acc.class_code);
            let e_subj = e_subj_map.get(&acc.class_code).copied().unwrap_or(1);

            let mut counts = [0usize; 5];
            for (norm_subj, bim) in cells.keys() {
                if !(1..=4).contains(bim) {
                    continue;
                }
                if !matches_subject_filter(norm_subj, subject_filter_norm)
                    || !matches_common_set(norm_subj, common_set)
                {
                    continue;
                }
                if use_strict && !s_exp.is_some_and(|s| s.contains(norm_subj)) {
                    continue;
                }
                counts[*bim as usize] += 1;
            }
            if !use_strict {
                for cnt in counts.iter_mut().skip(1) {
                    if *cnt > e_subj {
                        *cnt = e_subj;
                    }
                }
            }
            for b in 1i16..=4i16 {
                if counts[b as usize] >= 1 {
                    b_obs_by_modality
                        .entry(acc.class_desc.modality)
                        .or_default()
                        .insert(b);
                    global_b_obs.insert(b);
                }
            }
            n_reg_by_acct.insert(acct_id, counts);
        }

        // 3. Determine init_peq(m) per modality (Section 3.2).
        let mut init_peq_map: HashMap<Modality, bool> = HashMap::new();
        for modality in [Modality::Integrado, Modality::Noturno, Modality::Other] {
            let has_mod_data = base
                .vis_data_accts_by_modality
                .get(&modality)
                .is_some_and(|v| !v.is_empty());
            let no_large_annual = !base.large_annual_modalities.contains(&modality);
            let mod_single_bim = b_obs_by_modality
                .get(&modality)
                .is_some_and(|s| s.len() == 1);
            let d_m = default_modality_subjects(modality);
            let all_classes_below_dm = base.class_descs.iter().all(|(code, desc)| {
                if desc.modality != modality {
                    return true;
                }
                let has_data = base
                    .vis_data_accts_by_class
                    .get(code)
                    .is_some_and(|v| !v.is_empty());
                if !has_data {
                    return true;
                }
                base.union_anual_count_by_class
                    .get(code)
                    .copied()
                    .unwrap_or(0)
                    < d_m
            });
            let is_init_peq = bimester.is_none()
                && !has_common
                && global_b_obs.len() == 1
                && has_mod_data
                && no_large_annual
                && mod_single_bim
                && all_classes_below_dm;
            init_peq_map.insert(modality, is_init_peq);
        }

        // 4. Collect all strata present + resolve B_ref^{strict}(m, l) & bootstrap(m, l).
        let mut all_strata: HashSet<(Modality, Option<u8>)> = HashSet::new();
        for desc in base.class_descs.values() {
            all_strata.insert((desc.modality, desc.grade_level));
        }

        let mut strata: HashMap<(Modality, Option<u8>), StratumPoolInfo> = HashMap::new();
        for &(modality, grade_level) in &all_strata {
            let is_init_peq = init_peq_map.get(&modality).copied().unwrap_or(false);
            let pool: Vec<i64> = if is_init_peq {
                base.vis_data_accts_by_modality
                    .get(&modality)
                    .cloned()
                    .unwrap_or_default()
            } else if grade_level.is_some()
                && let Some(p) = base.vis_data_accts_by_stratum.get(&(modality, grade_level))
                && p.len() >= 5
            {
                p.clone()
            } else if let Some(p) = base.vis_data_accts_by_modality.get(&modality)
                && p.len() >= 5
            {
                p.clone()
            } else {
                base.all_vis_data_accts.clone()
            };

            let quorum = if is_init_peq {
                ceil_40pct(pool.len()).max(1)
            } else {
                ceil_40pct(pool.len()).max(2)
            };

            let mut b_ref_strict: BTreeSet<i16> = BTreeSet::new();
            let mut b_obs_pool: BTreeSet<i16> = BTreeSet::new();
            for b in 1i16..=4i16 {
                let mut strict_hits = 0usize;
                for &acct_id in &pool {
                    let Some(acc) = dataset.accounts.get(&acct_id) else {
                        continue;
                    };
                    let n_reg = n_reg_by_acct
                        .get(&acct_id)
                        .map(|arr| arr[b as usize])
                        .unwrap_or(0);
                    if n_reg >= 1 {
                        b_obs_pool.insert(b);
                    }
                    let e_subj = e_subj_map.get(&acc.class_code).copied().unwrap_or(1);
                    let thresh = if is_init_peq {
                        ceil_75pct(e_subj).clamp(1, 6)
                    } else {
                        bimester_consolidation_threshold(e_subj)
                    };
                    if n_reg >= thresh {
                        strict_hits += 1;
                    }
                }
                if strict_hits >= quorum {
                    b_ref_strict.insert(b);
                }
            }

            // Bootstrap ONLY when b_0 == None && S_com == None && B_ref^{strict} is empty.
            let is_bootstrap = bimester.is_none() && !has_common && b_ref_strict.is_empty();
            let b_ref = if !is_bootstrap {
                if !b_ref_strict.is_empty() {
                    b_ref_strict
                } else {
                    b_obs_pool.clone()
                }
            } else {
                let boot_q = ceil_40pct(pool.len()).max(1);
                let mut b_boot: BTreeSet<i16> = BTreeSet::new();
                for b in 1i16..=4i16 {
                    let hits = pool
                        .iter()
                        .filter(|id| {
                            n_reg_by_acct
                                .get(id)
                                .is_some_and(|arr| arr[b as usize] >= 1)
                        })
                        .count();
                    if hits >= boot_q {
                        b_boot.insert(b);
                    }
                }
                if b_boot.is_empty() {
                    b_obs_pool.clone()
                } else {
                    b_boot
                }
            };

            let e_subj_boot_fallback = if is_bootstrap {
                let vals: Vec<usize> = pool
                    .iter()
                    .flat_map(|id| {
                        let arr = n_reg_by_acct.get(id).copied().unwrap_or([0; 5]);
                        b_ref
                            .iter()
                            .filter_map(move |&b| {
                                let v = arr[b as usize];
                                (v >= 1).then_some(v)
                            })
                    })
                    .collect();
                p75_usize(&vals).max(1)
            } else {
                default_modality_subjects(modality)
            };

            strata.insert(
                (modality, grade_level),
                StratumPoolInfo {
                    pool,
                    quorum,
                    b_ref,
                    is_bootstrap,
                    e_subj_boot_fallback,
                },
            );
        }

        // 5. Build ClassCurricularInfo per visible class.
        let mut per_class: HashMap<String, ClassCurricularInfo> = HashMap::new();
        for (class_code, desc) in &base.class_descs {
            let s_exp = s_exp_map.get(class_code).cloned().unwrap_or_default();
            let e_subj = e_subj_map.get(class_code).copied().unwrap_or(1).max(1);
            let use_strict_set = use_strict_map.get(class_code).copied().unwrap_or(false);
            let stratum_info = strata.get(&(desc.modality, desc.grade_level));
            let is_bootstrap = stratum_info.is_some_and(|s| s.is_bootstrap);
            let b_ref = stratum_info.map(|s| s.b_ref.clone()).unwrap_or_default();

            let data_accts = base
                .vis_data_accts_by_class
                .get(class_code)
                .map(Vec::as_slice)
                .unwrap_or(&[]);

            let (b_exp, e_subj_eff, tau_b_eff) = if !is_bootstrap {
                let b_exp = if let Some(b0) = bimester {
                    let mut set = BTreeSet::new();
                    set.insert(b0);
                    set
                } else if data_accts.len() >= 5 {
                    let own_q = ceil_40pct(data_accts.len()).max(2);
                    let own_thresh = bimester_consolidation_threshold(e_subj);
                    let mut b_own: BTreeSet<i16> = BTreeSet::new();
                    for b in 1i16..=4i16 {
                        let hits = data_accts
                            .iter()
                            .filter(|id| {
                                n_reg_by_acct
                                    .get(id)
                                    .is_some_and(|arr| arr[b as usize] >= own_thresh)
                            })
                            .count();
                        if hits >= own_q {
                            b_own.insert(b);
                        }
                    }
                    if !b_own.is_empty() { b_own } else { b_ref }
                } else {
                    b_ref
                };
                (b_exp, e_subj, bimester_validity_floor(e_subj))
            } else {
                let b_exp = b_ref;
                let boot_pool: &[i64] = if data_accts.len() >= 5 {
                    data_accts
                } else {
                    stratum_info.map(|s| s.pool.as_slice()).unwrap_or(&[])
                };
                let vals: Vec<usize> = boot_pool
                    .iter()
                    .flat_map(|id| {
                        let arr = n_reg_by_acct.get(id).copied().unwrap_or([0; 5]);
                        b_exp.iter().filter_map(move |&b| {
                            let v = arr[b as usize];
                            (v >= 1).then_some(v)
                        })
                    })
                    .collect();
                let e_eff = p75_usize(&vals).max(1);
                (b_exp, e_eff, bimester_validity_floor(e_eff))
            };

            let e_cel = (e_subj_eff * b_exp.len()).max(1);
            per_class.insert(
                class_code.clone(),
                ClassCurricularInfo {
                    s_exp,
                    e_subj_eff,
                    b_exp,
                    tau_b_eff,
                    e_cel,
                    use_strict_set,
                },
            );
        }

        Self {
            bimester,
            subject_filter_norm: subject_filter_norm.map(ToString::to_string),
            common_set: common_set.cloned(),
            per_class,
            strata,
            global_b_obs,
        }
    }

    fn for_class(&self, class_code: &str, desc: &ClassDescriptor) -> ClassCurricularInfo {
        if let Some(info) = self.per_class.get(class_code) {
            return info.clone();
        }
        // Fallback for classes with zero visible accounts (e.g. invisible requester's private class).
        let stratum = self.strata.get(&(desc.modality, desc.grade_level));
        let (s_exp, e_subj_eff, use_strict_set) = if let Some(ref cs) = self.common_set {
            (cs.clone(), cs.len().max(1), true)
        } else if let Some(ref sf) = self.subject_filter_norm {
            let mut set = HashSet::new();
            set.insert(sf.clone());
            (set, 1, true)
        } else if let Some(st) = stratum
            && st.is_bootstrap
        {
            (HashSet::new(), st.e_subj_boot_fallback.max(1), false)
        } else {
            let d_m = default_modality_subjects(desc.modality).max(1);
            let e = if self.bimester.is_some() {
                6usize.min(d_m)
            } else {
                d_m
            };
            (HashSet::new(), e, false)
        };

        let b_exp = if let Some(b0) = self.bimester {
            let mut set = BTreeSet::new();
            set.insert(b0);
            set
        } else if let Some(st) = stratum
            && !st.b_ref.is_empty()
        {
            st.b_ref.clone()
        } else {
            self.global_b_obs.clone()
        };

        let tau_b_eff = bimester_validity_floor(e_subj_eff);
        let e_cel = (e_subj_eff * b_exp.len()).max(1);
        ClassCurricularInfo {
            s_exp,
            e_subj_eff,
            b_exp,
            tau_b_eff,
            e_cel,
            use_strict_set,
        }
    }
}

/// Computes `(cov_2d, is_qualified)` per Section 3.3 over a set of active `(norm_subj, bimester)` cells.
fn compute_2d_coverage<'a>(
    cells: impl Iterator<Item = (&'a str, i16)>,
    info: &ClassCurricularInfo,
) -> (f64, bool) {
    let mut subjs_by_bim: HashMap<i16, HashSet<&'a str>> = HashMap::new();
    for (norm_subj, bim) in cells {
        if !info.b_exp.contains(&bim) {
            continue;
        }
        if info.use_strict_set && !info.s_exp.contains(norm_subj) {
            continue;
        }
        subjs_by_bim.entry(bim).or_default().insert(norm_subj);
    }

    let mut cap_reg = 0usize;
    let mut all_bims_valid = !info.b_exp.is_empty();
    for &b in &info.b_exp {
        let cnt = subjs_by_bim.get(&b).map_or(0, HashSet::len);
        cap_reg += cnt.min(info.e_subj_eff);
        if cnt < info.tau_b_eff {
            all_bims_valid = false;
        }
    }

    let cov_2d = ((cap_reg as f64) / (info.e_cel.max(1) as f64)).clamp(0.0, 1.0);
    let is_qualified = all_bims_valid && cov_2d >= info.tau_cov() - 1e-12;
    (cov_2d, is_qualified)
}

/// Computes the official mean `M_bar_c^{ref}` for class `c` in the active slice (fallback `model.mu_0`, Section 4.3).
fn class_official_mean_in_slice(
    model: &YearStatisticalModel,
    class_code: &str,
    bimester: Option<i16>,
    subject_filter_norm: Option<&str>,
    common_set: Option<&HashSet<String>>,
) -> f64 {
    let mut sum = 0.0_f64;
    let mut cnt = 0usize;
    for ((c, s, b), &mt) in &model.mt_consensus {
        if c != class_code {
            continue;
        }
        if !bimester.is_none_or(|b0| *b == b0) {
            continue;
        }
        if !matches_subject_filter(s, subject_filter_norm) || !matches_common_set(s, common_set) {
            continue;
        }
        sum += mt;
        cnt += 1;
    }
    if cnt > 0 {
        sum / (cnt as f64)
    } else {
        model.mu_0
    }
}

/// Returns the paired subjects `S_i^{pair}(b)` present in both `b - 1` and `b` for a student (Section 6.2).
fn student_paired_subjects_for_bimester<'a>(
    cells: &'a BTreeMap<(String, i16), StudentCell>,
    b: i16,
    subject_filter_norm: Option<&str>,
    common_set: Option<&HashSet<String>>,
) -> Vec<&'a str> {
    if !(2..=4).contains(&b) {
        return Vec::new();
    }
    let prev_b = b - 1;
    let mut prev_set: HashSet<&'a str> = HashSet::new();
    let mut curr_set: HashSet<&'a str> = HashSet::new();
    for (norm_subj, bim) in cells.keys() {
        if !matches_subject_filter(norm_subj, subject_filter_norm)
            || !matches_common_set(norm_subj, common_set)
        {
            continue;
        }
        if *bim == prev_b {
            prev_set.insert(norm_subj.as_str());
        } else if *bim == b {
            curr_set.insert(norm_subj.as_str());
        }
    }
    prev_set.intersection(&curr_set).copied().collect()
}

fn momentum_k_min(bimester: Option<i16>, has_subject_filter: bool) -> usize {
    if has_subject_filter {
        1
    } else if bimester.is_some() {
        2
    } else {
        3
    }
}

/// Computes `cov_i^{mom}(b)` for a student or classroom given paired subject keys (Sections 6.2 & 8.4).
fn compute_momentum_cov(paired_subjects: &[&str], info: &ClassCurricularInfo) -> f64 {
    let matched = if info.use_strict_set {
        paired_subjects
            .iter()
            .filter(|s| info.s_exp.contains(**s))
            .count()
    } else {
        paired_subjects.len().min(info.e_subj_eff)
    };
    ((matched as f64) / (info.e_subj_eff.max(1) as f64)).clamp(0.0, 1.0)
}

/// Resolves the shared terminal bimester `t*` for Momentum aligned to `G_1^{mom}` (Section 6.2).
fn resolve_momentum_terminal_bimester(
    dataset: &YearRankingDataset,
    vis_cells: &HashMap<i64, BTreeMap<(String, i16), StudentCell>>,
    scope_vis_accts: &[i64],
    curricular: &CurricularContext,
    bimester: Option<i16>,
    subject_filter_norm: Option<&str>,
    common_set: Option<&HashSet<String>>,
) -> Option<i16> {
    if let Some(b0) = bimester {
        return (2..=4).contains(&b0).then_some(b0);
    }

    let k_min = momentum_k_min(None, subject_filter_norm.is_some());

    // Filter scope_vis_accts to those with active cells.
    let scope_vis_data: Vec<i64> = scope_vis_accts
        .iter()
        .copied()
        .filter(|id| {
            vis_cells.get(id).is_some_and(|cells| {
                cells.keys().any(|(s, _)| {
                    matches_subject_filter(s, subject_filter_norm)
                        && matches_common_set(s, common_set)
                })
            })
        })
        .collect();

    if scope_vis_data.is_empty() {
        return None;
    }

    let student_qualifies_for_b = |acct_id: i64, b: i16| -> bool {
        let Some(acc) = dataset.accounts.get(&acct_id) else {
            return false;
        };
        let Some(cells) = vis_cells.get(&acct_id) else {
            return false;
        };
        let info = curricular.for_class(&acc.class_code, &acc.class_desc);
        let paired =
            student_paired_subjects_for_bimester(cells, b, subject_filter_norm, common_set);
        if paired.len() < k_min {
            return false;
        }
        let cov = compute_momentum_cov(&paired, &info);
        cov >= info.tau_cov_mom() - 1e-12
    };

    // Group scope_vis_data by stratum (m, l) and modality m.
    let mut stratum_counts: HashMap<(Modality, Option<u8>), usize> = HashMap::new();
    let mut modality_counts: HashMap<Modality, usize> = HashMap::new();
    for &id in &scope_vis_data {
        if let Some(acc) = dataset.accounts.get(&id) {
            *stratum_counts
                .entry((acc.class_desc.modality, acc.class_desc.grade_level))
                .or_insert(0) += 1;
            *modality_counts.entry(acc.class_desc.modality).or_insert(0) += 1;
        }
    }

    let large_strata: Vec<(Modality, Option<u8>)> = stratum_counts
        .iter()
        .filter_map(|(&k, &cnt)| (cnt >= 5).then_some(k))
        .collect();
    let selected_strata: Vec<(Modality, Option<u8>)> = if !large_strata.is_empty() {
        large_strata
    } else {
        let large_mod_strata: Vec<(Modality, Option<u8>)> = stratum_counts
            .keys()
            .copied()
            .filter(|(m, _)| modality_counts.get(m).copied().unwrap_or(0) >= 5)
            .collect();
        if !large_mod_strata.is_empty() {
            large_mod_strata
        } else {
            stratum_counts.keys().copied().collect()
        }
    };

    // Compute B_ref^{mom}(m, l) for each selected stratum and intersect.
    let mut common_bims: Option<BTreeSet<i16>> = None;
    for &(m, l) in &selected_strata {
        let (pool, quorum): (&[i64], usize) = match curricular.strata.get(&(m, l)) {
            Some(st) => (st.pool.as_slice(), st.quorum),
            None => (scope_vis_data.as_slice(), ceil_40pct(scope_vis_data.len()).max(2)),
        };
        let mut b_ref_mom: BTreeSet<i16> = BTreeSet::new();
        for b in 2i16..=4i16 {
            let hits = pool
                .iter()
                .filter(|&&id| student_qualifies_for_b(id, b))
                .count();
            if hits >= quorum {
                b_ref_mom.insert(b);
            }
        }
        common_bims = Some(match common_bims {
            None => b_ref_mom,
            Some(acc) => acc.intersection(&b_ref_mom).copied().collect(),
        });
    }

    if let Some(b_com) = common_bims
        && let Some(&max_b) = b_com.iter().max()
    {
        return Some(max_b);
    }

    // Fallback: united pool A_E^{vis,data} with quorum max(2, ceil(0.40 * |A_E^{vis,data}|)).
    let united_q = ceil_40pct(scope_vis_data.len()).max(2);
    let mut fallback_bims: BTreeSet<i16> = BTreeSet::new();
    for b in 2i16..=4i16 {
        let hits = scope_vis_data
            .iter()
            .filter(|&&id| student_qualifies_for_b(id, b))
            .count();
        if hits >= united_q {
            fallback_bims.insert(b);
        }
    }
    fallback_bims.into_iter().max()
}

/// Complete axiomatic v7 evaluation of a student in an active slice.
#[derive(Debug, Clone)]
struct StudentEval {
    personal_average_raw: f64,
    personal_average_disp: f64,
    s_avg: f64,
    z_bar_rel: f64,
    m_bar_active: f64,
    headroom_pct: f64,
    multi_class_mode: bool,
    active_bimesters_count: usize,
    class_average_disp: f64,
    delta_vs_class_disp: f64,
    tilde_delta: Option<f64>,
    paired_count: usize,
    cov_2d_delta: f64,
    is_qualified_delta: bool,
    momentum_delta_disp: Option<f64>,
    tilde_delta_mom: Option<f64>,
    mom_pair_count: usize,
    cov_mom: f64,
    is_qualified_mom: bool,
    consistency_score: Option<f64>,
    consistency_stddev_disp: Option<f64>,
    consistency_stddev_raw: Option<f64>,
    cov_2d_base: f64,
    is_qualified_base: bool,
    posted_bimesters_count: i64,
    subjects_count: usize,
    total_absences: i64,
    active_subjects: HashSet<String>,
}

#[allow(clippy::too_many_arguments)]
fn evaluate_student_v7(
    cells: &BTreeMap<(String, i16), StudentCell>,
    class_code: &str,
    class_desc: &ClassDescriptor,
    model: &YearStatisticalModel,
    curricular: &CurricularContext,
    bimester: Option<i16>,
    subject_filter_norm: Option<&str>,
    common_set: Option<&HashSet<String>>,
    multi_class_mode: bool,
    t_star: Option<i16>,
) -> Option<StudentEval> {
    // 1. Collect active cells O_i^{ativo}.
    let active: Vec<(&str, i16, &StudentCell)> = cells
        .iter()
        .filter(|((s, b), _)| {
            bimester.is_none_or(|b0| *b == b0)
                && matches_subject_filter(s, subject_filter_norm)
                && matches_common_set(s, common_set)
        })
        .map(|((s, b), cell)| (s.as_str(), *b, cell))
        .collect();

    if active.is_empty() {
        return None;
    }

    let info = curricular.for_class(class_code, class_desc);
    let m_i = active.len();
    let mut active_subjects: HashSet<String> = HashSet::new();
    let mut b_obs: HashSet<i16> = HashSet::new();
    let mut grade_sum = 0.0_f64;
    let mut m_tilde_sum = 0.0_f64;
    let mut z_rel_sum = 0.0_f64;
    let mut total_absences = 0i64;
    let mut v_cells: Vec<f64> = Vec::with_capacity(m_i);

    let mut paired_cells: Vec<(&str, i16, f64, f64)> = Vec::new();
    let mut paired_b_obs: HashSet<i16> = HashSet::new();

    for &(norm_subj, bim, cell) in &active {
        active_subjects.insert(norm_subj.to_string());
        b_obs.insert(bim);
        grade_sum += cell.grade;
        total_absences += cell.absences;

        let m_tilde =
            model.cell_reference_mt(class_code, class_desc.grade_level, norm_subj, bim);
        m_tilde_sum += m_tilde;

        let (z_rel, u_isb) = model.calibrated_cell_score(
            class_code,
            class_desc.grade_level,
            norm_subj,
            bim,
            cell.grade,
        );
        z_rel_sum += z_rel;
        v_cells.push(if multi_class_mode { u_isb } else { cell.grade });

        if let Some(&mt_hat) =
            model
                .mt_consensus
                .get(&(class_code.to_string(), norm_subj.to_string(), bim))
        {
            paired_cells.push((norm_subj, bim, cell.grade, mt_hat));
            paired_b_obs.insert(bim);
        }
    }

    let (cov_2d_base, is_qualified_base) =
        compute_2d_coverage(active.iter().map(|&(s, b, _)| (s, b)), &info);

    let denom_b = 1usize.max(info.b_exp.len()).max(b_obs.len()) as f64;
    let n_eq = ((m_i as f64) / denom_b).min(info.e_subj_eff as f64);

    let mu_raw = grade_sum / (m_i as f64);
    let m_bar_active = m_tilde_sum / (m_i as f64);
    let z_bar_rel = z_rel_sum / (m_i as f64);
    let headroom_cap = (10.0 - m_bar_active).max(0.50);
    let headroom = ((mu_raw - m_bar_active) / headroom_cap).clamp(0.0, 1.0);
    let headroom_pct = round1(headroom * 100.0);

    let theta_c = model.theta_c.get(class_code).copied().unwrap_or(0.0);
    let m_bar_c_ref = if multi_class_mode {
        model.mu_0
    } else {
        class_official_mean_in_slice(model, class_code, bimester, subject_filter_norm, common_set)
    };

    // 2. PersonalAverage score (Section 4.3 + Headroom Ceiling Protection):
    // Above class reference (`mu_raw >= m_bar_active`), use class headroom so a literal 10.00
    // perfection (at full coverage) reaches 10.00 in EVERY class and never loses to a < 10.00
    // student from a lower-M.T. class, while preserving 100% continuity at `mu_raw == m_bar_active`
    // and exact intra-class order.
    let s_avg = if multi_class_mode {
        let base_c = model.mu_0 + ALPHA_THETA * theta_c;
        if mu_raw >= m_bar_active {
            let span_c = (10.0 - base_c).max(0.10);
            let eta_h = (model.sigma_scope * SHRINK_ETA_Z) / span_c;
            let h_tilde = phi_shrink(headroom, n_eq, cov_2d_base, SHRINK_M, eta_h);
            base_c + span_c * h_tilde
        } else {
            let z_tilde = phi_shrink(z_bar_rel, n_eq, cov_2d_base, SHRINK_M, SHRINK_ETA_Z);
            base_c + model.sigma_scope * z_tilde
        }
    } else {
        m_bar_c_ref
            + phi_shrink(
                mu_raw - m_bar_c_ref,
                n_eq,
                cov_2d_base,
                SHRINK_M,
                SHRINK_ETA_Z * SIGMA_0,
            )
    };

    // 3. DeltaVsClass (Section 6.1).
    let paired_count = paired_cells.len();
    let (class_average_disp, delta_vs_class_disp, tilde_delta, cov_2d_delta, is_qualified_delta) =
        if paired_count == 0 {
            (0.0, 0.0, None, 0.0, false)
        } else {
            let p_len = paired_count as f64;
            let mt_mean = paired_cells.iter().map(|c| c.3).sum::<f64>() / p_len;
            let delta_raw = paired_cells.iter().map(|c| c.2 - c.3).sum::<f64>() / p_len;
            let (cov_d, qual_d) =
                compute_2d_coverage(paired_cells.iter().map(|&(s, b, _, _)| (s, b)), &info);
            let denom_bd = 1usize.max(info.b_exp.len()).max(paired_b_obs.len()) as f64;
            let n_delta_eq = (p_len / denom_bd).min(info.e_subj_eff as f64);
            let td = phi_shrink(delta_raw, n_delta_eq, cov_d, SHRINK_M, SHRINK_ETA_Z * SIGMA_0);
            (round2(mt_mean), round2(delta_raw), Some(td), cov_d, qual_d)
        };

    // 4. Momentum (Section 6.2).
    let k_min_mom = momentum_k_min(bimester, subject_filter_norm.is_some());
    let (momentum_delta_disp, tilde_delta_mom, mom_pair_count, cov_mom, is_qualified_mom) =
        if let Some(ts) = t_star {
            let paired_subjs =
                student_paired_subjects_for_bimester(cells, ts, subject_filter_norm, common_set);
            let pair_cnt = paired_subjs.len();
            let cov_m = compute_momentum_cov(&paired_subjs, &info);
            if pair_cnt < k_min_mom {
                (None, None, pair_cnt, cov_m, false)
            } else {
                let mut diff_sum = 0.0_f64;
                let mut valid_pairs = 0usize;
                for &s in &paired_subjs {
                    if let (Some(curr), Some(prev)) = (
                        cells.get(&(s.to_string(), ts)),
                        cells.get(&(s.to_string(), ts - 1)),
                    ) {
                        diff_sum += curr.grade - prev.grade;
                        valid_pairs += 1;
                    }
                }
                if valid_pairs < k_min_mom {
                    (None, None, valid_pairs, cov_m, false)
                } else {
                    let raw_mom = diff_sum / (valid_pairs as f64);
                    let tilde_m = phi_shrink(
                        raw_mom,
                        valid_pairs as f64,
                        cov_m,
                        SHRINK_M,
                        SHRINK_ETA_Z * SIGMA_0,
                    );
                    let qual_m = cov_m >= info.tau_cov_mom() - 1e-12 && valid_pairs >= k_min_mom;
                    (Some(round2(raw_mom)), Some(tilde_m), valid_pairs, cov_m, qual_m)
                }
            }
        } else {
            (None, None, 0, 0.0, false)
        };

    // 5. Consistency via CARA + smooth floor psi (Section 7.2).
    let subjects_count = active_subjects.len();
    let (consistency_score, consistency_stddev_disp, consistency_stddev_raw) =
        if subjects_count >= 4 {
            let sd_raw = cell_stddev(&v_cells);
            let score = cara_certainty_equivalent(&v_cells, BETA_STUDENT).map(|cara| {
                let (mu_ref, eta_cons) = if multi_class_mode {
                    (
                        model.mu_0 + ALPHA_THETA * theta_c,
                        SHRINK_ETA_Z * model.sigma_scope,
                    )
                } else {
                    (m_bar_c_ref, SHRINK_ETA_Z * SIGMA_0)
                };
                let c_tilde = mu_ref + phi_shrink(cara - mu_ref, n_eq, cov_2d_base, SHRINK_M, eta_cons);
                psi_smooth_floor(c_tilde)
            });
            (score, Some(round2(sd_raw)), Some(sd_raw))
        } else {
            (None, None, None)
        };

    Some(StudentEval {
        personal_average_raw: mu_raw,
        personal_average_disp: round2(mu_raw),
        s_avg,
        z_bar_rel,
        m_bar_active,
        headroom_pct,
        multi_class_mode,
        active_bimesters_count: b_obs.len(),
        class_average_disp,
        delta_vs_class_disp,
        tilde_delta,
        paired_count,
        cov_2d_delta,
        is_qualified_delta,
        momentum_delta_disp,
        tilde_delta_mom,
        mom_pair_count,
        cov_mom,
        is_qualified_mom,
        consistency_score,
        consistency_stddev_disp,
        consistency_stddev_raw,
        cov_2d_base,
        is_qualified_base,
        posted_bimesters_count: m_i as i64,
        subjects_count,
        total_absences,
        active_subjects,
    })
}

fn intersect_subject_sets(sets: &[&HashSet<String>]) -> HashSet<String> {
    let Some((first, rest)) = sets.split_first() else {
        return HashSet::new();
    };
    let mut acc = (*first).clone();
    for s in rest {
        acc.retain(|k| s.contains(k));
    }
    acc
}

fn account_in_scope(
    acc: &StudentAccountRecord,
    requester_account_id: i64,
    requester_desc: &ClassDescriptor,
    scope: RankingScope,
    social: &RequesterSocialContext,
) -> bool {
    match scope {
        RankingScope::Class => acc.class_code.eq_ignore_ascii_case(&requester_desc.code),
        RankingScope::GradeLevel => match requester_desc.grade_level {
            Some(lvl) => acc.class_desc.grade_level == Some(lvl),
            None => acc.class_code.eq_ignore_ascii_case(&requester_desc.code),
        },
        RankingScope::Course => {
            if requester_desc.course_family != CourseFamily::Other {
                acc.class_desc.course_family == requester_desc.course_family
            } else {
                let p = requester_desc.prefix3();
                !p.is_empty() && acc.class_desc.prefix3() == p
            }
        }
        RankingScope::School => true,
        RankingScope::Friends => {
            acc.account_id == requester_account_id || social.friend_ids.contains(&acc.account_id)
        }
    }
}

fn scope_label_pt(scope: RankingScope, requester_desc: &ClassDescriptor) -> String {
    match scope {
        RankingScope::Class => {
            if requester_desc.code.is_empty() || requester_desc.code == "CTI" {
                "Minha Turma".to_string()
            } else {
                format!("Turma {}", requester_desc.code)
            }
        }
        RankingScope::GradeLevel => match requester_desc.grade_level {
            Some(lvl) => format!("{lvl}º Ano • Todos os cursos"),
            None => "Mesmo Ano".to_string(),
        },
        RankingScope::Course => requester_desc.course_family.label_pt().to_string(),
        RankingScope::School => "Todo o CTI".to_string(),
        RankingScope::Friends => "Amigos".to_string(),
    }
}

struct CommonCoreResolution {
    effective_common_only: bool,
    common_set: Option<HashSet<String>>,
    common_subjects_names: Option<Vec<String>>,
    note: Option<String>,
}

struct RankingEvalCtx<'a> {
    dataset: &'a YearRankingDataset,
    model: &'a YearStatisticalModel,
    vis_cells: &'a HashMap<i64, BTreeMap<(String, i16), StudentCell>>,
    requester_account_id: i64,
    requester_desc: &'a ClassDescriptor,
    scope: RankingScope,
    sort_by: RankingSortBy,
    bimester: Option<i16>,
    subject_filter_norm: Option<&'a str>,
    social: &'a RequesterSocialContext,
}

/// Resolves `common_set` per Section 9.5 (default `false`, observer-invariant in multi-class scopes,
/// strict anti-expulsion including immature classes and `Friends`).
fn resolve_common_subjects(
    ctx: &RankingEvalCtx<'_>,
    base_curricular: &BaseCurricularExpectation,
    scope_eligible_ids: &[i64],
    common_only_requested: bool,
) -> CommonCoreResolution {
    let inactive = |note: Option<String>| CommonCoreResolution {
        effective_common_only: false,
        common_set: None,
        common_subjects_names: None,
        note,
    };

    if !common_only_requested
        || ctx.subject_filter_norm.is_some()
        || ctx.scope == RankingScope::Class
        || ctx.sort_by == RankingSortBy::DeltaVsClass
    {
        return inactive(None);
    }

    let bimester = ctx.bimester;

    let computed: Option<HashSet<String>> = match ctx.scope {
        RankingScope::Class => None,
        RankingScope::Friends => {
            // Strict intersection over P_friends = ({r} if r has active base cells) ∪ F_obs.
            let mut participant_sets: Vec<HashSet<String>> = Vec::new();
            let mut seen_ids: HashSet<i64> = HashSet::new();

            // Check requester r's own grades (even if opted_out/invisible, private to Friends view).
            if let Some(req_grades) = ctx.dataset.grades_by_account.get(&ctx.requester_account_id) {
                let req_cells = deduplicate_student_cells(req_grades);
                let s: HashSet<String> = req_cells
                    .keys()
                    .filter(|(_, b)| bimester.is_none_or(|b0| *b == b0))
                    .map(|(k, _)| k.clone())
                    .collect();
                if !s.is_empty() {
                    seen_ids.insert(ctx.requester_account_id);
                    participant_sets.push(s);
                }
            }

            for &acct_id in scope_eligible_ids {
                if seen_ids.contains(&acct_id) {
                    continue;
                }
                if let Some(cells) = ctx.vis_cells.get(&acct_id) {
                    let s: HashSet<String> = cells
                        .keys()
                        .filter(|(_, b)| bimester.is_none_or(|b0| *b == b0))
                        .map(|(k, _)| k.clone())
                        .collect();
                    if !s.is_empty() {
                        seen_ids.insert(acct_id);
                        participant_sets.push(s);
                    }
                }
            }

            if participant_sets.len() < 2 {
                return inactive(None);
            }
            let refs: Vec<&HashSet<String>> = participant_sets.iter().collect();
            let inter = intersect_subject_sets(&refs);
            if inter.len() >= 3 {
                Some(inter)
            } else {
                return inactive(Some(INSUFFICIENT_COMMON_SUBJECTS_NOTE.to_string()));
            }
        }
        RankingScope::School | RankingScope::GradeLevel | RankingScope::Course => {
            // Observer-invariant: C_E^{elig} depends ONLY on eligible visible accounts in scope.
            let mut elig_classes: HashSet<String> = HashSet::new();
            for &acct_id in scope_eligible_ids {
                let Some(acc) = ctx.dataset.accounts.get(&acct_id) else {
                    continue;
                };
                if !is_valid_class_code(&acc.class_code) {
                    continue;
                }
                if ctx.vis_cells.get(&acct_id).is_some_and(|cells| {
                    cells.keys().any(|(_, b)| bimester.is_none_or(|b0| *b == b0))
                }) {
                    elig_classes.insert(acc.class_code.clone());
                }
            }

            if elig_classes.len() < 2 {
                return inactive(None);
            }

            // S_c^{obs} = union over visible accounts in class c for active bimester.
            let mut subjects_by_class: HashMap<String, HashSet<String>> = HashMap::new();
            for (&acct_id, acc) in &ctx.dataset.accounts {
                if acc.invisible || !elig_classes.contains(&acc.class_code) {
                    continue;
                }
                if let Some(cells) = ctx.vis_cells.get(&acct_id) {
                    let entry = subjects_by_class.entry(acc.class_code.clone()).or_default();
                    for (s, b) in cells.keys() {
                        if bimester.is_none_or(|b0| *b == b0) {
                            entry.insert(s.clone());
                        }
                    }
                }
            }
            subjects_by_class.retain(|_, set| !set.is_empty());

            // Mature classes C_E^{mad}.
            let mature_classes: Vec<&String> = subjects_by_class
                .iter()
                .filter_map(|(c, s_obs)| {
                    let e_base = base_curricular.e_subj_base.get(c).copied().unwrap_or(1);
                    let thresh = e_base.div_ceil(2).max(2);
                    (s_obs.len() >= thresh).then_some(c)
                })
                .collect();

            let pool_sets: Vec<&HashSet<String>> = if mature_classes.len() >= 2 {
                mature_classes
                    .iter()
                    .filter_map(|c| subjects_by_class.get(*c))
                    .collect()
            } else {
                subjects_by_class.values().collect()
            };

            if pool_sets.len() < 2 {
                return inactive(None);
            }

            let s_all = intersect_subject_sets(&pool_sets);
            let passes_anti_expulsion = s_all.len() >= 3
                && subjects_by_class.values().all(|s_obs| {
                    let shared = s_all.intersection(s_obs).count();
                    shared >= 3usize.min(s_obs.len())
                });

            if passes_anti_expulsion {
                Some(s_all)
            } else {
                let has_noturno = subjects_by_class
                    .keys()
                    .any(|c| ClassDescriptor::parse(c).modality == Modality::Noturno);
                let note = if has_noturno {
                    NOTURNO_COMMON_CORE_NOTE.to_string()
                } else {
                    INSUFFICIENT_COMMON_SUBJECTS_NOTE.to_string()
                };
                return inactive(Some(note));
            }
        }
    };

    let Some(s_com) = computed else {
        return inactive(None);
    };

    let mut names: Vec<String> = s_com
        .iter()
        .map(|k| {
            ctx.dataset
                .canonical_subject_names
                .get(k)
                .cloned()
                .unwrap_or_else(|| k.clone())
        })
        .collect();
    names.sort_by_key(|s| nsac_core::filters::normalize_text(s));

    CommonCoreResolution {
        effective_common_only: true,
        common_set: Some(s_com),
        common_subjects_names: Some(names),
        note: None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum MetricTier {
    G1 = 0,
    G2 = 1,
    G3 = 2,
}

#[derive(Debug, Clone)]
struct SortableEntry {
    entry: LeaderboardEntry,
    tier: MetricTier,
    eval: StudentEval,
}

fn student_tier_and_primary_score(
    eval: &StudentEval,
    sort_by: RankingSortBy,
) -> (MetricTier, Option<f64>, f64) {
    match sort_by {
        RankingSortBy::PersonalAverage => {
            let tier = if eval.is_qualified_base {
                MetricTier::G1
            } else {
                MetricTier::G2
            };
            (tier, Some(eval.s_avg), eval.cov_2d_base)
        }
        RankingSortBy::RawAverage => {
            let tier = if eval.is_qualified_base {
                MetricTier::G1
            } else {
                MetricTier::G2
            };
            (tier, Some(eval.personal_average_raw), eval.cov_2d_base)
        }
        RankingSortBy::DeltaVsClass => match eval.tilde_delta {
            None => (MetricTier::G3, None, eval.cov_2d_delta),
            Some(td) => {
                let tier = if eval.is_qualified_delta {
                    MetricTier::G1
                } else {
                    MetricTier::G2
                };
                (tier, Some(td), eval.cov_2d_delta)
            }
        },
        RankingSortBy::Momentum => match eval.tilde_delta_mom {
            None => (MetricTier::G3, None, eval.cov_mom),
            Some(tm) => {
                let tier = if eval.is_qualified_mom {
                    MetricTier::G1
                } else {
                    MetricTier::G2
                };
                (tier, Some(tm), eval.cov_mom)
            }
        },
        RankingSortBy::Consistency => match eval.consistency_score {
            None => (MetricTier::G3, None, eval.cov_2d_base),
            Some(sc) => {
                let tier = if eval.is_qualified_base {
                    MetricTier::G1
                } else {
                    MetricTier::G2
                };
                (tier, Some(sc), eval.cov_2d_base)
            }
        },
    }
}

fn compare_sortable_entries(a: &SortableEntry, b: &SortableEntry, sort_by: RankingSortBy) -> Ordering {
    // Strict block order: G1 < G2 < G3 (Section 9.2).
    let tier_ord = a.tier.cmp(&b.tier);
    if tier_ord != Ordering::Equal {
        return tier_ord;
    }

    if a.tier == MetricTier::G3 {
        return quantize_k(b.eval.s_avg, 4)
            .cmp(&quantize_k(a.eval.s_avg, 4))
            .then_with(|| a.entry.account_id.cmp(&b.entry.account_id));
    }

    match sort_by {
        RankingSortBy::PersonalAverage => quantize_k(b.eval.s_avg, 4)
            .cmp(&quantize_k(a.eval.s_avg, 4))
            .then_with(|| {
                // None < Some(x) for all x in R
                b.eval
                    .tilde_delta
                    .map(|d| quantize_k(d, 4))
                    .cmp(&a.eval.tilde_delta.map(|d| quantize_k(d, 4)))
            })
            .then_with(|| b.eval.posted_bimesters_count.cmp(&a.eval.posted_bimesters_count))
            .then_with(|| b.eval.subjects_count.cmp(&a.eval.subjects_count))
            .then_with(|| a.eval.total_absences.cmp(&b.eval.total_absences))
            .then_with(|| a.entry.account_id.cmp(&b.entry.account_id)),
        RankingSortBy::RawAverage => quantize_k(b.eval.personal_average_raw, 4)
            .cmp(&quantize_k(a.eval.personal_average_raw, 4))
            .then_with(|| quantize_k(b.eval.s_avg, 4).cmp(&quantize_k(a.eval.s_avg, 4)))
            .then_with(|| b.eval.posted_bimesters_count.cmp(&a.eval.posted_bimesters_count))
            .then_with(|| b.eval.subjects_count.cmp(&a.eval.subjects_count))
            .then_with(|| a.eval.total_absences.cmp(&b.eval.total_absences))
            .then_with(|| a.entry.account_id.cmp(&b.entry.account_id)),
        RankingSortBy::DeltaVsClass => b
            .eval
            .tilde_delta
            .map(|d| quantize_k(d, 4))
            .cmp(&a.eval.tilde_delta.map(|d| quantize_k(d, 4)))
            .then_with(|| quantize_k(b.eval.s_avg, 4).cmp(&quantize_k(a.eval.s_avg, 4)))
            .then_with(|| b.eval.paired_count.cmp(&a.eval.paired_count))
            .then_with(|| a.entry.account_id.cmp(&b.entry.account_id)),
        RankingSortBy::Momentum => b
            .eval
            .tilde_delta_mom
            .map(|m| quantize_k(m, 4))
            .cmp(&a.eval.tilde_delta_mom.map(|m| quantize_k(m, 4)))
            .then_with(|| quantize_k(b.eval.s_avg, 4).cmp(&quantize_k(a.eval.s_avg, 4)))
            .then_with(|| b.eval.mom_pair_count.cmp(&a.eval.mom_pair_count))
            .then_with(|| a.entry.account_id.cmp(&b.entry.account_id)),
        RankingSortBy::Consistency => b
            .eval
            .consistency_score
            .map(|c| quantize_k(c, 4))
            .cmp(&a.eval.consistency_score.map(|c| quantize_k(c, 4)))
            .then_with(|| {
                let asd = a.eval.consistency_stddev_raw.map(|s| quantize_k(s, 2));
                let bsd = b.eval.consistency_stddev_raw.map(|s| quantize_k(s, 2));
                asd.cmp(&bsd)
            })
            .then_with(|| quantize_k(b.eval.s_avg, 4).cmp(&quantize_k(a.eval.s_avg, 4)))
            .then_with(|| a.entry.account_id.cmp(&b.entry.account_id)),
    }
}

fn competition_tie_key(item: &SortableEntry, sort_by: RankingSortBy) -> Option<(MetricTier, i64)> {
    if item.tier == MetricTier::G3 {
        return None;
    }
    let (_, score, _) = student_tier_and_primary_score(&item.eval, sort_by);
    score.map(|s| (item.tier, quantize_k(s, 4)))
}

fn assign_competition_ranks(items: &mut [SortableEntry], sort_by: RankingSortBy) {
    let mut prev_key: Option<(MetricTier, i64)> = None;
    let mut prev_rank = 1usize;

    for idx in 0..items.len() {
        if items[idx].tier == MetricTier::G3 {
            // Backwards-compatible non-optional `rank: usize` for G_3; `is_tied = None`.
            items[idx].entry.rank = idx + 1;
            items[idx].entry.is_tied = None;
            prev_key = None;
            continue;
        }
        let key = competition_tie_key(&items[idx], sort_by);
        if idx == 0 || prev_key.is_none() {
            let new_rank = idx + 1;
            items[idx].entry.rank = new_rank;
            prev_rank = new_rank;
        } else if key.is_some() && key == prev_key {
            items[idx].entry.rank = prev_rank;
            items[idx - 1].entry.is_tied = Some(true);
            items[idx].entry.is_tied = Some(true);
        } else {
            let new_rank = idx + 1;
            items[idx].entry.rank = new_rank;
            prev_rank = new_rank;
        }
        prev_key = key;
    }
}

fn build_sortable_entry(
    dataset: &YearRankingDataset,
    acc: &StudentAccountRecord,
    eval: StudentEval,
    requester_account_id: i64,
    social: &RequesterSocialContext,
    sort_by: RankingSortBy,
) -> SortableEntry {
    let is_me = acc.account_id == requester_account_id;
    let is_friend = !is_me && social.friend_ids.contains(&acc.account_id);
    let display_label = resolve_student_display_label(acc, is_me);
    let tags = if is_me || !acc.hide_name {
        dataset
            .tags_by_account
            .get(&acc.account_id)
            .cloned()
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    let avatar_url = (is_me || (!acc.hide_name && (acc.show_avatar || is_friend)))
        .then_some(acc.avatar_version)
        .flatten()
        .map(|v| avatar_path(acc.account_id, v));

    let (tier, _, active_cov) = student_tier_and_primary_score(&eval, sort_by);
    let is_provisional = tier != MetricTier::G1;
    let coverage_ratio = round_k(active_cov, 3);

    SortableEntry {
        entry: LeaderboardEntry {
            rank: 0,
            account_id: acc.account_id,
            is_me,
            display_label,
            is_anonymous: acc.hide_name,
            avatar_url,
            has_public_profile: is_me || !acc.hide_name,
            class_code: acc.class_code.clone(),
            personal_average: eval.personal_average_disp,
            class_average: eval.class_average_disp,
            delta_vs_class: eval.delta_vs_class_disp,
            posted_bimesters_count: eval.posted_bimesters_count,
            subjects_count: eval.subjects_count as i64,
            total_absences: eval.total_absences,
            tags,
            is_provisional: Some(is_provisional),
            coverage_ratio: Some(coverage_ratio),
            momentum_delta: eval.momentum_delta_disp,
            consistency_stddev: eval.consistency_stddev_disp,
            is_tied: None,
            is_friend: is_friend.then_some(true),
            calibrated_score: eval
                .multi_class_mode
                .then(|| round2(eval.s_avg.clamp(0.0, 10.0))),
            consistency_score: eval
                .consistency_score
                .map(|c| round2(c.clamp(0.0, 10.0))),
        },
        tier,
        eval,
    }
}

#[allow(clippy::too_many_arguments)]
fn build_highlights(
    ctx: &RankingEvalCtx<'_>,
    sorted: &[SortableEntry],
    unfiltered_delta_candidates: Option<&[SortableEntry]>,
    common_set: Option<&HashSet<String>>,
) -> Vec<CohortHighlightItem> {
    let mut highlights = Vec::with_capacity(4);

    // 1. Leader: first element of G_1 in active sort (Section 10.1.1).
    if let Some(leader) = sorted.iter().find(|s| s.tier == MetricTier::G1) {
        let (metric_label, leader_subtext) = match ctx.sort_by {
            RankingSortBy::PersonalAverage => {
                if let Some(cs) = leader.entry.calibrated_score {
                    (
                        format!("{cs:.2}"),
                        format!(
                            "{} • Média {:.2}",
                            leader.entry.class_code, leader.entry.personal_average
                        ),
                    )
                } else {
                    (
                        format!("{:.2}", leader.entry.personal_average),
                        leader.entry.class_code.clone(),
                    )
                }
            }
            RankingSortBy::RawAverage => (
                format!("{:.2}", leader.entry.personal_average),
                leader.entry.class_code.clone(),
            ),
            RankingSortBy::DeltaVsClass => (
                format!("{:+.2} vs M.T.", leader.entry.delta_vs_class),
                format!(
                    "{} • Média {:.2}",
                    leader.entry.class_code, leader.entry.personal_average
                ),
            ),
            RankingSortBy::Momentum => (
                leader
                    .entry
                    .momentum_delta
                    .map(|m| format!("{m:+.2} pts"))
                    .unwrap_or_else(|| format!("{:.2}", leader.entry.personal_average)),
                format!(
                    "{} • Média {:.2}",
                    leader.entry.class_code, leader.entry.personal_average
                ),
            ),
            RankingSortBy::Consistency => {
                if let Some(cs) = leader.entry.consistency_score {
                    (
                        format!("{cs:.2}"),
                        format!(
                            "{} • Média {:.2}",
                            leader.entry.class_code, leader.entry.personal_average
                        ),
                    )
                } else {
                    (
                        match leader.entry.consistency_stddev {
                            Some(sd) => format!("{:.2} • DP {:.2}", leader.entry.personal_average, sd),
                            None => format!("{:.2}", leader.entry.personal_average),
                        },
                        leader.entry.class_code.clone(),
                    )
                }
            }
        };
        highlights.push(CohortHighlightItem {
            kind: "leader".to_string(),
            title: "Líder do recorte".to_string(),
            subject_or_label: leader.entry.display_label.clone(),
            value_text: metric_label,
            subtext: leader_subtext,
            account_id: Some(leader.entry.account_id),
        });
    }

    // 2. Top riser: element of G_1^{mom} maximizing Q_4(tilde_delta_mom) > 0.10 (Section 10.1.2).
    if let Some(riser) = sorted
        .iter()
        .filter(|s| {
            s.eval.is_qualified_mom && s.eval.tilde_delta_mom.is_some_and(|m| m > 0.10)
        })
        .max_by(|a, b| {
            let am = a.eval.tilde_delta_mom.map(|v| quantize_k(v, 4));
            let bm = b.eval.tilde_delta_mom.map(|v| quantize_k(v, 4));
            am.cmp(&bm)
                .then_with(|| quantize_k(a.eval.s_avg, 4).cmp(&quantize_k(b.eval.s_avg, 4)))
                .then_with(|| a.eval.mom_pair_count.cmp(&b.eval.mom_pair_count))
                .then_with(|| b.entry.account_id.cmp(&a.entry.account_id))
        })
        && let Some(mom) = riser.entry.momentum_delta
    {
        highlights.push(CohortHighlightItem {
            kind: "top_riser".to_string(),
            title: "Maior evolução".to_string(),
            subject_or_label: riser.entry.display_label.clone(),
            value_text: format!("+{mom:.2} pts"),
            subtext: format!(
                "{} • Média {:.2}",
                riser.entry.class_code, riser.entry.personal_average
            ),
            account_id: Some(riser.entry.account_id),
        });
    }

    // 3. Top delta: ALWAYS over P_i with S_com = None, element of G_1^Delta with tilde_delta > 0.10 (Section 10.1.3).
    let delta_pool = unfiltered_delta_candidates.unwrap_or(sorted);
    if let Some(top_d) = delta_pool
        .iter()
        .filter(|s| s.eval.is_qualified_delta && s.eval.tilde_delta.is_some_and(|d| d > 0.10))
        .max_by(|a, b| {
            let ad = a.eval.tilde_delta.map(|v| quantize_k(v, 4));
            let bd = b.eval.tilde_delta.map(|v| quantize_k(v, 4));
            ad.cmp(&bd)
                .then_with(|| quantize_k(a.eval.s_avg, 4).cmp(&quantize_k(b.eval.s_avg, 4)))
                .then_with(|| a.eval.paired_count.cmp(&b.eval.paired_count))
                .then_with(|| b.entry.account_id.cmp(&a.entry.account_id))
        })
    {
        highlights.push(CohortHighlightItem {
            kind: "top_delta".to_string(),
            title: "Destaque vs. turma".to_string(),
            subject_or_label: top_d.entry.display_label.clone(),
            value_text: format!("+{:.2} vs M.T.", top_d.entry.delta_vs_class),
            subtext: format!(
                "{} • Média {:.2}",
                top_d.entry.class_code, top_d.entry.personal_average
            ),
            account_id: Some(top_d.entry.account_id),
        });
    }

    // 4. Hardest subject in scope using median M.T. consensus (Section 10.1.5).
    let g1_entries: Vec<&SortableEntry> =
        sorted.iter().filter(|s| s.tier == MetricTier::G1).collect();
    let ref_classes: HashSet<String> = if ctx.scope == RankingScope::Class {
        let mut set = HashSet::new();
        if is_valid_class_code(&ctx.requester_desc.code) {
            set.insert(ctx.requester_desc.code.clone());
        }
        set
    } else if !g1_entries.is_empty() {
        g1_entries
            .iter()
            .filter(|s| is_valid_class_code(&s.entry.class_code))
            .map(|s| s.entry.class_code.clone())
            .collect()
    } else {
        sorted
            .iter()
            .filter(|s| is_valid_class_code(&s.entry.class_code))
            .map(|s| s.entry.class_code.clone())
            .collect()
    };

    let friends_subj_restrict: Option<HashSet<&str>> = if ctx.scope == RankingScope::Friends {
        let pool: Vec<&SortableEntry> = if !g1_entries.is_empty() {
            g1_entries
        } else {
            sorted.iter().collect()
        };
        let mut set = HashSet::new();
        for s in pool {
            for subj in &s.eval.active_subjects {
                set.insert(subj.as_str());
            }
        }
        Some(set)
    } else {
        None
    };

    if let Some((hardest_norm, avg_mt)) = select_hardest_subject(
        ctx.model,
        &ref_classes,
        ctx.bimester,
        ctx.subject_filter_norm,
        common_set,
        friends_subj_restrict.as_ref(),
    ) {
        let canonical = ctx
            .dataset
            .canonical_subject_names
            .get(&hardest_norm)
            .cloned()
            .unwrap_or(hardest_norm);
        highlights.push(CohortHighlightItem {
            kind: "hardest_subject".to_string(),
            title: "Matéria mais exigente".to_string(),
            subject_or_label: canonical,
            value_text: format!("M.T. {avg_mt:.2}"),
            subtext: "Menor média oficial das turmas".to_string(),
            account_id: None,
        });
    }

    highlights
}

/// Selects the hardest subject using median `M_hat_{c,s,b}` with minimum class support `k_sup` (Section 10.1.5).
fn select_hardest_subject(
    model: &YearStatisticalModel,
    ref_classes: &HashSet<String>,
    bimester: Option<i16>,
    subject_filter_norm: Option<&str>,
    common_set: Option<&HashSet<String>>,
    restrict_subjects: Option<&HashSet<&str>>,
) -> Option<(String, f64)> {
    if ref_classes.is_empty() {
        return None;
    }

    // norm_subj -> (mt_sum, cell_count, distinct_classes)
    let mut by_subj: HashMap<&str, (f64, usize, HashSet<&str>)> = HashMap::new();
    for ((c, s, b), &mt) in &model.mt_consensus {
        if !ref_classes.contains(c) {
            continue;
        }
        if !bimester.is_none_or(|b0| *b == b0) {
            continue;
        }
        if !matches_subject_filter(s, subject_filter_norm) || !matches_common_set(s, common_set) {
            continue;
        }
        if let Some(allowed) = restrict_subjects
            && !allowed.contains(s.as_str())
        {
            continue;
        }
        let slot = by_subj
            .entry(s.as_str())
            .or_insert_with(|| (0.0, 0, HashSet::new()));
        slot.0 += mt;
        slot.1 += 1;
        slot.2.insert(c.as_str());
    }

    let k_sup_preferred = if ref_classes.len() >= 2 && subject_filter_norm.is_none() {
        2usize
    } else {
        1usize
    };

    let pick = |min_classes: usize| -> Option<(String, f64)> {
        by_subj
            .iter()
            .filter(|(_, (_, cnt, classes))| *cnt > 0 && classes.len() >= min_classes)
            .map(|(&s, (sum, cnt, _))| {
                let mean = sum / (*cnt as f64);
                (s, quantize_k(mean, 2), round2(mean))
            })
            .min_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(b.0)))
            .map(|(s, _, disp)| (s.to_string(), disp))
    };

    pick(k_sup_preferred).or_else(|| {
        if k_sup_preferred > 1 {
            pick(1)
        } else {
            None
        }
    })
}

#[allow(clippy::too_many_arguments)]
fn build_my_benchmark(
    ctx: &RankingEvalCtx<'_>,
    base_curricular: &BaseCurricularExpectation,
    curricular: &CurricularContext,
    common_set: Option<&HashSet<String>>,
    sorted: &[SortableEntry],
    c_eff_alunos: &HashSet<String>,
    multi_class_mode: bool,
    t_star: Option<i16>,
    my_rank: Option<usize>,
    my_tier: Option<MetricTier>,
    n1_qualified: usize,
) -> Option<MyBenchmarkSummary> {
    let raw_grades = ctx.dataset.grades_by_account.get(&ctx.requester_account_id)?;
    let my_cells = deduplicate_student_cells(raw_grades);
    if my_cells.is_empty() {
        return None;
    }

    let my_class_code = &ctx.requester_desc.code;

    // Evaluate with active common_set; fallback locally to S_com = None if empty (Section 10.2.1).
    let eval_with_cur = |cur: &CurricularContext,
                         cs: Option<&HashSet<String>>,
                         mc_mode: bool| {
        evaluate_student_v7(
            &my_cells,
            my_class_code,
            ctx.requester_desc,
            ctx.model,
            cur,
            ctx.bimester,
            ctx.subject_filter_norm,
            cs,
            mc_mode,
            t_star,
        )
    };

    let mut fallback_cur: Option<CurricularContext> = None;
    let (eval, used_fallback, eff_common_set) = match eval_with_cur(curricular, common_set, multi_class_mode) {
        Some(ev) => (ev, false, common_set),
        None if common_set.is_some() => {
            let fb_cur = CurricularContext::finalize(
                base_curricular,
                ctx.dataset,
                ctx.vis_cells,
                ctx.subject_filter_norm,
                None,
            );
            let ev = eval_with_cur(&fb_cur, None, multi_class_mode)?;
            fallback_cur = Some(fb_cur);
            (ev, true, None)
        }
        None => return None,
    };
    let _eff_cur = fallback_cur.as_ref().unwrap_or(curricular);

    // 3. Per-subject stats for subjects_above_class / subjects_below_class (paired real M.T., Section 10.2.3)
    //    and standout_subject / focus_subject (unified reference m_tilde, Section 10.2.4).
    struct SubjBenchAcc {
        grade_sum: f64,
        grade_cnt: usize,
        m_tilde_sum: f64,
        paired_grade_sum: f64,
        paired_mt_sum: f64,
        paired_cnt: usize,
        fallback_name: String,
    }
    let mut by_subj: BTreeMap<&str, SubjBenchAcc> = BTreeMap::new();
    for ((norm_subj, bim), cell) in &my_cells {
        if !ctx.bimester.is_none_or(|b0| *bim == b0)
            || !matches_subject_filter(norm_subj, ctx.subject_filter_norm)
            || !matches_common_set(norm_subj, eff_common_set)
        {
            continue;
        }
        let m_tilde = ctx.model.cell_reference_mt(
            my_class_code,
            ctx.requester_desc.grade_level,
            norm_subj,
            *bim,
        );
        let mt_real = ctx
            .model
            .mt_consensus
            .get(&(my_class_code.clone(), norm_subj.clone(), *bim))
            .copied();

        let slot = by_subj
            .entry(norm_subj.as_str())
            .or_insert_with(|| SubjBenchAcc {
                grade_sum: 0.0,
                grade_cnt: 0,
                m_tilde_sum: 0.0,
                paired_grade_sum: 0.0,
                paired_mt_sum: 0.0,
                paired_cnt: 0,
                fallback_name: cell.subject_name.clone(),
            });
        slot.grade_sum += cell.grade;
        slot.grade_cnt += 1;
        slot.m_tilde_sum += m_tilde;
        if let Some(mt) = mt_real {
            slot.paired_grade_sum += cell.grade;
            slot.paired_mt_sum += mt;
            slot.paired_cnt += 1;
        }
    }

    let mut subjects_above_class = 0usize;
    let mut subjects_below_class = 0usize;

    struct SubjSpotlightItem<'a> {
        norm_key: &'a str,
        name: String,
        g_bar: f64,
        delta_tilde: f64,
        display_mt: f64,
        display_delta: f64,
    }
    let mut spotlight_items: Vec<SubjSpotlightItem<'_>> = Vec::with_capacity(by_subj.len());

    for (norm_key, acc) in by_subj {
        if acc.grade_cnt == 0 {
            continue;
        }
        let g_bar = acc.grade_sum / (acc.grade_cnt as f64);
        let m_tilde_bar = acc.m_tilde_sum / (acc.grade_cnt as f64);
        let delta_tilde = g_bar - m_tilde_bar;

        let (display_mt, display_delta) = if acc.paired_cnt > 0 {
            let p_g = acc.paired_grade_sum / (acc.paired_cnt as f64);
            let p_mt = acc.paired_mt_sum / (acc.paired_cnt as f64);
            let q_d = quantize_k(p_g - p_mt, 2);
            if q_d > 0 {
                subjects_above_class += 1;
            } else if q_d < 0 {
                subjects_below_class += 1;
            }
            (round2(p_mt), round2(p_g - p_mt))
        } else {
            (round2(m_tilde_bar), round2(delta_tilde))
        };

        let name = ctx
            .dataset
            .canonical_subject_names
            .get(norm_key)
            .cloned()
            .unwrap_or(acc.fallback_name);

        spotlight_items.push(SubjSpotlightItem {
            norm_key,
            name,
            g_bar,
            delta_tilde,
            display_mt,
            display_delta,
        });
    }

    let standout_idx = spotlight_items
        .iter()
        .enumerate()
        .max_by(|(_, a), (_, b)| {
            quantize_k(a.delta_tilde, 4)
                .cmp(&quantize_k(b.delta_tilde, 4))
                .then_with(|| quantize_k(a.g_bar, 4).cmp(&quantize_k(b.g_bar, 4)))
                .then_with(|| b.norm_key.cmp(a.norm_key))
        })
        .map(|(i, _)| i);

    let focus_idx = if spotlight_items.len() >= 2 {
        spotlight_items
            .iter()
            .enumerate()
            .filter(|(i, _)| Some(*i) != standout_idx)
            .min_by(|(_, a), (_, b)| {
                quantize_k(a.delta_tilde, 4)
                    .cmp(&quantize_k(b.delta_tilde, 4))
                    .then_with(|| quantize_k(a.g_bar, 4).cmp(&quantize_k(b.g_bar, 4)))
                    .then_with(|| a.norm_key.cmp(b.norm_key))
            })
            .map(|(i, _)| i)
    } else {
        None
    };

    let to_spotlight = |item: &SubjSpotlightItem<'_>| SubjectSpotlight {
        name: item.name.clone(),
        grade: round2(item.g_bar),
        class_average: item.display_mt,
        delta: item.display_delta,
    };
    let standout_subject = standout_idx.map(|i| to_spotlight(&spotlight_items[i]));
    let focus_subject = focus_idx.map(|i| to_spotlight(&spotlight_items[i]));

    let (my_local_tier, _, my_active_cov) = student_tier_and_primary_score(&eval, ctx.sort_by);
    let is_provisional = used_fallback || my_local_tier != MetricTier::G1;
    let coverage_ratio = round_k(my_active_cov, 3);

    // 5. hypothetical_rank & hypothetical_total strictly over G_1 (Section 10.2.5).
    let (hypothetical_rank, hypothetical_total) = if my_tier.is_some() {
        // Requester is already in the leaderboard.
        if my_tier == Some(MetricTier::G1) {
            (my_rank, Some(n1_qualified))
        } else {
            (None, None)
        }
    } else if !used_fallback && my_local_tier == MetricTier::G1 {
        let mut classes_sim = c_eff_alunos.clone();
        if is_valid_class_code(my_class_code) {
            classes_sim.insert(my_class_code.clone());
        }
        let multi_class_mode_sim = classes_sim.len() >= 2;

        if let Some(acc) = ctx.dataset.accounts.get(&ctx.requester_account_id) {
            let mut g1_sim: Vec<SortableEntry> = if multi_class_mode_sim == multi_class_mode {
                sorted
                    .iter()
                    .filter(|s| s.tier == MetricTier::G1)
                    .cloned()
                    .collect()
            } else {
                // Re-evaluate G_1 under multi_class_mode_sim using frozen global parameters.
                sorted
                    .iter()
                    .filter(|s| s.tier == MetricTier::G1)
                    .filter_map(|s| {
                        let peer_acc = ctx.dataset.accounts.get(&s.entry.account_id)?;
                        let peer_cells = ctx.vis_cells.get(&s.entry.account_id)?;
                        let peer_eval = evaluate_student_v7(
                            peer_cells,
                            &peer_acc.class_code,
                            &peer_acc.class_desc,
                            ctx.model,
                            curricular,
                            ctx.bimester,
                            ctx.subject_filter_norm,
                            common_set,
                            multi_class_mode_sim,
                            t_star,
                        )?;
                        Some(build_sortable_entry(
                            ctx.dataset,
                            peer_acc,
                            peer_eval,
                            ctx.requester_account_id,
                            ctx.social,
                            ctx.sort_by,
                        ))
                    })
                    .collect()
            };

            let my_eval_sim = if multi_class_mode_sim == multi_class_mode {
                eval.clone()
            } else {
                eval_with_cur(curricular, common_set, multi_class_mode_sim).unwrap_or_else(|| eval.clone())
            };
            let my_sim = build_sortable_entry(
                ctx.dataset,
                acc,
                my_eval_sim,
                ctx.requester_account_id,
                ctx.social,
                ctx.sort_by,
            );
            g1_sim.push(my_sim);
            g1_sim.sort_by(|a, b| compare_sortable_entries(a, b, ctx.sort_by));
            assign_competition_ranks(&mut g1_sim, ctx.sort_by);
            let sim_rank = g1_sim
                .iter()
                .find(|s| s.entry.account_id == ctx.requester_account_id)
                .map(|s| s.entry.rank);
            (sim_rank, Some(n1_qualified + 1))
        } else {
            (None, None)
        }
    } else {
        (None, None)
    };

    let theta_c = ctx.model.theta_c.get(my_class_code).copied().unwrap_or(0.0);
    let sigma_c = ctx
        .model
        .sigma_c
        .get(my_class_code)
        .copied()
        .unwrap_or(SIGMA_0);

    // Closed-form target average in requester's own class to match #1 in PersonalAverage or RawAverage.
    let is_already_leader = my_rank == Some(1) || hypothetical_rank == Some(1);
    let leader_target_average = if matches!(
        ctx.sort_by,
        RankingSortBy::PersonalAverage | RankingSortBy::RawAverage
    ) && !is_already_leader
        && let Some(leader) = sorted.iter().find(|s| s.tier == MetricTier::G1)
    {
        let raw_target = if ctx.sort_by == RankingSortBy::RawAverage {
            leader.entry.personal_average
        } else if multi_class_mode {
            let base_c = ctx.model.mu_0 + ALPHA_THETA * theta_c;
            let s_leader = leader.eval.s_avg;
            if s_leader >= base_c {
                let span_c = (10.0 - base_c).max(0.10);
                let h_req = ((s_leader - base_c) / span_c).clamp(0.0, 1.0);
                let h_cap = (10.0 - eval.m_bar_active).max(0.50);
                eval.m_bar_active + h_cap * h_req
            } else {
                let z_req = (s_leader - base_c) / ctx.model.sigma_scope.max(0.10);
                eval.m_bar_active + sigma_c * z_req
            }
        } else {
            leader.entry.personal_average
        };
        Some(round2(raw_target.clamp(0.0, 10.0)))
    } else {
        None
    };

    let nerd_stats = RankingNerdStats {
        school_mu_0: round2(ctx.model.mu_0),
        school_sigma: round2(ctx.model.sigma_scope),
        class_theta: round2(theta_c),
        class_sigma: round2(sigma_c),
        class_mean_mt: round2(eval.m_bar_active),
        z_score: round2(eval.z_bar_rel),
        coverage_2d_pct: round1(eval.cov_2d_base * 100.0),
        active_bimesters: eval.active_bimesters_count,
        ridge_active: !ctx.model.theta_c.is_empty(),
        consistency_cara_beta: BETA_STUDENT,
        multi_class_calibrated: multi_class_mode,
    };

    Some(MyBenchmarkSummary {
        class_code: ctx.requester_desc.code.clone(),
        personal_average: eval.personal_average_disp,
        class_average: eval.class_average_disp,
        delta_vs_class: eval.delta_vs_class_disp,
        subjects_count: eval.subjects_count as i64,
        posted_bimesters_count: eval.posted_bimesters_count,
        total_absences: eval.total_absences,
        subjects_above_class,
        subjects_below_class,
        is_provisional,
        coverage_ratio,
        momentum_delta: eval.momentum_delta_disp,
        consistency_stddev: eval.consistency_stddev_disp,
        standout_subject,
        focus_subject,
        hypothetical_rank,
        hypothetical_total,
        calibrated_score: multi_class_mode.then(|| round2(eval.s_avg.clamp(0.0, 10.0))),
        leader_target_average,
        headroom_pct: Some(eval.headroom_pct),
        nerd_stats: Some(nerd_stats),
    })
}

#[derive(Debug, Clone, Serialize)]
pub struct CustomSliceBimesterPoint {
    pub bimester: i16,
    pub my_avg: Option<f64>,
    pub class_avg: Option<f64>,
    pub subjects_count: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct CrossYearSliceStat {
    pub school_year: i32,
    pub students_count: usize,
    pub school_avg: f64,
    pub my_percentile_in_year: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CustomSliceAnalyticsResponse {
    pub school_year: i32,
    pub selected_subjects_count: usize,
    pub selected_bimesters: Vec<i16>,
    pub my_avg: Option<f64>,
    pub my_class_avg: Option<f64>,
    pub my_delta_vs_class: Option<f64>,
    pub evolution_delta: Option<f64>,
    pub same_class_top_pct: Option<f64>,
    pub same_class_total: usize,
    pub same_year_school_top_pct: Option<f64>,
    pub same_year_school_avg: Option<f64>,
    pub same_year_school_total: usize,
    pub bimester_series: Vec<CustomSliceBimesterPoint>,
    pub cross_year_stats: Vec<CrossYearSliceStat>,
}

/// Fast, cache-backed aggregated analytics for a custom subset of subjects & bimesters.
/// Strictly excludes `invisible == true` accounts and never exposes individual student data.
pub fn build_custom_slice_analytics(
    primary_dataset: &YearRankingDataset,
    other_year_datasets: &[&YearRankingDataset],
    requester_account_id: i64,
    subjects_filter: Option<&[String]>,
    bimesters_filter: Option<&[i16]>,
) -> CustomSliceAnalyticsResponse {
    let subj_set: Option<HashSet<String>> = subjects_filter
        .filter(|s| !s.is_empty())
        .map(|list| list.iter().map(|s| normalize_subject_key(s)).collect());

    let bim_set: BTreeSet<i16> = bimesters_filter
        .filter(|b| !b.is_empty())
        .map(|list| {
            list.iter()
                .copied()
                .filter(|b| (1..=4).contains(b))
                .collect()
        })
        .unwrap_or_else(|| [1, 2, 3, 4].into_iter().collect());

    let matches_slice = |norm_subj: &str, bim: i16| -> bool {
        bim_set.contains(&bim)
            && subj_set
                .as_ref()
                .is_none_or(|set| set.contains(norm_subj))
    };

    let compute_student_avg = |grades: &[PostedGradeRecord]| -> Option<f64> {
        let cells = deduplicate_student_cells(grades);
        let mut sum = 0.0_f64;
        let mut cnt = 0usize;
        for ((s, b), cell) in &cells {
            if matches_slice(s, *b) {
                sum += cell.grade;
                cnt += 1;
            }
        }
        (cnt > 0).then(|| sum / (cnt as f64))
    };

    // 1. Requester's own slice stats + bimester series
    let mut my_avg: Option<f64> = None;
    let mut my_class_avg: Option<f64> = None;
    let mut evolution_delta: Option<f64> = None;
    let mut bimester_series: Vec<CustomSliceBimesterPoint> = Vec::new();
    let mut selected_subjects_set: HashSet<String> = HashSet::new();

    if let Some(my_grades) = primary_dataset.grades_by_account.get(&requester_account_id) {
        let mt_map: HashMap<(String, i16), f64> = my_grades
            .iter()
            .filter_map(|g| {
                g.average_grade
                    .filter(|&m| m.is_finite() && m > 0.0)
                    .map(|m| ((g.normalized_subject.clone(), g.bimester), m))
            })
            .collect();
        let cells = deduplicate_student_cells(my_grades);
        let mut p_sum = 0.0_f64;
        let mut p_cnt = 0usize;
        let mut c_sum = 0.0_f64;
        let mut c_cnt = 0usize;

        let mut by_bim: BTreeMap<i16, (f64, usize, f64, usize)> = BTreeMap::new();
        for ((s, b), cell) in &cells {
            if !matches_slice(s, *b) {
                continue;
            }
            selected_subjects_set.insert(s.clone());
            p_sum += cell.grade;
            p_cnt += 1;
            let slot = by_bim.entry(*b).or_insert((0.0, 0, 0.0, 0));
            slot.0 += cell.grade;
            slot.1 += 1;
            if let Some(&mt) = mt_map.get(&(s.clone(), *b)) {
                c_sum += mt;
                c_cnt += 1;
                slot.2 += mt;
                slot.3 += 1;
            }
        }

        if p_cnt > 0 {
            my_avg = Some(round2(p_sum / (p_cnt as f64)));
        }
        if c_cnt > 0 {
            my_class_avg = Some(round2(c_sum / (c_cnt as f64)));
        }

        let mut bim_avgs: Vec<f64> = Vec::new();
        for (&b, &(ps, pc, cs, cc)) in &by_bim {
            let m_avg = (pc > 0).then(|| round2(ps / (pc as f64)));
            if let Some(v) = m_avg {
                bim_avgs.push(v);
            }
            bimester_series.push(CustomSliceBimesterPoint {
                bimester: b,
                my_avg: m_avg,
                class_avg: (cc > 0).then(|| round2(cs / (cc as f64))),
                subjects_count: pc,
            });
        }
        if bim_avgs.len() >= 2 {
            let last = bim_avgs[bim_avgs.len() - 1];
            let prev = bim_avgs[bim_avgs.len() - 2];
            evolution_delta = Some(round2(last - prev));
        }
    }

    let my_delta_vs_class = match (my_avg, my_class_avg) {
        (Some(m), Some(c)) => Some(round2(m - c)),
        _ => None,
    };

    // 2. Same-year aggregated cohort percentiles (visible accounts only, zero individual leak)
    let my_class_code = primary_dataset
        .accounts
        .get(&requester_account_id)
        .map(|a| a.class_code.as_str())
        .unwrap_or("");

    let mut class_avgs: Vec<f64> = Vec::new();
    let mut school_avgs: Vec<f64> = Vec::new();

    for (&acct_id, acc) in &primary_dataset.accounts {
        if acc.invisible {
            continue;
        }
        let Some(grades) = primary_dataset.grades_by_account.get(&acct_id) else {
            continue;
        };
        let Some(avg) = compute_student_avg(grades) else {
            continue;
        };
        school_avgs.push(avg);
        if !my_class_code.is_empty() && acc.class_code.eq_ignore_ascii_case(my_class_code) {
            class_avgs.push(avg);
        }
    }

    let top_pct_for = |pool: &[f64], target: Option<f64>| -> Option<f64> {
        let t = target?;
        if pool.len() < 3 {
            return None;
        }
        let strictly_better = pool.iter().filter(|&&v| v > t + 1e-6).count();
        let rank = strictly_better + 1;
        let pct = ((rank as f64) / (pool.len() as f64)) * 100.0;
        Some(round1(pct.clamp(1.0, 100.0)))
    };

    let same_class_top_pct = top_pct_for(&class_avgs, my_avg);
    let same_year_school_top_pct = top_pct_for(&school_avgs, my_avg);
    let same_year_school_avg = (school_avgs.len() >= 3)
        .then(|| round2(school_avgs.iter().sum::<f64>() / (school_avgs.len() as f64)));

    // 3. Cross-year aggregated comparison
    let mut cross_year_stats: Vec<CrossYearSliceStat> = Vec::new();
    for ds in other_year_datasets {
        let mut yr_avgs: Vec<f64> = Vec::new();
        for (&acct_id, acc) in &ds.accounts {
            if acc.invisible {
                continue;
            }
            if let Some(grades) = ds.grades_by_account.get(&acct_id)
                && let Some(avg) = compute_student_avg(grades)
            {
                yr_avgs.push(avg);
            }
        }
        if yr_avgs.len() >= 3 {
            let yr_mean = round2(yr_avgs.iter().sum::<f64>() / (yr_avgs.len() as f64));
            cross_year_stats.push(CrossYearSliceStat {
                school_year: ds.school_year,
                students_count: yr_avgs.len(),
                school_avg: yr_mean,
                my_percentile_in_year: top_pct_for(&yr_avgs, my_avg),
            });
        }
    }

    CustomSliceAnalyticsResponse {
        school_year: primary_dataset.school_year,
        selected_subjects_count: subj_set.as_ref().map_or(selected_subjects_set.len(), HashSet::len),
        selected_bimesters: bim_set.into_iter().collect(),
        my_avg,
        my_class_avg,
        my_delta_vs_class,
        evolution_delta,
        same_class_top_pct,
        same_class_total: class_avgs.len(),
        same_year_school_top_pct,
        same_year_school_avg,
        same_year_school_total: school_avgs.len(),
        bimester_series,
        cross_year_stats,
    }
}

/// Pure in-memory multi-ranking engine operating on a cached `YearRankingDataset` (Specification v7).
pub fn build_comparison_summary(
    dataset: &YearRankingDataset,
    requester_account_id: i64,
    requester_fallback_class: Option<&str>,
    my_prefs: RankingPreferences,
    social: &RequesterSocialContext,
    query: &RankingQuery,
) -> ComparisonSummaryResponse {
    let requester_class_str = dataset
        .accounts
        .get(&requester_account_id)
        .map(|a| a.class_code.as_str())
        .or(requester_fallback_class)
        .unwrap_or("CTI");
    let requester_desc = ClassDescriptor::parse(requester_class_str);

    let subject_filter_clean = query
        .subject_filter
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToString::to_string);
    let subject_filter_norm = subject_filter_clean
        .as_deref()
        .map(normalize_subject_key)
        .filter(|s| !s.is_empty());

    let scope = query.scope;
    let sort_by = query.sort_by;

    // Global default: `common_only = false` across all scopes (Section 9.5).
    let common_only_requested = query.common_only.unwrap_or(false);

    // 1. Global visible-only statistical model & deduplicated cells (Sections 1, 2, 4.1).
    let model = YearStatisticalModel::from_dataset(dataset);
    let vis_cells = collect_visible_cells(dataset);
    let base_curricular =
        BaseCurricularExpectation::build(dataset, &model, &vis_cells, query.bimester);

    // 2. Gather visible accounts in scope (strictly `!acc.invisible`, Section 1.1).
    let mut scope_vis_account_ids: Vec<i64> = Vec::new();
    let mut scope_eligible_ids: Vec<i64> = Vec::new();
    for (&acct_id, acc) in &dataset.accounts {
        if acc.invisible {
            continue;
        }
        if !account_in_scope(acc, requester_account_id, &requester_desc, scope, social) {
            continue;
        }
        scope_vis_account_ids.push(acct_id);
        if acc.opted_in && !social.blocked_ids.contains(&acct_id) {
            scope_eligible_ids.push(acct_id);
        }
    }

    // Available subjects in scope from visible accounts ONLY (before subject_filter).
    let mut avail_norm: BTreeSet<String> = BTreeSet::new();
    for &acct_id in &scope_vis_account_ids {
        if let Some(cells) = vis_cells.get(&acct_id) {
            for (norm_subj, bim) in cells.keys() {
                if query.bimester.is_none_or(|b| *bim == b) {
                    avail_norm.insert(norm_subj.clone());
                }
            }
        }
    }
    let mut available_subjects: Vec<String> = avail_norm
        .into_iter()
        .map(|k| {
            dataset
                .canonical_subject_names
                .get(&k)
                .cloned()
                .unwrap_or(k)
        })
        .collect();
    available_subjects.sort_by_key(|s| nsac_core::filters::normalize_text(s));

    let ctx = RankingEvalCtx {
        dataset,
        model: &model,
        vis_cells: &vis_cells,
        requester_account_id,
        requester_desc: &requester_desc,
        scope,
        sort_by,
        bimester: query.bimester,
        subject_filter_norm: subject_filter_norm.as_deref(),
        social,
    };

    // 3. Resolve opt-in `common_set` (Section 9.5).
    let common_res =
        resolve_common_subjects(&ctx, &base_curricular, &scope_eligible_ids, common_only_requested);

    // 4. Finalize Section 3 curricular expectation & consolidation.
    let curricular = CurricularContext::finalize(
        &base_curricular,
        dataset,
        &vis_cells,
        subject_filter_norm.as_deref(),
        common_res.common_set.as_ref(),
    );

    // 5. Determine effective classes C_eff^{alunos} and multi-class mode (Section 4.3).
    let mut c_eff_alunos: HashSet<String> = HashSet::new();
    for &acct_id in &scope_eligible_ids {
        let Some(acc) = dataset.accounts.get(&acct_id) else {
            continue;
        };
        let Some(cells) = vis_cells.get(&acct_id) else {
            continue;
        };
        let has_active = cells.keys().any(|(s, b)| {
            query.bimester.is_none_or(|b0| *b == b0)
                && matches_subject_filter(s, subject_filter_norm.as_deref())
                && matches_common_set(s, common_res.common_set.as_ref())
        });
        if has_active && is_valid_class_code(&acc.class_code) {
            c_eff_alunos.insert(acc.class_code.clone());
        }
    }
    let multi_class_mode = c_eff_alunos.len() >= 2;

    // 6. Shared terminal bimester t* for Momentum (Section 6.2).
    let t_star = resolve_momentum_terminal_bimester(
        dataset,
        &vis_cells,
        &scope_vis_account_ids,
        &curricular,
        query.bimester,
        subject_filter_norm.as_deref(),
        common_res.common_set.as_ref(),
    );

    // 7. Evaluate eligible participants, sort (G1 < G2 < G3), assign competition ranks.
    let mut sortable: Vec<SortableEntry> = Vec::new();
    for &acct_id in &scope_eligible_ids {
        let Some(acc) = dataset.accounts.get(&acct_id) else {
            continue;
        };
        let Some(cells) = vis_cells.get(&acct_id) else {
            continue;
        };
        if let Some(eval) = evaluate_student_v7(
            cells,
            &acc.class_code,
            &acc.class_desc,
            &model,
            &curricular,
            query.bimester,
            subject_filter_norm.as_deref(),
            common_res.common_set.as_ref(),
            multi_class_mode,
            t_star,
        ) {
            sortable.push(build_sortable_entry(
                dataset,
                acc,
                eval,
                requester_account_id,
                social,
                sort_by,
            ));
        }
    }

    sortable.sort_by(|a, b| compare_sortable_entries(a, b, sort_by));
    assign_competition_ranks(&mut sortable, sort_by);

    let total_participants = sortable.len();
    let n1_qualified = sortable
        .iter()
        .filter(|s| s.tier == MetricTier::G1)
        .count();

    let mut my_rank: Option<usize> = None;
    let mut my_tier: Option<MetricTier> = None;
    let mut my_entry: Option<LeaderboardEntry> = None;
    for s in &sortable {
        if s.entry.is_me {
            my_tier = Some(s.tier);
            if s.tier != MetricTier::G3 {
                my_rank = Some(s.entry.rank);
            }
            my_entry = Some(s.entry.clone());
            break;
        }
    }

    // Tied midpoint percentile exclusively over G_1 (Section 9.4).
    let my_percentile = if my_tier == Some(MetricTier::G1)
        && let Some(r) = my_rank
    {
        let tie_count = sortable
            .iter()
            .filter(|s| s.tier == MetricTier::G1 && s.entry.rank == r)
            .count();
        compute_tied_midpoint_percentile(r, tie_count, n1_qualified)
    } else {
        None
    };

    // Expected subjects count for response metadata.
    let expected_subjects = if let Some(ref cs) = common_res.common_set {
        cs.len()
    } else if is_valid_class_code(&requester_desc.code) {
        curricular
            .for_class(&requester_desc.code, &requester_desc)
            .e_subj_eff
    } else {
        let es: Vec<usize> = c_eff_alunos
            .iter()
            .filter_map(|c| curricular.per_class.get(c).map(|info| info.e_subj_eff))
            .collect();
        if es.is_empty() { 0 } else { p75_usize(&es) }
    };

    let my_benchmark = build_my_benchmark(
        &ctx,
        &base_curricular,
        &curricular,
        common_res.common_set.as_ref(),
        &sortable,
        &c_eff_alunos,
        multi_class_mode,
        t_star,
        my_rank,
        my_tier,
        n1_qualified,
    );

    // If common_set is active, compute unfiltered DeltaVsClass candidates for `top_delta` (Section 10.1.3).
    let unfiltered_delta_candidates: Option<Vec<SortableEntry>> = if common_res.common_set.is_some() {
        let unfiltered_cur = CurricularContext::finalize(
            &base_curricular,
            dataset,
            &vis_cells,
            subject_filter_norm.as_deref(),
            None,
        );
        let mut cands = Vec::with_capacity(sortable.len());
        for &acct_id in &scope_eligible_ids {
            let Some(acc) = dataset.accounts.get(&acct_id) else {
                continue;
            };
            let Some(cells) = vis_cells.get(&acct_id) else {
                continue;
            };
            if let Some(eval) = evaluate_student_v7(
                cells,
                &acc.class_code,
                &acc.class_desc,
                &model,
                &unfiltered_cur,
                query.bimester,
                subject_filter_norm.as_deref(),
                None,
                multi_class_mode,
                None,
            ) {
                cands.push(build_sortable_entry(
                    dataset,
                    acc,
                    eval,
                    requester_account_id,
                    social,
                    RankingSortBy::DeltaVsClass,
                ));
            }
        }
        Some(cands)
    } else {
        None
    };

    let highlights = build_highlights(
        &ctx,
        &sortable,
        unfiltered_delta_candidates.as_deref(),
        common_res.common_set.as_ref(),
    );

    let leaderboard: Vec<LeaderboardEntry> = sortable.into_iter().map(|s| s.entry).collect();

    let peer_comparison = query.compare_with_account_id.and_then(|peer_id| {
        if peer_id == requester_account_id || social.blocked_ids.contains(&peer_id) {
            return None;
        }
        // Section 10.3: 1v1 always evaluates with S_com = None so peer_entry matches subjects.
        build_peer_comparison_from_dataset(
            dataset,
            requester_account_id,
            peer_id,
            social,
            query.bimester,
            None,
        )
    });

    ComparisonSummaryResponse {
        scope: scope.as_str().to_string(),
        school_year: dataset.school_year,
        bimester: query.bimester,
        subject_filter: subject_filter_clean,
        total_participants,
        my_preferences: my_prefs,
        my_rank,
        my_percentile,
        my_entry,
        leaderboard,
        peer_comparison,
        scope_label: Some(scope_label_pt(scope, &requester_desc)),
        sort_by: Some(sort_by.as_str().to_string()),
        common_only: Some(common_res.effective_common_only),
        common_subjects: common_res.common_subjects_names,
        common_subjects_note: common_res.note,
        qualified_participants: Some(n1_qualified),
        expected_subjects_count: Some(expected_subjects),
        my_benchmark,
        highlights,
        available_subjects,
        available_years: Vec::new(),
        my_class_code: Some(requester_desc.code),
    }
}

/// Computes head-to-head peer comparison strictly in memory from `YearRankingDataset`
/// (Section 10.3: strict bimester intersection per common subject, median M.T., single Q_2 quantization).
pub fn build_peer_comparison_from_dataset(
    dataset: &YearRankingDataset,
    requester_account_id: i64,
    peer_account_id: i64,
    social: &RequesterSocialContext,
    bimester: Option<i16>,
    peer_entry_override: Option<LeaderboardEntry>,
) -> Option<PeerComparisonDetail> {
    if peer_account_id == requester_account_id || social.blocked_ids.contains(&peer_account_id) {
        return None;
    }

    let peer_acc = dataset.accounts.get(&peer_account_id)?;
    if !(peer_acc.opted_in && !peer_acc.invisible) {
        return None;
    }

    let model = YearStatisticalModel::from_dataset(dataset);
    let vis_cells = collect_visible_cells(dataset);

    let peer_cells = vis_cells.get(&peer_account_id)?;
    let my_cells: BTreeMap<(String, i16), StudentCell> = dataset
        .grades_by_account
        .get(&requester_account_id)
        .map(|gs| deduplicate_student_cells(gs))
        .unwrap_or_default();

    let my_class_code = dataset
        .accounts
        .get(&requester_account_id)
        .map(|a| a.class_code.as_str())
        .unwrap_or("");

    let peer_entry = match peer_entry_override {
        Some(e) => e,
        None => {
            let base_cur =
                BaseCurricularExpectation::build(dataset, &model, &vis_cells, bimester);
            let cur = CurricularContext::finalize(&base_cur, dataset, &vis_cells, None, None);
            let all_vis_ids: Vec<i64> = dataset
                .accounts
                .iter()
                .filter_map(|(&id, a)| (!a.invisible).then_some(id))
                .collect();
            let t_star = resolve_momentum_terminal_bimester(
                dataset,
                &vis_cells,
                &all_vis_ids,
                &cur,
                bimester,
                None,
                None,
            );
            let distinct_classes = base_cur.class_descs.len() >= 2;
            let peer_eval = evaluate_student_v7(
                peer_cells,
                &peer_acc.class_code,
                &peer_acc.class_desc,
                &model,
                &cur,
                bimester,
                None,
                None,
                distinct_classes,
                t_star,
            )?;
            build_sortable_entry(
                dataset,
                peer_acc,
                peer_eval,
                requester_account_id,
                social,
                RankingSortBy::PersonalAverage,
            )
            .entry
        }
    };

    // Group active cells by normalized_subject -> bimester -> StudentCell.
    let mut my_by_subj: BTreeMap<&str, BTreeMap<i16, &StudentCell>> = BTreeMap::new();
    for ((norm_subj, bim), cell) in &my_cells {
        if bimester.is_none_or(|b0| *bim == b0) {
            my_by_subj
                .entry(norm_subj.as_str())
                .or_default()
                .insert(*bim, cell);
        }
    }

    let mut peer_by_subj: BTreeMap<&str, BTreeMap<i16, &StudentCell>> = BTreeMap::new();
    for ((norm_subj, bim), cell) in peer_cells {
        if bimester.is_none_or(|b0| *bim == b0) {
            peer_by_subj
                .entry(norm_subj.as_str())
                .or_default()
                .insert(*bim, cell);
        }
    }

    let mut all_subjs: BTreeSet<&str> = BTreeSet::new();
    all_subjs.extend(my_by_subj.keys().copied());
    all_subjs.extend(peer_by_subj.keys().copied());

    let mt_avg_for_bims = |class_code: &str, norm_subj: &str, bims: &[i16]| -> Option<f64> {
        if !is_valid_class_code(class_code) {
            return None;
        }
        let mut sum = 0.0_f64;
        let mut cnt = 0usize;
        for &b in bims {
            if let Some(&mt) =
                model
                    .mt_consensus
                    .get(&(class_code.to_string(), norm_subj.to_string(), b))
            {
                sum += mt;
                cnt += 1;
            }
        }
        (cnt > 0).then(|| round2(sum / (cnt as f64)))
    };

    let mut my_wins = 0usize;
    let mut peer_wins = 0usize;
    let mut ties = 0usize;
    let mut subjects = Vec::with_capacity(all_subjs.len());

    for norm_subj in all_subjs {
        let my_map = my_by_subj.get(norm_subj);
        let peer_map = peer_by_subj.get(norm_subj);

        let fallback_name = my_map
            .and_then(|m| m.values().next().map(|c| c.subject_name.as_str()))
            .or_else(|| peer_map.and_then(|m| m.values().next().map(|c| c.subject_name.as_str())))
            .unwrap_or(norm_subj);
        let subject_name = dataset
            .canonical_subject_names
            .get(norm_subj)
            .cloned()
            .unwrap_or_else(|| fallback_name.to_string());

        let common_bims: Vec<i16> = match (my_map, peer_map) {
            (Some(mm), Some(pm)) => mm
                .keys()
                .copied()
                .filter(|b| pm.contains_key(b))
                .collect(),
            _ => Vec::new(),
        };

        if !common_bims.is_empty()
            && let (Some(mm), Some(pm)) = (my_map, peer_map)
        {
            let n = common_bims.len() as f64;
            let my_raw = common_bims.iter().map(|b| mm[b].grade).sum::<f64>() / n;
            let peer_raw = common_bims.iter().map(|b| pm[b].grade).sum::<f64>() / n;
            let my_abs: i64 = common_bims.iter().map(|b| mm[b].absences).sum();
            let peer_abs: i64 = common_bims.iter().map(|b| pm[b].absences).sum();

            let my_mt = mt_avg_for_bims(my_class_code, norm_subj, &common_bims);
            let peer_mt = mt_avg_for_bims(&peer_acc.class_code, norm_subj, &common_bims);

            // Single quantization Q_2(diff_raw) without double rounding (Section 10.3.4).
            let q = quantize_k(my_raw - peer_raw, 2);
            let delta = if q == 0 { 0.0 } else { (q as f64) / 100.0 };
            if q > 5 {
                my_wins += 1;
            } else if q < -5 {
                peer_wins += 1;
            } else {
                ties += 1;
            }

            subjects.push(SubjectComparisonRow {
                subject_name,
                my_average: Some(round2(my_raw)),
                peer_average: Some(round2(peer_raw)),
                class_average: my_mt.or(peer_mt),
                delta_me_vs_peer: Some(delta),
                my_absences: my_abs,
                peer_absences: peer_abs,
                is_common: Some(true),
                my_class_average: my_mt,
                peer_class_average: peer_mt,
            });
        } else {
            let (my_avg, my_abs, my_mt) = match my_map {
                Some(mm) if !mm.is_empty() => {
                    let bims: Vec<i16> = mm.keys().copied().collect();
                    let avg = mm.values().map(|c| c.grade).sum::<f64>() / (mm.len() as f64);
                    let abs = mm.values().map(|c| c.absences).sum();
                    let mt = mt_avg_for_bims(my_class_code, norm_subj, &bims);
                    (Some(round2(avg)), abs, mt)
                }
                _ => (None, 0, None),
            };
            let (peer_avg, peer_abs, peer_mt) = match peer_map {
                Some(pm) if !pm.is_empty() => {
                    let bims: Vec<i16> = pm.keys().copied().collect();
                    let avg = pm.values().map(|c| c.grade).sum::<f64>() / (pm.len() as f64);
                    let abs = pm.values().map(|c| c.absences).sum();
                    let mt = mt_avg_for_bims(&peer_acc.class_code, norm_subj, &bims);
                    (Some(round2(avg)), abs, mt)
                }
                _ => (None, 0, None),
            };

            subjects.push(SubjectComparisonRow {
                subject_name,
                my_average: my_avg,
                peer_average: peer_avg,
                class_average: my_mt.or(peer_mt),
                delta_me_vs_peer: None,
                my_absences: my_abs,
                peer_absences: peer_abs,
                is_common: Some(false),
                my_class_average: my_mt,
                peer_class_average: peer_mt,
            });
        }
    }

    subjects.sort_by(|a, b| {
        b.is_common
            .unwrap_or(false)
            .cmp(&a.is_common.unwrap_or(false))
            .then_with(|| {
                nsac_core::filters::normalize_text(&a.subject_name)
                    .cmp(&nsac_core::filters::normalize_text(&b.subject_name))
            })
    });

    Some(PeerComparisonDetail {
        peer_entry,
        my_wins,
        peer_wins,
        ties,
        subjects,
    })
}

/// Versioned avatar path: a new photo gets a new URL, so clients may cache it forever.
pub fn avatar_path(account_id: i64, version: i64) -> String {
    format!("/api/v1/profiles/{account_id}/avatar?v={version}")
}

/// Derives a clean, human-readable official name from an institutional `@unesp.br` email,
/// stripping trailing numeric collision suffixes (`oliveira16` -> `Oliveira`), splitting
/// hyphenated surnames (`silva-rodrigues` -> `Silva Rodrigues`), and formatting middle initials
/// (`walther.vb.franco` -> `Walther V. B. Franco`, `g.ullrich16` -> `G. Ullrich`).
pub(crate) fn derive_official_name_from_email(email: &str) -> String {
    const PREPOSITIONS: [&str; 6] = ["de", "da", "do", "dos", "das", "e"];
    let local = email.split('@').next().unwrap_or("").trim();
    let raw_parts: Vec<&str> = local
        .split(['.', '_', '-'])
        .map(|p| p.trim_end_matches(|c: char| c.is_ascii_digit()))
        .filter(|p| !p.is_empty())
        .collect();

    if raw_parts.is_empty() {
        return String::new();
    }

    let total = raw_parts.len();
    let mut out: Vec<String> = Vec::with_capacity(total + 1);

    for (idx, part) in raw_parts.into_iter().enumerate() {
        let lower = part.to_lowercase();
        let is_middle = idx > 0 && idx + 1 < total;
        let char_count = lower.chars().count();

        if is_middle && PREPOSITIONS.contains(&lower.as_str()) {
            out.push(lower);
            continue;
        }

        // Single-letter initial (e.g., `g` in `g.ullrich` or `leonardo.g.anjos`) -> `G.`
        if char_count == 1 {
            out.push(format!("{}.", lower.to_uppercase()));
            continue;
        }

        // Two-letter consonant-only cluster (e.g., `ds` in `ds.batista`, `vb` in `walther.vb.franco`,
        // `ls` in `thayla.ls.bitar`) -> `D. S.` / `V. B.` / `L. S.`
        let has_vowel = lower.chars().any(|c| {
            matches!(
                c,
                'a' | 'e'
                    | 'i'
                    | 'o'
                    | 'u'
                    | 'y'
                    | 'á'
                    | 'é'
                    | 'í'
                    | 'ó'
                    | 'ú'
                    | 'â'
                    | 'ê'
                    | 'ô'
                    | 'ã'
                    | 'õ'
            )
        });
        if char_count == 2 && !has_vowel && idx + 1 < total {
            for ch in lower.chars() {
                out.push(format!("{}.", ch.to_uppercase()));
            }
            continue;
        }

        let mut chars = lower.chars();
        if let Some(first) = chars.next() {
            out.push(format!("{}{}", first.to_uppercase(), chars.as_str()));
        }
    }

    out.join(" ")
}

/// Returns `true` when the institutional email is abbreviated (`g.ullrich`, `ds.batista`),
/// contains numeric collision suffixes (`leticia.oliveira16`), or has intermediate ambiguous
/// structures (middle initials like `walther.vb.franco` / `leonardo.g.anjos`, or hyphenated
/// compound surnames where inference may be incomplete). Clean `first.last@unesp.br` emails
/// return `false` (no intrusive modal, though Google confirmation remains available in Profile).
pub(crate) fn email_name_needs_verification(email: &str) -> bool {
    let local = email.split('@').next().unwrap_or("").trim();
    let parts: Vec<&str> = local
        .split(['.', '_', '-'])
        .filter(|p| !p.is_empty())
        .collect();
    let Some(first) = parts.first() else {
        return true;
    };
    let first_clean = first.trim_end_matches(|c: char| c.is_ascii_digit());
    if first_clean.chars().count() <= 2 {
        return true;
    }
    if local.chars().any(|c| c.is_ascii_digit()) {
        return true;
    }
    // Intermediate ambiguity: middle initial(s) (1-2 chars in middle) or hyphenated/multi-part surnames
    if local.contains('-') || parts.len() >= 3 {
        return true;
    }
    parts.iter().any(|p| p.chars().count() <= 2)
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

    let derived = derive_official_name_from_email(email);
    if derived.is_empty() {
        format!("Aluno #{account_id}")
    } else {
        derived
    }
}

// ============================================================================
// Classroom Ranking ("Ranking das Salas") — Pure in-memory aggregation over
// official NSAC class averages (M.T.) from `YearRankingDataset`.
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ClassroomRankingScope {
    All,
    GradeLevel,
    Course,
    Integrado,
    Noturno,
}

impl ClassroomRankingScope {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "all" | "school" | "cti" => Some(Self::All),
            "grade_level" | "gradelevel" | "year_level" => Some(Self::GradeLevel),
            "course" => Some(Self::Course),
            "integrado" => Some(Self::Integrado),
            "noturno" => Some(Self::Noturno),
            _ => None,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::GradeLevel => "grade_level",
            Self::Course => "course",
            Self::Integrado => "integrado",
            Self::Noturno => "noturno",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ClassroomRankingSortBy {
    OfficialAverage,
    RawAverage,
    Momentum,
    Consistency,
}

impl ClassroomRankingSortBy {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "official_average" | "average" | "media" | "score_cti" => Some(Self::OfficialAverage),
            "raw_average" | "raw" | "media_bruta" => Some(Self::RawAverage),
            "momentum" | "evolucao" => Some(Self::Momentum),
            "consistency" | "consistencia" => Some(Self::Consistency),
            _ => None,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OfficialAverage => "official_average",
            Self::RawAverage => "raw_average",
            Self::Momentum => "momentum",
            Self::Consistency => "consistency",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ClassroomRankingQuery {
    pub scope: ClassroomRankingScope,
    pub sort_by: ClassroomRankingSortBy,
    pub bimester: Option<i16>,
    pub subject_filter: Option<String>,
    pub common_only: Option<bool>,
    pub compare_class_code: Option<String>,
}

impl Default for ClassroomRankingQuery {
    fn default() -> Self {
        Self {
            scope: ClassroomRankingScope::All,
            sort_by: ClassroomRankingSortBy::OfficialAverage,
            bimester: None,
            subject_filter: None,
            common_only: None,
            compare_class_code: None,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ClassroomSubjectHighlight {
    pub name: String,
    pub average: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ClassroomLeaderboardEntry {
    pub rank: usize,
    pub class_code: String,
    pub is_my_class: bool,
    pub course_family: CourseFamily,
    pub modality: Modality,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub grade_level: Option<u8>,
    pub official_average: f64,
    pub subjects_count: i64,
    pub posted_bimesters_count: i64,
    pub active_students_count: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_provisional: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub coverage_ratio: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub momentum_delta: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub consistency_stddev: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_tied: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub best_subject: Option<ClassroomSubjectHighlight>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hardest_subject: Option<ClassroomSubjectHighlight>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub calibrated_score: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub consistency_score: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ClassroomSubjectComparisonRow {
    pub subject_name: String,
    pub my_class_average: Option<f64>,
    pub peer_class_average: Option<f64>,
    pub delta: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_common: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ClassroomComparisonDetail {
    pub my_class_entry: ClassroomLeaderboardEntry,
    pub peer_class_entry: ClassroomLeaderboardEntry,
    pub my_wins: usize,
    pub peer_wins: usize,
    pub ties: usize,
    pub subjects: Vec<ClassroomSubjectComparisonRow>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ClassroomRankingResponse {
    pub scope: String,
    pub sort_by: String,
    pub scope_label: String,
    pub school_year: i32,
    pub bimester: Option<i16>,
    pub subject_filter: Option<String>,
    pub common_only: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub common_subjects: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub common_subjects_note: Option<String>,
    pub total_classes: usize,
    pub qualified_classes: usize,
    pub expected_subjects_count: usize,
    pub my_class_code: Option<String>,
    pub my_class_rank: Option<usize>,
    pub my_class_percentile: Option<f64>,
    pub my_class_entry: Option<ClassroomLeaderboardEntry>,
    pub leaderboard: Vec<ClassroomLeaderboardEntry>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub highlights: Vec<CohortHighlightItem>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub available_subjects: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub available_years: Vec<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub class_comparison: Option<ClassroomComparisonDetail>,
}

fn class_in_scope(
    desc: &ClassDescriptor,
    requester_desc: &ClassDescriptor,
    scope: ClassroomRankingScope,
) -> bool {
    match scope {
        ClassroomRankingScope::All => true,
        ClassroomRankingScope::GradeLevel => match requester_desc.grade_level {
            Some(lvl) => desc.grade_level == Some(lvl),
            None => desc.code.eq_ignore_ascii_case(&requester_desc.code),
        },
        ClassroomRankingScope::Course => {
            if requester_desc.course_family != CourseFamily::Other {
                desc.course_family == requester_desc.course_family
            } else {
                let p = requester_desc.prefix3();
                !p.is_empty() && desc.prefix3() == p
            }
        }
        ClassroomRankingScope::Integrado => desc.modality == Modality::Integrado,
        ClassroomRankingScope::Noturno => desc.modality == Modality::Noturno,
    }
}

fn classroom_scope_label_pt(
    scope: ClassroomRankingScope,
    requester_desc: &ClassDescriptor,
) -> String {
    match scope {
        ClassroomRankingScope::All => "Todo o CTI".to_string(),
        ClassroomRankingScope::GradeLevel => match requester_desc.grade_level {
            Some(lvl) => format!("{lvl}º Ano • Todas as salas"),
            None => "Mesmo Ano".to_string(),
        },
        ClassroomRankingScope::Course => requester_desc.course_family.label_pt().to_string(),
        ClassroomRankingScope::Integrado => "Ensino Integrado".to_string(),
        ClassroomRankingScope::Noturno => "Técnico Noturno".to_string(),
    }
}

struct ClassroomInfoRaw {
    class_code: String,
    desc: ClassDescriptor,
    active_students_count: i64,
    /// `(normalized_subject, bimester)` -> median official `M_hat_{c,s,b}` (Section 2.1).
    official_cells: BTreeMap<(String, i16), f64>,
}

#[derive(Debug, Clone)]
struct ClassroomEval {
    official_average_raw: f64,
    official_average_disp: f64,
    s_avg_sala: f64,
    cells_count: usize,
    subjects_count: usize,
    posted_bimesters_count: i64,
    cov_2d_sala: f64,
    is_qualified_base: bool,
    momentum_delta_disp: Option<f64>,
    tilde_delta_mom: Option<f64>,
    cov_mom: f64,
    is_qualified_mom: bool,
    consistency_score: Option<f64>,
    consistency_stddev_disp: Option<f64>,
    consistency_stddev_raw: Option<f64>,
    best_subject: Option<ClassroomSubjectHighlight>,
    hardest_subject: Option<ClassroomSubjectHighlight>,
}

/// Collects visible classrooms in `scope` using exclusively `!acc.invisible` accounts and median
/// `M_hat_{c,s,b}` from `YearStatisticalModel` (Sections 1.1 & 8).
fn collect_visible_classrooms(
    dataset: &YearRankingDataset,
    model: &YearStatisticalModel,
    vis_cells: &HashMap<i64, BTreeMap<(String, i16), StudentCell>>,
    requester_desc: &ClassDescriptor,
    scope: ClassroomRankingScope,
) -> (HashMap<String, ClassroomInfoRaw>, Vec<i64>) {
    let mut active_students_by_class: HashMap<String, i64> = HashMap::new();
    let mut class_descs: HashMap<String, ClassDescriptor> = HashMap::new();
    let mut scope_vis_accts: Vec<i64> = Vec::new();

    for (&acct_id, acc) in &dataset.accounts {
        if acc.invisible || !is_valid_class_code(&acc.class_code) {
            continue;
        }
        if !class_in_scope(&acc.class_desc, requester_desc, scope) {
            continue;
        }
        scope_vis_accts.push(acct_id);
        class_descs
            .entry(acc.class_code.clone())
            .or_insert_with(|| acc.class_desc.clone());
        // N_c^{alunos} = visible accounts in class c with non-empty grades in the year (Section 8).
        if vis_cells.contains_key(&acct_id) {
            *active_students_by_class
                .entry(acc.class_code.clone())
                .or_insert(0) += 1;
        }
    }

    let mut cells_by_class: HashMap<String, BTreeMap<(String, i16), f64>> = HashMap::new();
    for ((c, s, b), &mt) in &model.mt_consensus {
        if !class_descs.contains_key(c) {
            continue;
        }
        cells_by_class
            .entry(c.clone())
            .or_default()
            .insert((s.clone(), *b), mt);
    }

    let mut out: HashMap<String, ClassroomInfoRaw> = HashMap::new();
    for (class_code, official_cells) in cells_by_class {
        if official_cells.is_empty() {
            continue;
        }
        let Some(desc) = class_descs.get(&class_code).cloned() else {
            continue;
        };
        let active_students_count = active_students_by_class
            .get(&class_code)
            .copied()
            .unwrap_or(0);
        out.insert(
            class_code.clone(),
            ClassroomInfoRaw {
                class_code,
                desc,
                active_students_count,
                official_cells,
            },
        );
    }

    (out, scope_vis_accts)
}

#[allow(clippy::too_many_arguments)]
fn evaluate_classroom_v7(
    raw: &ClassroomInfoRaw,
    canonical_names: &HashMap<String, String>,
    model: &YearStatisticalModel,
    curricular: &CurricularContext,
    bimester: Option<i16>,
    subject_filter_norm: Option<&str>,
    common_set: Option<&HashSet<String>>,
    multi_class_mode: bool,
    t_star: Option<i16>,
) -> Option<ClassroomEval> {
    let active: Vec<(&str, i16, f64)> = raw
        .official_cells
        .iter()
        .filter(|((s, b), _)| {
            bimester.is_none_or(|b0| *b == b0)
                && matches_subject_filter(s, subject_filter_norm)
                && matches_common_set(s, common_set)
        })
        .map(|((s, b), &mt)| (s.as_str(), *b, mt))
        .collect();

    if active.is_empty() {
        return None;
    }

    let info = curricular.for_class(&raw.class_code, &raw.desc);
    let m_c = active.len();
    let quorum_ok =
        (raw.active_students_count as usize) >= min_students_for_classroom(raw.desc.modality);

    let (cov_2d_sala, cov_ok) =
        compute_2d_coverage(active.iter().map(|&(s, b, _)| (s, b)), &info);
    let is_qualified_base = quorum_ok && cov_ok;

    let theta_c = model.theta_c.get(&raw.class_code).copied().unwrap_or(0.0);
    let mut b_obs: HashSet<i16> = HashSet::new();
    let mut by_subj: HashMap<&str, (f64, usize)> = HashMap::new();
    let mut mt_sum = 0.0_f64;
    let mut m_tilde_sum = 0.0_f64;
    let mut shared_cells: Vec<(&str, i16, f64)> = Vec::new();
    let mut shared_subjs: HashSet<&str> = HashSet::new();

    for &(norm_subj, bim, mt) in &active {
        b_obs.insert(bim);
        mt_sum += mt;
        let slot = by_subj.entry(norm_subj).or_insert((0.0, 0));
        slot.0 += mt;
        slot.1 += 1;

        let class_cnt = model
            .class_count_for_cell
            .get(&(norm_subj.to_string(), bim))
            .copied()
            .unwrap_or(0);
        if class_cnt >= 2 {
            let delta = model
                .delta_sb
                .get(&(norm_subj.to_string(), bim))
                .copied()
                .unwrap_or(0.0);
            let gamma = raw
                .desc
                .grade_level
                .and_then(|l| model.gamma_sl.get(&(norm_subj.to_string(), l)).copied())
                .unwrap_or(0.0);
            let cal = mt - delta - gamma;
            m_tilde_sum += cal;
            shared_cells.push((norm_subj, bim, cal));
            shared_subjs.insert(norm_subj);
        } else {
            m_tilde_sum += model.mu_0 + theta_c;
        }
    }

    let subjects_count = by_subj.len();
    let m_bar_c = mt_sum / (m_c as f64);
    let m_tilde_c = m_tilde_sum / (m_c as f64);

    // Anti-Simpson + disconnected classroom protection + single-subject bypass (Section 8.2).
    let f_comp = (shared_cells.len() as f64) / (m_c as f64);
    let w_cal = (f_comp / 0.25).min(1.0);
    let m_ativo = if multi_class_mode && subject_filter_norm.is_none() {
        w_cal * m_tilde_c + (1.0 - w_cal) * m_bar_c
    } else {
        m_bar_c
    };

    let eta_sala = SHRINK_ETA_Z * S0_CLASSROOM;
    let denom_b = 1usize.max(info.b_exp.len()).max(b_obs.len()) as f64;
    let n_c_eq = ((m_c as f64) / denom_b).min(info.e_subj_eff as f64);
    let s_avg_sala =
        model.mu_0 + phi_shrink(m_ativo - model.mu_0, n_c_eq, cov_2d_sala, SHRINK_M, eta_sala);

    // Classroom Consistency via CARA (BETA_CLASSROOM = 2.00, Section 8.3).
    let (consistency_score, consistency_stddev_disp, consistency_stddev_raw) = if multi_class_mode {
        if shared_subjs.len() >= 4 {
            let v_c: Vec<f64> = shared_cells.iter().map(|c| c.2).collect();
            let b_disp: HashSet<i16> = shared_cells.iter().map(|c| c.1).collect();
            let denom_bd = 1usize.max(info.b_exp.len()).max(b_disp.len()) as f64;
            let n_disp_eq = ((v_c.len() as f64) / denom_bd).min(info.e_subj_eff as f64);
            let sd_raw = cell_stddev(&v_c);
            let score = cara_certainty_equivalent(&v_c, BETA_CLASSROOM).map(|cara| {
                let c_tilde = model.mu_0
                    + phi_shrink(
                        cara - model.mu_0,
                        n_disp_eq,
                        cov_2d_sala,
                        SHRINK_M,
                        eta_sala,
                    );
                psi_smooth_floor(c_tilde)
            });
            (score, Some(round2(sd_raw)), Some(sd_raw))
        } else {
            // Section 8.3: strictly G_3 (NO raw fallback in multi-class mode).
            (None, None, None)
        }
    } else if subjects_count >= 4 {
        let v_c: Vec<f64> = active.iter().map(|c| c.2).collect();
        let sd_raw = cell_stddev(&v_c);
        let score = cara_certainty_equivalent(&v_c, BETA_CLASSROOM).map(|cara| {
            let c_tilde =
                model.mu_0 + phi_shrink(cara - model.mu_0, n_c_eq, cov_2d_sala, SHRINK_M, eta_sala);
            psi_smooth_floor(c_tilde)
        });
        (score, Some(round2(sd_raw)), Some(sd_raw))
    } else {
        (None, None, None)
    };

    // Classroom Momentum aligned with shared terminal bimester t* (Section 8.4).
    let k_min_mom = momentum_k_min(bimester, subject_filter_norm.is_some());
    let (momentum_delta_disp, tilde_delta_mom, cov_mom, is_qualified_mom) = if let Some(ts) = t_star
    {
        let mut prev_set: HashSet<&str> = HashSet::new();
        let mut curr_set: HashSet<&str> = HashSet::new();
        for (s, b) in raw.official_cells.keys() {
            if !matches_subject_filter(s, subject_filter_norm) || !matches_common_set(s, common_set)
            {
                continue;
            }
            if *b == ts - 1 {
                prev_set.insert(s.as_str());
            } else if *b == ts {
                curr_set.insert(s.as_str());
            }
        }
        let paired_subjs: Vec<&str> = prev_set.intersection(&curr_set).copied().collect();
        let cov_m = compute_momentum_cov(&paired_subjs, &info);
        if paired_subjs.len() < k_min_mom {
            (None, None, cov_m, false)
        } else {
            let mut diff_sum = 0.0_f64;
            let mut valid = 0usize;
            for &s in &paired_subjs {
                if let (Some(&curr), Some(&prev)) = (
                    raw.official_cells.get(&(s.to_string(), ts)),
                    raw.official_cells.get(&(s.to_string(), ts - 1)),
                ) {
                    diff_sum += curr - prev;
                    valid += 1;
                }
            }
            if valid < k_min_mom {
                (None, None, cov_m, false)
            } else {
                let raw_mom = diff_sum / (valid as f64);
                let tilde_m = phi_shrink(raw_mom, valid as f64, cov_m, SHRINK_M, eta_sala);
                let qual_m =
                    quorum_ok && valid >= k_min_mom && cov_m >= info.tau_cov_mom() - 1e-12;
                (Some(round2(raw_mom)), Some(tilde_m), cov_m, qual_m)
            }
        }
    } else {
        (None, None, 0.0, false)
    };

    // Per-classroom best_subject & hardest_subject display highlights.
    let mut subj_vals: Vec<(String, f64)> = by_subj
        .into_iter()
        .filter(|(_, (_, cnt))| *cnt > 0)
        .map(|(norm_s, (sum, cnt))| {
            let canonical = canonical_names
                .get(norm_s)
                .cloned()
                .unwrap_or_else(|| norm_s.to_string());
            (canonical, sum / (cnt as f64))
        })
        .collect();
    subj_vals.sort_by(|a, b| a.0.cmp(&b.0));

    let best_subject = subj_vals
        .iter()
        .max_by(|a, b| quantize_k(a.1, 4).cmp(&quantize_k(b.1, 4)).then_with(|| b.0.cmp(&a.0)))
        .map(|(name, avg)| ClassroomSubjectHighlight {
            name: name.clone(),
            average: round2(*avg),
        });

    let hardest_subject = if subj_vals.len() >= 2 {
        subj_vals
            .iter()
            .min_by(|a, b| quantize_k(a.1, 4).cmp(&quantize_k(b.1, 4)).then_with(|| a.0.cmp(&b.0)))
            .map(|(name, avg)| ClassroomSubjectHighlight {
                name: name.clone(),
                average: round2(*avg),
            })
    } else {
        None
    };

    Some(ClassroomEval {
        official_average_raw: m_bar_c,
        official_average_disp: round2(m_bar_c),
        s_avg_sala,
        cells_count: m_c,
        subjects_count,
        posted_bimesters_count: b_obs.len() as i64,
        cov_2d_sala,
        is_qualified_base,
        momentum_delta_disp,
        tilde_delta_mom,
        cov_mom,
        is_qualified_mom,
        consistency_score,
        consistency_stddev_disp,
        consistency_stddev_raw,
        best_subject,
        hardest_subject,
    })
}

#[derive(Debug, Clone)]
struct SortableClassroomEntry {
    entry: ClassroomLeaderboardEntry,
    tier: MetricTier,
    eval: ClassroomEval,
}

fn classroom_tier_and_primary_score(
    eval: &ClassroomEval,
    sort_by: ClassroomRankingSortBy,
) -> (MetricTier, Option<f64>, f64) {
    match sort_by {
        ClassroomRankingSortBy::OfficialAverage => {
            let tier = if eval.is_qualified_base {
                MetricTier::G1
            } else {
                MetricTier::G2
            };
            (tier, Some(eval.s_avg_sala), eval.cov_2d_sala)
        }
        ClassroomRankingSortBy::RawAverage => {
            let tier = if eval.is_qualified_base {
                MetricTier::G1
            } else {
                MetricTier::G2
            };
            (tier, Some(eval.official_average_raw), eval.cov_2d_sala)
        }
        ClassroomRankingSortBy::Momentum => match eval.tilde_delta_mom {
            None => (MetricTier::G3, None, eval.cov_mom),
            Some(tm) => {
                let tier = if eval.is_qualified_mom {
                    MetricTier::G1
                } else {
                    MetricTier::G2
                };
                (tier, Some(tm), eval.cov_mom)
            }
        },
        ClassroomRankingSortBy::Consistency => match eval.consistency_score {
            None => (MetricTier::G3, None, eval.cov_2d_sala),
            Some(sc) => {
                let tier = if eval.is_qualified_base {
                    MetricTier::G1
                } else {
                    MetricTier::G2
                };
                (tier, Some(sc), eval.cov_2d_sala)
            }
        },
    }
}

fn compare_sortable_classrooms(
    a: &SortableClassroomEntry,
    b: &SortableClassroomEntry,
    sort_by: ClassroomRankingSortBy,
) -> Ordering {
    let tier_ord = a.tier.cmp(&b.tier);
    if tier_ord != Ordering::Equal {
        return tier_ord;
    }

    if a.tier == MetricTier::G3 {
        return quantize_k(b.eval.s_avg_sala, 4)
            .cmp(&quantize_k(a.eval.s_avg_sala, 4))
            .then_with(|| a.entry.class_code.cmp(&b.entry.class_code));
    }

    match sort_by {
        ClassroomRankingSortBy::OfficialAverage => quantize_k(b.eval.s_avg_sala, 4)
            .cmp(&quantize_k(a.eval.s_avg_sala, 4))
            .then_with(|| b.eval.cells_count.cmp(&a.eval.cells_count))
            .then_with(|| b.eval.subjects_count.cmp(&a.eval.subjects_count))
            .then_with(|| a.entry.class_code.cmp(&b.entry.class_code)),
        ClassroomRankingSortBy::RawAverage => quantize_k(b.eval.official_average_raw, 4)
            .cmp(&quantize_k(a.eval.official_average_raw, 4))
            .then_with(|| quantize_k(b.eval.s_avg_sala, 4).cmp(&quantize_k(a.eval.s_avg_sala, 4)))
            .then_with(|| b.eval.cells_count.cmp(&a.eval.cells_count))
            .then_with(|| b.eval.subjects_count.cmp(&a.eval.subjects_count))
            .then_with(|| a.entry.class_code.cmp(&b.entry.class_code)),
        ClassroomRankingSortBy::Momentum => b
            .eval
            .tilde_delta_mom
            .map(|m| quantize_k(m, 4))
            .cmp(&a.eval.tilde_delta_mom.map(|m| quantize_k(m, 4)))
            .then_with(|| {
                quantize_k(b.eval.s_avg_sala, 4).cmp(&quantize_k(a.eval.s_avg_sala, 4))
            })
            .then_with(|| a.entry.class_code.cmp(&b.entry.class_code)),
        ClassroomRankingSortBy::Consistency => b
            .eval
            .consistency_score
            .map(|c| quantize_k(c, 4))
            .cmp(&a.eval.consistency_score.map(|c| quantize_k(c, 4)))
            .then_with(|| {
                let asd = a.eval.consistency_stddev_raw.map(|s| quantize_k(s, 2));
                let bsd = b.eval.consistency_stddev_raw.map(|s| quantize_k(s, 2));
                asd.cmp(&bsd)
            })
            .then_with(|| {
                quantize_k(b.eval.s_avg_sala, 4).cmp(&quantize_k(a.eval.s_avg_sala, 4))
            })
            .then_with(|| a.entry.class_code.cmp(&b.entry.class_code)),
    }
}

fn assign_classroom_ranks(
    items: &mut [SortableClassroomEntry],
    sort_by: ClassroomRankingSortBy,
) {
    let mut prev_key: Option<(MetricTier, i64)> = None;
    let mut prev_rank = 1usize;

    for idx in 0..items.len() {
        if items[idx].tier == MetricTier::G3 {
            items[idx].entry.rank = idx + 1;
            items[idx].entry.is_tied = None;
            prev_key = None;
            continue;
        }
        let (_, score, _) = classroom_tier_and_primary_score(&items[idx].eval, sort_by);
        let key = score.map(|s| (items[idx].tier, quantize_k(s, 4)));
        if idx == 0 || prev_key.is_none() {
            let new_rank = idx + 1;
            items[idx].entry.rank = new_rank;
            prev_rank = new_rank;
        } else if key.is_some() && key == prev_key {
            items[idx].entry.rank = prev_rank;
            items[idx - 1].entry.is_tied = Some(true);
            items[idx].entry.is_tied = Some(true);
        } else {
            let new_rank = idx + 1;
            items[idx].entry.rank = new_rank;
            prev_rank = new_rank;
        }
        prev_key = key;
    }
}

/// Resolves opt-in `common_set` for `ClassroomRanking` per Section 9.5.3.
fn resolve_classroom_common_subjects(
    raw_by_class: &HashMap<String, ClassroomInfoRaw>,
    base_curricular: &BaseCurricularExpectation,
    canonical_names: &HashMap<String, String>,
    bimester: Option<i16>,
    subject_filter_norm: Option<&str>,
    common_only_requested: bool,
) -> CommonCoreResolution {
    let inactive = |note: Option<String>| CommonCoreResolution {
        effective_common_only: false,
        common_set: None,
        common_subjects_names: None,
        note,
    };

    if !common_only_requested || subject_filter_norm.is_some() {
        return inactive(None);
    }

    let mut subjects_by_class: HashMap<String, HashSet<String>> = HashMap::new();
    for (code, raw) in raw_by_class {
        let mut set = HashSet::new();
        for (norm_subj, bim) in raw.official_cells.keys() {
            if bimester.is_none_or(|b| *bim == b) {
                set.insert(norm_subj.clone());
            }
        }
        if !set.is_empty() {
            subjects_by_class.insert(code.clone(), set);
        }
    }

    if subjects_by_class.len() < 2 {
        return inactive(None);
    }

    let mature_classes: Vec<&String> = subjects_by_class
        .iter()
        .filter_map(|(c, s_obs)| {
            let e_base = base_curricular.e_subj_base.get(c).copied().unwrap_or(1);
            let thresh = e_base.div_ceil(2).max(2);
            (s_obs.len() >= thresh).then_some(c)
        })
        .collect();

    let pool_sets: Vec<&HashSet<String>> = if mature_classes.len() >= 2 {
        mature_classes
            .iter()
            .filter_map(|c| subjects_by_class.get(*c))
            .collect()
    } else {
        subjects_by_class.values().collect()
    };

    if pool_sets.len() < 2 {
        return inactive(None);
    }

    let s_all = intersect_subject_sets(&pool_sets);
    let passes_anti_expulsion = s_all.len() >= 3
        && subjects_by_class.values().all(|s_obs| {
            let shared = s_all.intersection(s_obs).count();
            shared >= 3usize.min(s_obs.len())
        });

    if !passes_anti_expulsion {
        let has_noturno = subjects_by_class
            .keys()
            .any(|c| ClassDescriptor::parse(c).modality == Modality::Noturno);
        let note = if has_noturno {
            "Turmas do noturno têm grade técnica diferente da BNCC; exibindo todas as matérias com calibração inter-salas."
                .to_string()
        } else {
            INSUFFICIENT_COMMON_SUBJECTS_NOTE.to_string()
        };
        return inactive(Some(note));
    }

    let mut names: Vec<String> = s_all
        .iter()
        .map(|k| {
            canonical_names
                .get(k)
                .cloned()
                .unwrap_or_else(|| k.clone())
        })
        .collect();
    names.sort_by_key(|s| nsac_core::filters::normalize_text(s));

    CommonCoreResolution {
        effective_common_only: true,
        common_set: Some(s_all),
        common_subjects_names: Some(names),
        note: None,
    }
}

fn build_classroom_highlights(
    dataset: &YearRankingDataset,
    model: &YearStatisticalModel,
    sorted: &[SortableClassroomEntry],
    bimester: Option<i16>,
    subject_filter_norm: Option<&str>,
    common_set: Option<&HashSet<String>>,
    sort_by: ClassroomRankingSortBy,
) -> Vec<CohortHighlightItem> {
    let mut highlights = Vec::with_capacity(4);

    // 1. Leader: first element of G_1^{sala} (Section 10.1.1).
    if let Some(leader) = sorted.iter().find(|s| s.tier == MetricTier::G1) {
        let (metric_label, leader_subtext) = match sort_by {
            ClassroomRankingSortBy::OfficialAverage => {
                if let Some(cs) = leader.entry.calibrated_score {
                    (
                        format!("{cs:.2}"),
                        format!(
                            "{} • M.T. {:.2}",
                            leader.entry.course_family.label_pt(),
                            leader.entry.official_average
                        ),
                    )
                } else {
                    (
                        format!("{:.2}", leader.entry.official_average),
                        format!(
                            "{} • {} mat.",
                            leader.entry.course_family.label_pt(),
                            leader.entry.subjects_count
                        ),
                    )
                }
            }
            ClassroomRankingSortBy::RawAverage => (
                format!("{:.2}", leader.entry.official_average),
                format!(
                    "{} • {} mat.",
                    leader.entry.course_family.label_pt(),
                    leader.entry.subjects_count
                ),
            ),
            ClassroomRankingSortBy::Momentum => (
                leader
                    .entry
                    .momentum_delta
                    .map(|m| format!("{m:+.2} pts"))
                    .unwrap_or_else(|| format!("{:.2}", leader.entry.official_average)),
                format!(
                    "{} • M.T. {:.2}",
                    leader.entry.course_family.label_pt(),
                    leader.entry.official_average
                ),
            ),
            ClassroomRankingSortBy::Consistency => {
                if let Some(cs) = leader.entry.consistency_score {
                    (
                        format!("{cs:.2}"),
                        format!(
                            "{} • M.T. {:.2}",
                            leader.entry.course_family.label_pt(),
                            leader.entry.official_average
                        ),
                    )
                } else {
                    (
                        match leader.entry.consistency_stddev {
                            Some(sd) => format!("{:.2} • DP {sd:.2}", leader.entry.official_average),
                            None => format!("{:.2}", leader.entry.official_average),
                        },
                        format!(
                            "{} • {} mat.",
                            leader.entry.course_family.label_pt(),
                            leader.entry.subjects_count
                        ),
                    )
                }
            }
        };
        highlights.push(CohortHighlightItem {
            kind: "leader".to_string(),
            title: "Sala líder".to_string(),
            subject_or_label: leader.entry.class_code.clone(),
            value_text: metric_label,
            subtext: leader_subtext,
            account_id: None,
        });
    }

    // 2. Top riser: element of G_1^{mom,sala} maximizing Q_4(tilde_delta_mom) > 0.05 (Section 10.1.2).
    if let Some(riser) = sorted
        .iter()
        .filter(|s| {
            s.eval.is_qualified_mom && s.eval.tilde_delta_mom.is_some_and(|m| m > 0.05)
        })
        .max_by(|a, b| {
            let am = a.eval.tilde_delta_mom.map(|v| quantize_k(v, 4));
            let bm = b.eval.tilde_delta_mom.map(|v| quantize_k(v, 4));
            am.cmp(&bm)
                .then_with(|| {
                    quantize_k(a.eval.s_avg_sala, 4).cmp(&quantize_k(b.eval.s_avg_sala, 4))
                })
                .then_with(|| b.entry.class_code.cmp(&a.entry.class_code))
        })
        && let Some(mom) = riser.entry.momentum_delta
    {
        highlights.push(CohortHighlightItem {
            kind: "top_riser".to_string(),
            title: "Maior evolução".to_string(),
            subject_or_label: riser.entry.class_code.clone(),
            value_text: format!("+{mom:.2} pts"),
            subtext: format!("M.T. {:.2}", riser.entry.official_average),
            account_id: None,
        });
    }

    // 3. Most consistent: element of G_1^{cons,sala} maximizing Q_4(S_c^{cons,sala}) (Section 10.1.4).
    if let Some(consistent) = sorted
        .iter()
        .filter(|s| s.eval.is_qualified_base && s.eval.consistency_score.is_some())
        .max_by(|a, b| {
            let ac = a.eval.consistency_score.map(|v| quantize_k(v, 4));
            let bc = b.eval.consistency_score.map(|v| quantize_k(v, 4));
            ac.cmp(&bc)
                .then_with(|| {
                    let asd = a.eval.consistency_stddev_raw.map(|s| quantize_k(s, 2));
                    let bsd = b.eval.consistency_stddev_raw.map(|s| quantize_k(s, 2));
                    bsd.cmp(&asd)
                })
                .then_with(|| {
                    quantize_k(a.eval.s_avg_sala, 4).cmp(&quantize_k(b.eval.s_avg_sala, 4))
                })
                .then_with(|| b.entry.class_code.cmp(&a.entry.class_code))
        })
        && let Some(sd) = consistent.entry.consistency_stddev
    {
        highlights.push(CohortHighlightItem {
            kind: "most_consistent".to_string(),
            title: "Sala mais constante".to_string(),
            subject_or_label: consistent.entry.class_code.clone(),
            value_text: format!("DP ±{sd:.2}"),
            subtext: format!("M.T. {:.2}", consistent.entry.official_average),
            account_id: None,
        });
    }

    // 4. Hardest subject over G_1^{sala} (fallback to all classrooms in scope if G_1 empty, Section 10.1.5).
    let g1_classes: HashSet<String> = sorted
        .iter()
        .filter(|s| s.tier == MetricTier::G1)
        .map(|s| s.entry.class_code.clone())
        .collect();
    let ref_classes: HashSet<String> = if !g1_classes.is_empty() {
        g1_classes
    } else {
        sorted.iter().map(|s| s.entry.class_code.clone()).collect()
    };

    if let Some((hardest_norm, avg_mt)) = select_hardest_subject(
        model,
        &ref_classes,
        bimester,
        subject_filter_norm,
        common_set,
        None,
    ) {
        let canonical = dataset
            .canonical_subject_names
            .get(&hardest_norm)
            .cloned()
            .unwrap_or(hardest_norm);
        highlights.push(CohortHighlightItem {
            kind: "hardest_subject".to_string(),
            title: "Matéria mais exigente".to_string(),
            subject_or_label: canonical,
            value_text: format!("M.T. {avg_mt:.2}"),
            subtext: "Menor média oficial das salas".to_string(),
            account_id: None,
        });
    }

    highlights
}

/// Pure in-memory classroom ranking engine over `YearRankingDataset` using official M.T. consensus
/// (Specification v7, Section 8).
pub fn build_classroom_ranking(
    dataset: &YearRankingDataset,
    requester_account_id: i64,
    requester_fallback_class: Option<&str>,
    query: &ClassroomRankingQuery,
) -> ClassroomRankingResponse {
    let requester_class_str = dataset
        .accounts
        .get(&requester_account_id)
        .map(|a| a.class_code.as_str())
        .or(requester_fallback_class)
        .unwrap_or("CTI");
    let requester_desc = ClassDescriptor::parse(requester_class_str);
    let my_class_code = is_valid_class_code(&requester_desc.code).then(|| requester_desc.code.clone());

    let subject_filter_clean = query
        .subject_filter
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToString::to_string);
    let subject_filter_norm = subject_filter_clean
        .as_deref()
        .map(normalize_subject_key)
        .filter(|s| !s.is_empty());

    let model = YearStatisticalModel::from_dataset(dataset);
    let vis_cells = collect_visible_cells(dataset);
    let base_curricular =
        BaseCurricularExpectation::build(dataset, &model, &vis_cells, query.bimester);

    let (raw_by_class, scope_vis_accts) =
        collect_visible_classrooms(dataset, &model, &vis_cells, &requester_desc, query.scope);

    // Available subjects across visible classrooms in scope (before subject_filter).
    let mut avail_norm: BTreeSet<String> = BTreeSet::new();
    for raw in raw_by_class.values() {
        for (norm_subj, bim) in raw.official_cells.keys() {
            if query.bimester.is_none_or(|b| *bim == b) {
                avail_norm.insert(norm_subj.clone());
            }
        }
    }
    let mut available_subjects: Vec<String> = avail_norm
        .into_iter()
        .map(|k| {
            dataset
                .canonical_subject_names
                .get(&k)
                .cloned()
                .unwrap_or(k)
        })
        .collect();
    available_subjects.sort_by_key(|s| nsac_core::filters::normalize_text(s));

    // Global default: `common_only = false` (Section 9.5).
    let common_only_requested = query.common_only.unwrap_or(false);
    let common_res = resolve_classroom_common_subjects(
        &raw_by_class,
        &base_curricular,
        &dataset.canonical_subject_names,
        query.bimester,
        subject_filter_norm.as_deref(),
        common_only_requested,
    );

    let curricular = CurricularContext::finalize(
        &base_curricular,
        dataset,
        &vis_cells,
        subject_filter_norm.as_deref(),
        common_res.common_set.as_ref(),
    );

    // Determine effective classrooms C_eff^{sala}.
    let active_class_codes: Vec<&String> = raw_by_class
        .iter()
        .filter_map(|(code, raw)| {
            let has_active = raw.official_cells.keys().any(|(s, b)| {
                query.bimester.is_none_or(|b0| *b == b0)
                    && matches_subject_filter(s, subject_filter_norm.as_deref())
                    && matches_common_set(s, common_res.common_set.as_ref())
            });
            has_active.then_some(code)
        })
        .collect();
    let multi_class_mode = active_class_codes.len() >= 2;

    // Shared terminal bimester t* for Momentum over visible students in scope classrooms (Section 8.4).
    let t_star = resolve_momentum_terminal_bimester(
        dataset,
        &vis_cells,
        &scope_vis_accts,
        &curricular,
        query.bimester,
        subject_filter_norm.as_deref(),
        common_res.common_set.as_ref(),
    );

    let mut sortable: Vec<SortableClassroomEntry> = Vec::with_capacity(active_class_codes.len());
    for code in &active_class_codes {
        let Some(raw) = raw_by_class.get(*code) else {
            continue;
        };
        if let Some(eval) = evaluate_classroom_v7(
            raw,
            &dataset.canonical_subject_names,
            &model,
            &curricular,
            query.bimester,
            subject_filter_norm.as_deref(),
            common_res.common_set.as_ref(),
            multi_class_mode,
            t_star,
        ) {
            let (tier, _, active_cov) = classroom_tier_and_primary_score(&eval, query.sort_by);
            let is_provisional = tier != MetricTier::G1;
            let entry = ClassroomLeaderboardEntry {
                rank: 0,
                class_code: raw.class_code.clone(),
                is_my_class: my_class_code
                    .as_deref()
                    .is_some_and(|mc| mc.eq_ignore_ascii_case(&raw.class_code)),
                course_family: raw.desc.course_family,
                modality: raw.desc.modality,
                grade_level: raw.desc.grade_level,
                official_average: eval.official_average_disp,
                subjects_count: eval.subjects_count as i64,
                posted_bimesters_count: eval.posted_bimesters_count,
                active_students_count: raw.active_students_count,
                is_provisional: Some(is_provisional),
                coverage_ratio: Some(round_k(active_cov, 3)),
                momentum_delta: eval.momentum_delta_disp,
                consistency_stddev: eval.consistency_stddev_disp,
                is_tied: None,
                best_subject: eval.best_subject.clone(),
                hardest_subject: eval.hardest_subject.clone(),
                calibrated_score: multi_class_mode
                    .then(|| round2(eval.s_avg_sala.clamp(0.0, 10.0))),
                consistency_score: eval
                    .consistency_score
                    .map(|c| round2(c.clamp(0.0, 10.0))),
            };
            sortable.push(SortableClassroomEntry { entry, tier, eval });
        }
    }

    sortable.sort_by(|a, b| compare_sortable_classrooms(a, b, query.sort_by));
    assign_classroom_ranks(&mut sortable, query.sort_by);

    let total_classes = sortable.len();
    let qualified_classes = sortable
        .iter()
        .filter(|s| s.tier == MetricTier::G1)
        .count();

    let mut my_class_rank: Option<usize> = None;
    let mut my_class_tier: Option<MetricTier> = None;
    let mut my_class_entry: Option<ClassroomLeaderboardEntry> = None;
    for s in &sortable {
        if s.entry.is_my_class {
            my_class_tier = Some(s.tier);
            if s.tier != MetricTier::G3 {
                my_class_rank = Some(s.entry.rank);
            }
            my_class_entry = Some(s.entry.clone());
            break;
        }
    }

    let my_class_percentile = if my_class_tier == Some(MetricTier::G1)
        && let Some(r) = my_class_rank
    {
        let tie_count = sortable
            .iter()
            .filter(|s| s.tier == MetricTier::G1 && s.entry.rank == r)
            .count();
        compute_tied_midpoint_percentile(r, tie_count, qualified_classes)
    } else {
        None
    };

    let expected_subjects_count = if let Some(ref cs) = common_res.common_set {
        cs.len()
    } else if let Some(ref mc) = my_class_code {
        curricular.for_class(mc, &requester_desc).e_subj_eff
    } else {
        let es: Vec<usize> = active_class_codes
            .iter()
            .filter_map(|c| curricular.per_class.get(*c).map(|info| info.e_subj_eff))
            .collect();
        if es.is_empty() { 0 } else { p75_usize(&es) }
    };

    let highlights = build_classroom_highlights(
        dataset,
        &model,
        &sortable,
        query.bimester,
        subject_filter_norm.as_deref(),
        common_res.common_set.as_ref(),
        query.sort_by,
    );

    let leaderboard: Vec<ClassroomLeaderboardEntry> =
        sortable.into_iter().map(|s| s.entry).collect();

    let class_comparison = query.compare_class_code.as_deref().and_then(|peer_code| {
        let my_code = my_class_code.as_deref()?;
        if my_code.eq_ignore_ascii_case(peer_code) {
            return None;
        }
        build_classroom_comparison_from_dataset(
            dataset,
            my_code,
            peer_code,
            query.bimester,
            subject_filter_norm.as_deref(),
        )
    });

    ClassroomRankingResponse {
        scope: query.scope.as_str().to_string(),
        sort_by: query.sort_by.as_str().to_string(),
        scope_label: classroom_scope_label_pt(query.scope, &requester_desc),
        school_year: dataset.school_year,
        bimester: query.bimester,
        subject_filter: subject_filter_clean,
        common_only: common_res.effective_common_only,
        common_subjects: common_res.common_subjects_names,
        common_subjects_note: common_res.note,
        total_classes,
        qualified_classes,
        expected_subjects_count,
        my_class_code,
        my_class_rank,
        my_class_percentile,
        my_class_entry,
        leaderboard,
        highlights,
        available_subjects,
        available_years: Vec::new(),
        class_comparison,
    }
}

/// Computes head-to-head classroom comparison strictly in memory from `YearRankingDataset`
/// (Section 10.3: visible classrooms only, median M.T., strict bimester intersection, single Q_2).
pub fn build_classroom_comparison_from_dataset(
    dataset: &YearRankingDataset,
    my_class_code: &str,
    peer_class_code: &str,
    bimester: Option<i16>,
    subject_filter_norm: Option<&str>,
) -> Option<ClassroomComparisonDetail> {
    let my_desc = ClassDescriptor::parse(my_class_code);
    let peer_desc = ClassDescriptor::parse(peer_class_code);
    if my_desc.code.eq_ignore_ascii_case(&peer_desc.code)
        || !is_valid_class_code(&my_desc.code)
        || !is_valid_class_code(&peer_desc.code)
    {
        return None;
    }

    let model = YearStatisticalModel::from_dataset(dataset);
    let vis_cells = collect_visible_cells(dataset);
    let base_curricular = BaseCurricularExpectation::build(dataset, &model, &vis_cells, bimester);
    let (all_raw, all_vis_accts) = collect_visible_classrooms(
        dataset,
        &model,
        &vis_cells,
        &my_desc,
        ClassroomRankingScope::All,
    );

    let my_raw = all_raw.get(&my_desc.code)?;
    let peer_raw = all_raw.get(&peer_desc.code)?;

    let curricular = CurricularContext::finalize(
        &base_curricular,
        dataset,
        &vis_cells,
        subject_filter_norm,
        None,
    );
    let t_star = resolve_momentum_terminal_bimester(
        dataset,
        &vis_cells,
        &all_vis_accts,
        &curricular,
        bimester,
        subject_filter_norm,
        None,
    );
    let multi_class_mode = all_raw.len() >= 2;

    let my_eval = evaluate_classroom_v7(
        my_raw,
        &dataset.canonical_subject_names,
        &model,
        &curricular,
        bimester,
        subject_filter_norm,
        None,
        multi_class_mode,
        t_star,
    )?;
    let peer_eval = evaluate_classroom_v7(
        peer_raw,
        &dataset.canonical_subject_names,
        &model,
        &curricular,
        bimester,
        subject_filter_norm,
        None,
        multi_class_mode,
        t_star,
    )?;

    // Group active official cells by subject -> bimester -> M_hat_{c,s,b}.
    let mut my_by_subj: BTreeMap<&str, BTreeMap<i16, f64>> = BTreeMap::new();
    for ((s, b), &mt) in &my_raw.official_cells {
        if bimester.is_none_or(|b0| *b == b0) && matches_subject_filter(s, subject_filter_norm) {
            my_by_subj.entry(s.as_str()).or_default().insert(*b, mt);
        }
    }
    let mut peer_by_subj: BTreeMap<&str, BTreeMap<i16, f64>> = BTreeMap::new();
    for ((s, b), &mt) in &peer_raw.official_cells {
        if bimester.is_none_or(|b0| *b == b0) && matches_subject_filter(s, subject_filter_norm) {
            peer_by_subj.entry(s.as_str()).or_default().insert(*b, mt);
        }
    }

    let mut all_keys: BTreeSet<&str> = BTreeSet::new();
    all_keys.extend(my_by_subj.keys().copied());
    all_keys.extend(peer_by_subj.keys().copied());

    let mut my_wins = 0usize;
    let mut peer_wins = 0usize;
    let mut ties = 0usize;
    let mut subjects: Vec<ClassroomSubjectComparisonRow> = Vec::with_capacity(all_keys.len());

    for k in all_keys {
        let my_map = my_by_subj.get(k);
        let peer_map = peer_by_subj.get(k);
        let subject_name = dataset
            .canonical_subject_names
            .get(k)
            .cloned()
            .unwrap_or_else(|| k.to_string());

        let common_bims: Vec<i16> = match (my_map, peer_map) {
            (Some(mm), Some(pm)) => mm
                .keys()
                .copied()
                .filter(|b| pm.contains_key(b))
                .collect(),
            _ => Vec::new(),
        };

        if !common_bims.is_empty()
            && let (Some(mm), Some(pm)) = (my_map, peer_map)
        {
            let n = common_bims.len() as f64;
            let my_raw_avg = common_bims.iter().map(|b| mm[b]).sum::<f64>() / n;
            let peer_raw_avg = common_bims.iter().map(|b| pm[b]).sum::<f64>() / n;

            // Single quantization Q_2(diff_raw) without double rounding (Section 10.3.4).
            let q = quantize_k(my_raw_avg - peer_raw_avg, 2);
            let d = if q == 0 { 0.0 } else { (q as f64) / 100.0 };
            if q > 5 {
                my_wins += 1;
            } else if q < -5 {
                peer_wins += 1;
            } else {
                ties += 1;
            }

            subjects.push(ClassroomSubjectComparisonRow {
                subject_name,
                my_class_average: Some(round2(my_raw_avg)),
                peer_class_average: Some(round2(peer_raw_avg)),
                delta: Some(d),
                is_common: Some(true),
            });
        } else {
            let my_avg = my_map.filter(|m| !m.is_empty()).map(|m| {
                round2(m.values().sum::<f64>() / (m.len() as f64))
            });
            let peer_avg = peer_map.filter(|m| !m.is_empty()).map(|m| {
                round2(m.values().sum::<f64>() / (m.len() as f64))
            });
            subjects.push(ClassroomSubjectComparisonRow {
                subject_name,
                my_class_average: my_avg,
                peer_class_average: peer_avg,
                delta: None,
                is_common: Some(false),
            });
        }
    }

    subjects.sort_by(|a, b| {
        b.is_common
            .unwrap_or(false)
            .cmp(&a.is_common.unwrap_or(false))
            .then_with(|| {
                nsac_core::filters::normalize_text(&a.subject_name)
                    .cmp(&nsac_core::filters::normalize_text(&b.subject_name))
            })
    });

    let to_entry = |raw: &ClassroomInfoRaw, m: &ClassroomEval, is_my: bool| {
        ClassroomLeaderboardEntry {
            rank: 0,
            class_code: raw.class_code.clone(),
            is_my_class: is_my,
            course_family: raw.desc.course_family,
            modality: raw.desc.modality,
            grade_level: raw.desc.grade_level,
            official_average: m.official_average_disp,
            subjects_count: m.subjects_count as i64,
            posted_bimesters_count: m.posted_bimesters_count,
            active_students_count: raw.active_students_count,
            is_provisional: Some(!m.is_qualified_base),
            coverage_ratio: Some(round_k(m.cov_2d_sala, 3)),
            momentum_delta: m.momentum_delta_disp,
            consistency_stddev: m.consistency_stddev_disp,
            is_tied: None,
            best_subject: m.best_subject.clone(),
            hardest_subject: m.hardest_subject.clone(),
            calibrated_score: multi_class_mode.then(|| round2(m.s_avg_sala.clamp(0.0, 10.0))),
            consistency_score: m.consistency_score.map(|c| round2(c.clamp(0.0, 10.0))),
        }
    };

    Some(ClassroomComparisonDetail {
        my_class_entry: to_entry(my_raw, &my_eval, true),
        peer_class_entry: to_entry(peer_raw, &peer_eval, false),
        my_wins,
        peer_wins,
        ties,
        subjects,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk_prefs(account_id: i64, opted_in: bool) -> RankingPreferences {
        RankingPreferences {
            account_id,
            opted_in,
            hide_name: false,
            display_name: None,
            show_avatar: true,
            public_profile: true,
            discoverable: true,
            invisible: false,
            has_decided: true,
            updated_at: Utc::now(),
        }
    }

    fn mk_account(id: i64, email: &str, class_code: &str, opted_in: bool) -> StudentAccountRecord {
        let code = class_code.to_ascii_uppercase();
        StudentAccountRecord {
            account_id: id,
            nsac_email: email.to_string(),
            class_desc: ClassDescriptor::parse(&code),
            class_code: code,
            verified_official_name: None,
            opted_in,
            hide_name: false,
            ranking_display_name: None,
            profile_display_name: None,
            show_avatar: true,
            public_profile: true,
            invisible: false,
            avatar_version: Some(12345),
        }
    }

    fn add_grade(
        ds: &mut YearRankingDataset,
        account_id: i64,
        subject: &str,
        bimester: i16,
        grade: f64,
        mt: Option<f64>,
        absences: i32,
    ) {
        let norm = normalize_subject_key(subject);
        ds.canonical_subject_names
            .entry(norm.clone())
            .or_insert_with(|| subject.to_string());
        ds.grades_by_account
            .entry(account_id)
            .or_default()
            .push(PostedGradeRecord {
                account_id,
                subject_name: subject.to_string(),
                normalized_subject: norm,
                bimester,
                grade,
                average_grade: mt,
                absences,
            });
    }

    fn empty_dataset(year: i32) -> YearRankingDataset {
        YearRankingDataset {
            school_year: year,
            accounts: HashMap::new(),
            grades_by_account: HashMap::new(),
            tags_by_account: HashMap::new(),
            canonical_subject_names: HashMap::new(),
        }
    }

    #[test]
    fn class_descriptor_parses_all_cti_modalities_and_families() {
        let ini1 = ClassDescriptor::parse("INI1");
        assert_eq!(ini1.course_family, CourseFamily::Informatica);
        assert_eq!(ini1.modality, Modality::Integrado);
        assert_eq!(ini1.grade_level, Some(1));

        let ini2a = ClassDescriptor::parse("ini2a");
        assert_eq!(ini2a.course_family, CourseFamily::Informatica);
        assert_eq!(ini2a.modality, Modality::Integrado);
        assert_eq!(ini2a.grade_level, Some(2));

        let inf1 = ClassDescriptor::parse("INF1");
        assert_eq!(inf1.course_family, CourseFamily::Informatica);
        assert_eq!(inf1.modality, Modality::Noturno);
        assert_eq!(inf1.grade_level, Some(1));

        let eli3 = ClassDescriptor::parse("ELI3");
        assert_eq!(eli3.course_family, CourseFamily::Eletronica);
        assert_eq!(eli3.modality, Modality::Integrado);
        assert_eq!(eli3.grade_level, Some(3));

        let mec2 = ClassDescriptor::parse("MEC2");
        assert_eq!(mec2.course_family, CourseFamily::Mecanica);
        assert_eq!(mec2.modality, Modality::Noturno);
        assert_eq!(mec2.grade_level, Some(2));

        let cti = ClassDescriptor::parse("CTI");
        assert_eq!(cti.course_family, CourseFamily::Other);
        assert_eq!(cti.modality, Modality::Other);
        assert_eq!(cti.grade_level, None);
    }

    #[test]
    fn common_subjects_opt_in_is_observer_invariant_with_anti_expulsion() {
        let mut ds = empty_dataset(2026);
        let bncc = [
            "Matemática",
            "Português",
            "Física",
            "Química",
            "Biologia",
            "História",
            "Geografia",
            "Inglês",
            "Filosofia",
        ];
        let tech_inf = [
            "Algoritmos",
            "Banco de Dados",
            "Web I",
            "Sistemas Operacionais",
            "Redes",
            "POO",
            "Arquitetura",
        ];
        let tech_ini3 = ["TCC", "Mobile", "Cloud", "Segurança"];

        ds.accounts.insert(1, mk_account(1, "ana.silva@unesp.br", "INI1", true));
        ds.accounts.insert(2, mk_account(2, "bruno.lima@unesp.br", "INI3", true));
        ds.accounts.insert(3, mk_account(3, "carla.souza@unesp.br", "INF1", true));
        ds.accounts.insert(4, mk_account(4, "diego.costa@unesp.br", "ELI3", true));

        for s in bncc {
            add_grade(&mut ds, 1, s, 1, 8.0, Some(7.0), 0);
            add_grade(&mut ds, 2, s, 1, 8.5, Some(7.0), 0);
            add_grade(&mut ds, 4, s, 1, 9.0, Some(7.0), 0);
        }
        for s in tech_inf {
            add_grade(&mut ds, 1, s, 1, 7.5, Some(7.0), 0);
            add_grade(&mut ds, 3, s, 1, 9.2, Some(7.0), 0);
        }
        for s in tech_ini3 {
            add_grade(&mut ds, 2, s, 1, 9.5, Some(8.0), 0);
        }

        let social = RequesterSocialContext::default();

        // 1) Default `common_only` is ALWAYS false (Section 9.5).
        let q_school_default = RankingQuery {
            scope: RankingScope::School,
            ..Default::default()
        };
        let res_default = build_comparison_summary(
            &ds,
            3,
            Some("INF1"),
            mk_prefs(3, true),
            &social,
            &q_school_default,
        );
        assert_eq!(res_default.common_only, Some(false));
        assert_eq!(res_default.total_participants, 4);
        assert!(res_default.my_benchmark.is_some());

        // 2) Explicit `common_only = Some(true)` in Course scope (INI1, INI3, INF1):
        //    INI3 has no tech_inf and INF1 has no BNCC -> intersection is empty -> anti-expulsion
        //    keeps `common_only = Some(false)` with explanatory note (observer-invariant for INI1 and INF1!).
        let q_course_opt_in = RankingQuery {
            scope: RankingScope::Course,
            common_only: Some(true),
            ..Default::default()
        };
        let res_inf = build_comparison_summary(
            &ds,
            3,
            Some("INF1"),
            mk_prefs(3, true),
            &social,
            &q_course_opt_in,
        );
        let res_ini = build_comparison_summary(
            &ds,
            1,
            Some("INI1"),
            mk_prefs(1, true),
            &social,
            &q_course_opt_in,
        );
        assert_eq!(res_inf.common_only, Some(false));
        assert_eq!(res_ini.common_only, Some(false));
        assert!(res_inf.common_subjects_note.is_some());
        assert_eq!(res_inf.scope_label.as_deref(), Some("Informática (INI + INF)"));
        assert_eq!(res_inf.leaderboard.len(), 3);
        assert_eq!(res_ini.leaderboard.len(), 3);

        // 3) Explicit `common_only = Some(true)` in GradeLevel 3 (INI3 + ELI3 share all 9 BNCC subjects):
        //    activates cleanly with 9 common subjects and is observer-invariant!
        let q_grade_opt_in = RankingQuery {
            scope: RankingScope::GradeLevel,
            common_only: Some(true),
            ..Default::default()
        };
        let res_g3_ini = build_comparison_summary(
            &ds,
            2,
            Some("INI3"),
            mk_prefs(2, true),
            &social,
            &q_grade_opt_in,
        );
        let res_g3_eli = build_comparison_summary(
            &ds,
            4,
            Some("ELI3"),
            mk_prefs(4, true),
            &social,
            &q_grade_opt_in,
        );
        assert_eq!(res_g3_ini.common_only, Some(true));
        assert_eq!(res_g3_eli.common_only, Some(true));
        assert_eq!(res_g3_ini.common_subjects.as_ref().map(Vec::len), Some(9));
        assert_eq!(res_g3_eli.common_subjects, res_g3_ini.common_subjects);
    }

    #[test]
    fn provisional_coverage_prevents_single_grade_from_stealing_first_place() {
        let mut ds = empty_dataset(2026);
        let subjs = [
            "Matemática",
            "Português",
            "Física",
            "Química",
            "Biologia",
            "História",
            "Geografia",
            "Inglês",
            "Filosofia",
            "Sociologia",
        ];
        // 5 students in INI1 => large class (|A_c| >= 5), E_subj^anual(INI1) = 10.
        ds.accounts.insert(1, mk_account(1, "completo@unesp.br", "INI1", true));
        ds.accounts.insert(2, mk_account(2, "parcial@unesp.br", "INI1", true));
        ds.accounts.insert(3, mk_account(3, "terceiro@unesp.br", "INI1", true));
        ds.accounts.insert(4, mk_account(4, "quarto@unesp.br", "INI1", true));
        ds.accounts.insert(5, mk_account(5, "quinto@unesp.br", "INI1", true));

        for s in subjs {
            add_grade(&mut ds, 1, s, 1, 8.5, Some(7.0), 0);
            add_grade(&mut ds, 3, s, 1, 8.0, Some(7.0), 0);
            add_grade(&mut ds, 4, s, 1, 7.5, Some(7.0), 0);
            add_grade(&mut ds, 5, s, 1, 7.0, Some(7.0), 0);
        }
        // Student 2 only has 1 grade with 10.0!
        add_grade(&mut ds, 2, "Matemática", 1, 10.0, Some(7.0), 0);

        let social = RequesterSocialContext::default();
        let res = build_comparison_summary(
            &ds,
            1,
            Some("INI1"),
            mk_prefs(1, true),
            &social,
            &RankingQuery::default(),
        );
        assert_eq!(res.expected_subjects_count, Some(10));
        assert_eq!(res.qualified_participants, Some(4));
        assert_eq!(res.total_participants, 5);
        assert_eq!(res.leaderboard[0].account_id, 1);
        assert_eq!(res.leaderboard[0].is_provisional, Some(false));
        assert_eq!(res.leaderboard[4].account_id, 2);
        assert_eq!(res.leaderboard[4].is_provisional, Some(true));
        assert_eq!(res.leaderboard[4].coverage_ratio, Some(0.1));

        // Provisional student (G_2) gets my_percentile = None (Section 9.4).
        let res_p2 = build_comparison_summary(
            &ds,
            2,
            Some("INI1"),
            mk_prefs(2, true),
            &social,
            &RankingQuery::default(),
        );
        assert_eq!(res_p2.my_rank, Some(5));
        assert_eq!(res_p2.my_percentile, None);
    }

    #[test]
    fn bimester_weighted_average_and_atomic_paired_delta_vs_class() {
        let mut ds = empty_dataset(2026);
        ds.accounts.insert(1, mk_account(1, "aluno@unesp.br", "INI1", true));

        // Subject A has 2 bimesters (8.0 and 10.0, valid M.T. = 7.0 -> paired delta +2.0)
        add_grade(&mut ds, 1, "Matemática", 1, 8.0, Some(7.0), 1);
        add_grade(&mut ds, 1, "Matemática", 2, 10.0, Some(7.0), 1);
        // Subject B has 1 bimester (5.0, NO valid M.T.!)
        add_grade(&mut ds, 1, "Artes", 1, 5.0, Some(0.0), 0);

        let social = RequesterSocialContext::default();
        let res = build_comparison_summary(
            &ds,
            1,
            Some("INI1"),
            mk_prefs(1, true),
            &social,
            &RankingQuery::default(),
        );
        let me = res.my_entry.expect("entry must exist");
        // Section 4.3 bimester-coverage weighting (= cell mean): (8.0 + 10.0 + 5.0) / 3 = 7.67.
        assert!((me.personal_average - 7.67).abs() < 1e-6);
        // Paired delta strictly over cells with valid M.T. > 0: only Matemática ((8-7 + 10-7)/2 = +2.0)!
        assert!((me.class_average - 7.0).abs() < 1e-6);
        assert!((me.delta_vs_class - 2.0).abs() < 1e-6);
    }

    #[test]
    fn momentum_and_consistency_sorting_work_accurately() {
        let mut ds = empty_dataset(2026);
        let subjs = ["Matemática", "Português", "Física", "Química"];
        ds.accounts.insert(1, mk_account(1, "constante@unesp.br", "INI1", true));
        ds.accounts.insert(2, mk_account(2, "volatil@unesp.br", "INI1", true));

        // Student 1: B1 = 7.0 across all 4, B2 = 8.5 across all 4 (momentum = +1.5, cell stddev = 0.75)
        for s in subjs {
            add_grade(&mut ds, 1, s, 1, 7.0, Some(7.0), 0);
            add_grade(&mut ds, 1, s, 2, 8.5, Some(7.0), 0);
        }
        // Student 2: B1 = 8.0 avg, B2 = 7.5 avg (momentum = -0.5), huge cell variance
        add_grade(&mut ds, 2, "Matemática", 1, 10.0, Some(7.0), 0);
        add_grade(&mut ds, 2, "Português", 1, 10.0, Some(7.0), 0);
        add_grade(&mut ds, 2, "Física", 1, 6.0, Some(7.0), 0);
        add_grade(&mut ds, 2, "Química", 1, 6.0, Some(7.0), 0);

        add_grade(&mut ds, 2, "Matemática", 2, 10.0, Some(7.0), 0);
        add_grade(&mut ds, 2, "Português", 2, 10.0, Some(7.0), 0);
        add_grade(&mut ds, 2, "Física", 2, 5.0, Some(7.0), 0);
        add_grade(&mut ds, 2, "Química", 2, 5.0, Some(7.0), 0);

        let social = RequesterSocialContext::default();

        let q_mom = RankingQuery {
            sort_by: RankingSortBy::Momentum,
            ..Default::default()
        };
        let res_mom = build_comparison_summary(&ds, 1, Some("INI1"), mk_prefs(1, true), &social, &q_mom);
        assert_eq!(res_mom.leaderboard[0].account_id, 1);
        assert_eq!(res_mom.leaderboard[0].momentum_delta, Some(1.5));
        assert_eq!(res_mom.leaderboard[1].account_id, 2);
        assert_eq!(res_mom.leaderboard[1].momentum_delta, Some(-0.5));

        let q_con = RankingQuery {
            sort_by: RankingSortBy::Consistency,
            ..Default::default()
        };
        let res_con = build_comparison_summary(&ds, 1, Some("INI1"), mk_prefs(1, true), &social, &q_con);
        // Student 1 has cell mean 7.75, cell stddev 0.75 (wins CARA consistency).
        // Student 2 has cell mean 7.75, cell stddev 2.28 (penalized by CARA + psi below 6.0).
        assert_eq!(res_con.leaderboard[0].account_id, 1);
        assert_eq!(res_con.leaderboard[0].consistency_stddev, Some(0.75));
        assert_eq!(res_con.leaderboard[1].account_id, 2);
        assert_eq!(res_con.leaderboard[1].consistency_stddev, Some(2.28));
    }

    #[test]
    fn competition_tie_ranking_1_1_3_and_benchmark_when_opted_out() {
        let mut ds = empty_dataset(2026);
        ds.accounts.insert(1, mk_account(1, "a@unesp.br", "INI1", true));
        ds.accounts.insert(2, mk_account(2, "b@unesp.br", "INI1", true));
        ds.accounts.insert(3, mk_account(3, "c@unesp.br", "INI1", true));
        ds.accounts.insert(4, mk_account(4, "optout@unesp.br", "INI1", false));

        add_grade(&mut ds, 1, "Matemática", 1, 9.5, Some(7.0), 0);
        add_grade(&mut ds, 2, "Matemática", 1, 9.5, Some(7.0), 0);
        add_grade(&mut ds, 3, "Matemática", 1, 8.0, Some(7.0), 0);
        add_grade(&mut ds, 4, "Matemática", 1, 9.0, Some(7.0), 0);

        let social = RequesterSocialContext::default();
        let res = build_comparison_summary(
            &ds,
            4,
            Some("INI1"),
            mk_prefs(4, false),
            &social,
            &RankingQuery::default(),
        );
        assert_eq!(res.total_participants, 3);
        assert_eq!(res.leaderboard[0].rank, 1);
        assert_eq!(res.leaderboard[0].is_tied, Some(true));
        assert_eq!(res.leaderboard[1].rank, 1);
        assert_eq!(res.leaderboard[1].is_tied, Some(true));
        assert_eq!(res.leaderboard[2].rank, 3);
        assert_eq!(res.leaderboard[2].is_tied, None);

        // Opted-out requester still gets full my_benchmark + hypothetical rank 3 of 4!
        assert!(res.my_entry.is_none());
        let bm = res.my_benchmark.expect("benchmark must be present even when opted_out");
        assert!((bm.personal_average - 9.0).abs() < 1e-6);
        assert_eq!(bm.hypothetical_rank, Some(3));
        assert_eq!(bm.hypothetical_total, Some(4));
    }

    #[test]
    fn peer_comparison_counts_wins_strictly_on_common_subjects() {
        let mut ds = empty_dataset(2026);
        ds.accounts.insert(1, mk_account(1, "me@unesp.br", "INI1", true));
        ds.accounts.insert(2, mk_account(2, "peer@unesp.br", "INF1", true));

        add_grade(&mut ds, 1, "Algoritmos", 1, 9.0, Some(7.0), 0);
        add_grade(&mut ds, 2, "Algoritmos", 1, 8.0, Some(7.5), 1);
        add_grade(&mut ds, 1, "Português", 1, 9.5, Some(7.0), 0); // Only INI1
        add_grade(&mut ds, 2, "Redes", 1, 9.8, Some(8.0), 0); // Only INF1

        let social = RequesterSocialContext::default();
        let detail = build_peer_comparison_from_dataset(&ds, 1, 2, &social, None, None)
            .expect("peer comparison must succeed");
        assert_eq!(detail.my_wins, 1);
        assert_eq!(detail.peer_wins, 0);
        assert_eq!(detail.ties, 0);
        assert_eq!(detail.subjects.len(), 3);
        assert_eq!(detail.subjects[0].subject_name, "Algoritmos");
        assert_eq!(detail.subjects[0].is_common, Some(true));
        assert_eq!(detail.subjects[0].my_class_average, Some(7.0));
        assert_eq!(detail.subjects[0].peer_class_average, Some(7.5));
        assert_eq!(detail.subjects[1].is_common, Some(false));
    }

    #[test]
    fn classroom_ranking_deduplicates_mt_and_supports_scopes_and_comparison() {
        let mut ds = empty_dataset(2026);
        // Two students in INI1 (should deduplicate M.T., not double-weight)
        ds.accounts.insert(1, mk_account(1, "a@unesp.br", "INI1", true));
        ds.accounts.insert(2, mk_account(2, "b@unesp.br", "INI1", false));
        // One student in ELI1 (opted out of personal ranking — classroom ranking still uses official M.T.)
        ds.accounts.insert(3, mk_account(3, "c@unesp.br", "ELI1", false));
        // One student in INF1 (Noturno)
        ds.accounts.insert(4, mk_account(4, "d@unesp.br", "INF1", true));

        add_grade(&mut ds, 1, "Matemática", 1, 9.0, Some(7.2), 0);
        add_grade(&mut ds, 2, "Matemática", 1, 5.0, Some(7.2), 0);
        add_grade(&mut ds, 1, "Português", 1, 8.0, Some(7.8), 0);
        add_grade(&mut ds, 2, "Português", 1, 6.0, Some(7.8), 0);
        add_grade(&mut ds, 1, "História", 1, 8.0, Some(7.5), 0);
        add_grade(&mut ds, 1, "Matemática", 2, 9.5, Some(8.0), 0);
        add_grade(&mut ds, 1, "Português", 2, 8.5, Some(8.2), 0);
        add_grade(&mut ds, 1, "História", 2, 8.5, Some(8.1), 0);

        add_grade(&mut ds, 3, "Matemática", 1, 7.0, Some(8.5), 0);
        add_grade(&mut ds, 3, "Português", 1, 7.5, Some(8.5), 0);
        add_grade(&mut ds, 3, "História", 1, 7.5, Some(8.5), 0);
        add_grade(&mut ds, 3, "Matemática", 2, 7.5, Some(8.5), 0);
        add_grade(&mut ds, 3, "Português", 2, 7.5, Some(8.5), 0);
        add_grade(&mut ds, 3, "História", 2, 7.5, Some(8.5), 0);

        add_grade(&mut ds, 4, "Algoritmos", 1, 8.5, Some(7.9), 0);
        add_grade(&mut ds, 4, "Matemática", 1, 7.0, Some(6.5), 0);

        let res_all = build_classroom_ranking(
            &ds,
            1,
            Some("INI1"),
            &ClassroomRankingQuery {
                common_only: Some(false),
                ..Default::default()
            },
        );
        assert_eq!(res_all.total_classes, 3);
        assert_eq!(res_all.leaderboard[0].class_code, "ELI1");
        assert!((res_all.leaderboard[0].official_average - 8.5).abs() < 1e-6);
        assert_eq!(res_all.leaderboard[1].class_code, "INI1");
        assert!(res_all.leaderboard[1].is_my_class);
        assert_eq!(res_all.my_class_rank, Some(2));
        // INI1 B1 avg = (7.2+7.8)/2 = 7.5, B2 avg = (8.0+8.2)/2 = 8.1 -> overall = 7.8, momentum = +0.6
        assert!((res_all.leaderboard[1].official_average - 7.8).abs() < 1e-6);
        assert_eq!(res_all.leaderboard[1].momentum_delta, Some(0.6));

        // Course scope for INI1 includes Informática (INI1 + INF1), excludes ELI1
        let res_course = build_classroom_ranking(
            &ds,
            1,
            Some("INI1"),
            &ClassroomRankingQuery {
                scope: ClassroomRankingScope::Course,
                common_only: Some(false),
                ..Default::default()
            },
        );
        assert_eq!(res_course.total_classes, 2);
        assert!(res_course.leaderboard.iter().any(|c| c.class_code == "INF1"));
        assert!(!res_course.leaderboard.iter().any(|c| c.class_code == "ELI1"));

        // Classroom comparison INI1 vs ELI1
        let cmp = build_classroom_comparison_from_dataset(&ds, "INI1", "ELI1", None, None)
            .expect("classroom comparison must succeed");
        assert_eq!(cmp.my_wins, 0);
        assert_eq!(cmp.peer_wins, 3);
        assert_eq!(cmp.ties, 0);
    }

    #[test]
    fn ini_ab_pdf_extraction_and_subclass_resolution() {
        use flate2::Compression;
        use flate2::write::ZlibEncoder;
        use std::io::Write;

        // 1. Real dompdf-style FlateDecode PDF stream containing `[(INI3A)] TJ`
        let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
        enc.write_all(b"BT 983 732.852 Td /F1 10.0 Tf [(INI3A)] TJ ET")
            .unwrap();
        let compressed = enc.finish().unwrap();
        let mut fake_pdf = b"%PDF-1.7\n1 0 obj\n<< /Filter /FlateDecode >>\nstream\n".to_vec();
        fake_pdf.extend_from_slice(&compressed);
        fake_pdf.extend_from_slice(b"\nendstream\nendobj\n%%EOF");
        assert_eq!(extract_ini_ab_from_pdf(&fake_pdf).as_deref(), Some("INI3A"));

        // Non-INI tokens must never match
        assert_eq!(extract_ini_ab_token("ELI3A MEI1B INF1A"), None);
        assert_eq!(extract_ini_ab_token("Turma: INI2B (2026)").as_deref(), Some("INI2B"));

        // 2. Single anchor (INI3A) splits 2 matching students into INI3A and 1 divergent student into INI3B,
        //    leaves ELI3 untouched, and ignores an invisible anchor for INI1.
        let mut ds = empty_dataset(2026);
        ds.accounts.insert(1, mk_account(1, "a1@unesp.br", "INI3", true)); // Anchor A
        ds.accounts.insert(2, mk_account(2, "a2@unesp.br", "INI3", true)); // Matches A
        ds.accounts.insert(3, mk_account(3, "a3@unesp.br", "INI3", true)); // Diverges -> B
        ds.accounts.insert(4, mk_account(4, "a4@unesp.br", "ELI3", true)); // Non-INI -> untouched
        ds.accounts.insert(5, mk_account(5, "a5@unesp.br", "INI1", true)); // INI1 visible
        ds.accounts.insert(6, mk_account(6, "a6@unesp.br", "INI1", true)); // INI1 invisible anchor
        ds.accounts.get_mut(&6).unwrap().invisible = true;

        for &id in &[1, 2] {
            add_grade(&mut ds, id, "Matemática", 1, 8.0, Some(7.4), 0);
            add_grade(&mut ds, id, "História", 1, 8.5, Some(7.8), 0);
            add_grade(&mut ds, id, "Física", 1, 7.0, Some(6.5), 0);
        }
        add_grade(&mut ds, 3, "Matemática", 1, 8.0, Some(6.1), 0);
        add_grade(&mut ds, 3, "História", 1, 8.5, Some(5.4), 0);
        add_grade(&mut ds, 3, "Física", 1, 7.0, Some(6.5), 0); // 1 cell ties, 2 diverge -> strict majority B

        add_grade(&mut ds, 4, "Matemática", 1, 8.0, Some(7.4), 0);
        add_grade(&mut ds, 5, "Matemática", 1, 8.0, Some(7.0), 0);
        add_grade(&mut ds, 6, "Matemática", 1, 8.0, Some(7.0), 0);

        let mut anchors = HashMap::new();
        anchors.insert(1, "INI3A".to_string());
        anchors.insert(6, "INI1B".to_string()); // invisible -> must NOT anchor visible student 5!

        resolve_ini_ab_subclasses(&mut ds.accounts, &ds.grades_by_account, &anchors);

        assert_eq!(ds.accounts[&1].class_code, "INI3A");
        assert_eq!(ds.accounts[&2].class_code, "INI3A");
        assert_eq!(ds.accounts[&3].class_code, "INI3B");
        assert_eq!(ds.accounts[&4].class_code, "ELI3");
        // INI1 had only an invisible anchor -> student 5 MUST stay unified INI1 (zero privacy leak)
        assert_eq!(ds.accounts[&5].class_code, "INI1");
    }

    #[test]
    fn v7_cara_consistency_strict_monotonicity_no_penalty_for_raising_grade() {
        let mut ds = empty_dataset(2026);
        let subjs = ["Matemática", "Português", "Física", "Química"];
        // Student 1: [8.0, 8.0, 8.0, 8.0] (avg 8.0, stddev 0.0)
        // Student 2: [8.0, 8.0, 8.0, 10.0] (strictly dominates Student 1 in one subject!)
        // Old linear formula (avg - 0.65*sd) could penalize raising a top grade; CARA MUST rank 2 > 1!
        ds.accounts.insert(1, mk_account(1, "s1@unesp.br", "INI2A", true));
        ds.accounts.insert(2, mk_account(2, "s2@unesp.br", "INI2A", true));

        for s in subjs {
            add_grade(&mut ds, 1, s, 1, 8.0, Some(7.0), 0);
        }
        add_grade(&mut ds, 2, "Matemática", 1, 8.0, Some(7.0), 0);
        add_grade(&mut ds, 2, "Português", 1, 8.0, Some(7.0), 0);
        add_grade(&mut ds, 2, "Física", 1, 8.0, Some(7.0), 0);
        add_grade(&mut ds, 2, "Química", 1, 10.0, Some(7.0), 0);

        let social = RequesterSocialContext::default();
        let res = build_comparison_summary(
            &ds,
            1,
            Some("INI2A"),
            mk_prefs(1, true),
            &social,
            &RankingQuery {
                sort_by: RankingSortBy::Consistency,
                ..Default::default()
            },
        );
        assert_eq!(res.leaderboard[0].account_id, 2);
        assert_eq!(res.leaderboard[1].account_id, 1);
    }

    #[test]
    fn v7_multi_class_ridge_preserves_exact_intra_class_order_even_without_mt() {
        let mut ds = empty_dataset(2026);
        // Two classmates in INI2A with identical support, including a subject WITHOUT official M.T.
        // Plus another class ELI2 so School scope activates Multi-Class Ridge mode.
        ds.accounts.insert(1, mk_account(1, "top@unesp.br", "INI2A", true));
        ds.accounts.insert(2, mk_account(2, "mid@unesp.br", "INI2A", true));
        ds.accounts.insert(3, mk_account(3, "eli@unesp.br", "ELI2", true));

        let shared = ["Matemática", "Português", "Física", "Química"];
        for s in shared {
            add_grade(&mut ds, 1, s, 1, 8.8, Some(7.5), 0);
            add_grade(&mut ds, 2, s, 1, 8.6, Some(7.5), 0);
            add_grade(&mut ds, 3, s, 1, 7.5, Some(6.8), 0);
        }
        // Subject without official M.T. (None) — unified reference m_tilde prevents black-hole inversion!
        add_grade(&mut ds, 1, "Projeto Integrador", 1, 9.5, None, 0);
        add_grade(&mut ds, 2, "Projeto Integrador", 1, 8.0, None, 0);

        let social = RequesterSocialContext::default();
        let res_school = build_comparison_summary(
            &ds,
            1,
            Some("INI2A"),
            mk_prefs(1, true),
            &social,
            &RankingQuery {
                scope: RankingScope::School,
                ..Default::default()
            },
        );
        let pos_1 = res_school
            .leaderboard
            .iter()
            .position(|e| e.account_id == 1)
            .expect("student 1 present");
        let pos_2 = res_school
            .leaderboard
            .iter()
            .position(|e| e.account_id == 2)
            .expect("student 2 present");
        assert!(
            pos_1 < pos_2,
            "exact intra-class theorem violated in multi-class mode: pos_1={pos_1}, pos_2={pos_2}"
        );
    }

    #[test]
    fn v7_classroom_minimum_student_quorum_and_disconnected_protection() {
        let mut ds = empty_dataset(2026);
        // 12 Integrado subjects -> cov = 12/16 = 0.75 >= tau_cov (passes coverage for small Integrado classes).
        let subjs = [
            "Matemática",
            "Português",
            "Física",
            "Química",
            "História",
            "Geografia",
            "Biologia",
            "Inglês",
            "Filosofia",
            "Sociologia",
            "Artes",
            "Educação Física",
        ];

        // INI1 (Integrado, quorum = 3): only 2 visible students -> MUST be provisional (G_2, percentile = None).
        ds.accounts.insert(1, mk_account(1, "i1a@unesp.br", "INI1", true));
        ds.accounts.insert(2, mk_account(2, "i1b@unesp.br", "INI1", true));

        // INI2A (Integrado, quorum = 3): 3 visible students -> qualifies (G_1).
        ds.accounts.insert(3, mk_account(3, "i2a@unesp.br", "INI2A", true));
        ds.accounts.insert(4, mk_account(4, "i2b@unesp.br", "INI2A", true));
        ds.accounts.insert(5, mk_account(5, "i2c@unesp.br", "INI2A", true));

        // INF1 (Noturno, quorum = 2): 2 visible students with 6/7 exclusive technical subjects -> qualifies (G_1)
        // and does NOT teleport its official_average to mu_0.
        ds.accounts.insert(6, mk_account(6, "n1a@unesp.br", "INF1", true));
        ds.accounts.insert(7, mk_account(7, "n1b@unesp.br", "INF1", true));

        for s in subjs {
            for &id in &[1, 2] {
                add_grade(&mut ds, id, s, 1, 9.0, Some(8.8), 0);
            }
            for &id in &[3, 4, 5] {
                add_grade(&mut ds, id, s, 1, 8.0, Some(7.6), 0);
            }
        }
        let tech = ["Algoritmos", "Redes", "Banco de Dados", "Web I", "POO", "Sistemas"];
        for s in tech {
            for &id in &[6, 7] {
                add_grade(&mut ds, id, s, 1, 9.0, Some(8.9), 0);
            }
        }

        let res = build_classroom_ranking(
            &ds,
            1,
            Some("INI1"),
            &ClassroomRankingQuery::default(),
        );
        assert!(!res.common_only);
        assert_eq!(res.qualified_classes, 2); // INI2A and INF1
        assert_eq!(res.total_classes, 3);
        // INI1 is provisional (only 2 < 3 Integrado students) so it ranks AFTER qualified INI2A & INF1
        // and gets my_class_percentile = None!
        assert_eq!(res.leaderboard[2].class_code, "INI1");
        assert_eq!(res.leaderboard[2].is_provisional, Some(true));
        assert_eq!(res.my_class_percentile, None);
        // Disconnected INF1 keeps its true M.T. (8.9 > 7.6) and ranks #1!
        assert_eq!(res.leaderboard[0].class_code, "INF1");
        assert_eq!(res.leaderboard[0].official_average, 8.9);
    }

    #[test]
    fn v7_absolute_privacy_invisible_accounts_zero_leak_and_private_benchmark() {
        let mut ds_clean = empty_dataset(2026);
        let subjs = ["Matemática", "Português", "Física", "Química"];
        for id in 1..=3 {
            ds_clean.accounts.insert(id, mk_account(id, &format!("v{id}@unesp.br"), "INI1", true));
            for s in subjs {
                add_grade(&mut ds_clean, id, s, 1, 7.0 + (id as f64) * 0.5, Some(7.2), 0);
            }
        }

        let mut ds_with_inv = ds_clean.clone();
        let mut inv = mk_account(99, "ghost@unesp.br", "INI1", false);
        inv.invisible = true;
        ds_with_inv.accounts.insert(99, inv);
        for s in subjs {
            add_grade(&mut ds_with_inv, 99, s, 1, 10.0, Some(1.0), 0);
        }
        add_grade(&mut ds_with_inv, 99, "Matéria Secreta", 1, 10.0, Some(1.5), 0);

        let social = RequesterSocialContext::default();
        let q = RankingQuery::default();
        let r_clean = build_comparison_summary(&ds_clean, 1, Some("INI1"), mk_prefs(1, true), &social, &q);
        let r_inv = build_comparison_summary(&ds_with_inv, 1, Some("INI1"), mk_prefs(1, true), &social, &q);

        assert_eq!(r_clean.available_subjects, r_inv.available_subjects);
        assert!(!r_inv.available_subjects.iter().any(|s| s.contains("Secreta")));
        assert_eq!(r_clean.leaderboard.len(), r_inv.leaderboard.len());
        assert_eq!(r_clean.leaderboard[0].personal_average, r_inv.leaderboard[0].personal_average);
        assert_eq!(r_clean.leaderboard[0].class_average, r_inv.leaderboard[0].class_average);

        let cr_clean = build_classroom_ranking(&ds_clean, 1, Some("INI1"), &ClassroomRankingQuery::default());
        let cr_inv = build_classroom_ranking(&ds_with_inv, 1, Some("INI1"), &ClassroomRankingQuery::default());
        assert_eq!(cr_clean.leaderboard[0].active_students_count, cr_inv.leaderboard[0].active_students_count);
        assert_eq!(cr_clean.leaderboard[0].official_average, cr_inv.leaderboard[0].official_average);

        // Invisible requester still receives their own private my_benchmark + hypothetical_rank!
        let mut inv_prefs = mk_prefs(99, false);
        inv_prefs.invisible = true;
        let r_ghost = build_comparison_summary(&ds_with_inv, 99, Some("INI1"), inv_prefs, &social, &q);
        assert!(r_ghost.my_entry.is_none());
        let bm = r_ghost.my_benchmark.expect("invisible requester gets private benchmark");
        assert_eq!(bm.personal_average, 10.0);
        assert_eq!(bm.hypothetical_rank, Some(1));
        assert_eq!(bm.hypothetical_total, Some(4));
    }

    #[test]
    fn v7_exact_canonical_subject_filter_and_peer_bimester_intersection() {
        let mut ds = empty_dataset(2026);
        ds.accounts.insert(1, mk_account(1, "a@unesp.br", "INI1", true));
        ds.accounts.insert(2, mk_account(2, "b@unesp.br", "INI1", true));

        // Exact canonical subject filter: "Física" MUST NOT match "Educação Física"!
        add_grade(&mut ds, 1, "Física", 1, 8.0, Some(7.0), 0);
        add_grade(&mut ds, 1, "Educação Física", 1, 10.0, Some(9.0), 0);
        add_grade(&mut ds, 2, "Física", 1, 9.0, Some(7.0), 0);
        // Student 2 also has Física in B2 (not present for Student 1)
        add_grade(&mut ds, 2, "Física", 2, 5.0, Some(7.0), 0);

        let social = RequesterSocialContext::default();
        let res = build_comparison_summary(
            &ds,
            1,
            Some("INI1"),
            mk_prefs(1, true),
            &social,
            &RankingQuery {
                subject_filter: Some("Física".to_string()),
                ..Default::default()
            },
        );
        let me = res.my_entry.expect("student 1 present");
        assert_eq!(me.subjects_count, 1);
        assert_eq!(me.personal_average, 8.0); // NOT (8+10)/2 = 9.0!

        // 1v1 PeerComparison intersects bimesters strictly for Física -> compares B1 only (8.0 vs 9.0, diff -1.0)!
        let peer_cmp = build_peer_comparison_from_dataset(&ds, 1, 2, &social, None, None)
            .expect("peer comparison must succeed");
        let fisica_row = peer_cmp
            .subjects
            .iter()
            .find(|r| r.subject_name == "Física")
            .expect("Física row present");
        assert_eq!(fisica_row.is_common, Some(true));
        assert_eq!(fisica_row.my_average, Some(8.0));
        assert_eq!(fisica_row.peer_average, Some(9.0)); // B1 only, NOT (9+5)/2 = 7.0!
        assert_eq!(fisica_row.delta_me_vs_peer, Some(-1.0));
        assert_eq!(peer_cmp.peer_wins, 1);
        assert_eq!(peer_cmp.my_wins, 0);
    }

    #[test]
    fn v7_headroom_ceiling_protection_and_nerd_stats() {
        let mut ds = empty_dataset(2026);
        let subjs = [
            "Matemática",
            "Física",
            "Química",
            "Biologia",
            "Português",
            "História",
            "Geografia",
            "Inglês",
            "Filosofia",
            "Sociologia",
            "Educação Física",
            "Arte",
            "Algoritmos",
            "Banco de Dados",
            "Redes",
            "Sistemas Operacionais",
        ];

        // Easy class (INI3A, M.T. = 8.4):
        // - id 1: literal 10.00 across all 16 subjects (full coverage perfection!)
        // - id 2: 9.70
        // - id 3: 8.40
        ds.accounts.insert(1, mk_account(1, "perfect@unesp.br", "INI3A", true));
        ds.accounts.insert(2, mk_account(2, "strong_easy@unesp.br", "INI3A", true));
        ds.accounts.insert(3, mk_account(3, "avg_easy@unesp.br", "INI3A", true));
        for s in subjs {
            add_grade(&mut ds, 1, s, 1, 10.0, Some(8.4), 0);
            add_grade(&mut ds, 2, s, 1, 9.7, Some(8.4), 0);
            add_grade(&mut ds, 3, s, 1, 8.4, Some(8.4), 0);
        }

        // Hard class (MEI1, M.T. = 6.6):
        // - id 4: 9.60 (huge standout in hard class, but < 10.00!)
        // - id 5: 6.60
        ds.accounts.insert(4, mk_account(4, "ace_hard@unesp.br", "MEI1", true));
        ds.accounts.insert(5, mk_account(5, "avg_hard@unesp.br", "MEI1", true));
        for s in subjs {
            add_grade(&mut ds, 4, s, 1, 9.6, Some(6.6), 0);
            add_grade(&mut ds, 5, s, 1, 6.6, Some(6.6), 0);
        }

        let social = RequesterSocialContext::default();
        let q = RankingQuery {
            scope: RankingScope::School,
            ..Default::default()
        };
        let res_for_2 =
            build_comparison_summary(&ds, 2, Some("INI3A"), mk_prefs(2, true), &social, &q);

        // 1. Literal 10.00 in easy class MUST be #1 (score = 10.00), beating 9.60 in hard class!
        assert_eq!(res_for_2.leaderboard[0].account_id, 1);
        assert_eq!(res_for_2.leaderboard[0].calibrated_score, Some(10.0));

        // 2. Below 10.00, 9.60 in hard class (MEI1) MUST still beat 9.70 in easy class (INI3A)!
        assert_eq!(res_for_2.leaderboard[1].account_id, 4);
        assert_eq!(res_for_2.leaderboard[2].account_id, 2);

        // 3. Exact intra-class order preserved (id 1 > id 2 > id 3 inside INI3A).
        let pos = |id: i64| res_for_2.leaderboard.iter().position(|e| e.account_id == id).unwrap();
        assert!(pos(1) < pos(2) && pos(2) < pos(3));

        // 4. Student 2's benchmark includes leader_target_average == 10.00 (since #1 has 10.00) and nerd_stats!
        let bm2 = res_for_2.my_benchmark.expect("benchmark present for opted-in user");
        assert_eq!(bm2.leader_target_average, Some(10.0));
        assert!(bm2.calibrated_score.is_some());
        assert!(bm2.headroom_pct.is_some_and(|h| h > 70.0 && h < 90.0));
        let ns2 = bm2.nerd_stats.expect("nerd_stats present");
        assert!(ns2.multi_class_calibrated);
        assert!(ns2.ridge_active);
        assert_eq!(ns2.active_bimesters, 1);

        // 5. Leader (#1) has leader_target_average == None.
        let res_for_1 =
            build_comparison_summary(&ds, 1, Some("INI3A"), mk_prefs(1, true), &social, &q);
        let bm1 = res_for_1.my_benchmark.expect("benchmark present for leader");
        assert_eq!(bm1.leader_target_average, None);
        assert_eq!(bm1.headroom_pct, Some(100.0));
    }

    #[test]
    fn v7_ini_ab_collapse_and_raw_average_modes() {
        let mut ds = empty_dataset(2026);
        let bncc = ["Matematica", "Portugues", "Fisica", "Quimica", "Historia", "Geografia"];
        let ini_tech = ["Sistemas", "Banco de Dados", "Java", "Mobile"];
        let eli_tech = ["Eletronica Digital", "Microcontroladores", "Circuitos", "Automacao"];

        // INI3A (3 students): BNCC M.T. = 7.8, exclusive tech M.T. = 9.0
        // INI3B (3 students): BNCC M.T. = 7.4, exclusive tech M.T. = 8.2
        // ELI3  (3 students): BNCC M.T. = 8.1, exclusive tech M.T. = 7.2
        for i in 1..=3 {
            ds.accounts.insert(i, mk_account(i, &format!("a{i}@unesp.br"), "INI3A", true));
            ds.accounts.insert(10 + i, mk_account(10 + i, &format!("b{i}@unesp.br"), "INI3B", true));
            ds.accounts.insert(20 + i, mk_account(20 + i, &format!("e{i}@unesp.br"), "ELI3", true));
            for s in bncc {
                add_grade(&mut ds, i, s, 1, 7.8, Some(7.8), 0);
                add_grade(&mut ds, 10 + i, s, 1, 7.4, Some(7.4), 0);
                add_grade(&mut ds, 20 + i, s, 1, 8.1, Some(8.1), 0);
            }
            for s in ini_tech {
                add_grade(&mut ds, i, s, 1, 9.0, Some(9.0), 0);
                add_grade(&mut ds, 10 + i, s, 1, 8.2, Some(8.2), 0);
            }
            for s in eli_tech {
                add_grade(&mut ds, 20 + i, s, 1, 7.2, Some(7.2), 0);
            }
        }

        // Verify class_count_for_cell collapses INI3A + INI3B into 1 base class ("INI3")
        // so Informatics tech subjects have count == 1 (not 2!).
        let model = YearStatisticalModel::from_dataset(&ds);
        assert_eq!(
            model.class_count_for_cell.get(&("sistemas".to_string(), 1)),
            Some(&1)
        );
        assert_eq!(
            model.class_count_for_cell.get(&("matematica".to_string(), 1)),
            Some(&2) // INI3 + ELI3
        );

        // Score CTI classroom ranking: ELI3 (higher BNCC M.T. 8.1 vs 7.8) MUST beat INI3A!
        let cti_res = build_classroom_ranking(
            &ds,
            1,
            Some("INI3A"),
            &ClassroomRankingQuery {
                scope: ClassroomRankingScope::All,
                sort_by: ClassroomRankingSortBy::OfficialAverage,
                ..Default::default()
            },
        );
        assert_eq!(cti_res.leaderboard[0].class_code, "ELI3");
        assert!(cti_res.leaderboard[0].calibrated_score.is_some());

        // RawAverage classroom ranking: INI3A (raw avg 8.28) beats ELI3 (raw avg 7.74)!
        let raw_res = build_classroom_ranking(
            &ds,
            1,
            Some("INI3A"),
            &ClassroomRankingQuery {
                scope: ClassroomRankingScope::All,
                sort_by: ClassroomRankingSortBy::RawAverage,
                ..Default::default()
            },
        );
        assert_eq!(raw_res.leaderboard[0].class_code, "INI3A");

        // Student RawAverage mode works and exposes raw order.
        let social = RequesterSocialContext::default();
        let stu_raw = build_comparison_summary(
            &ds,
            1,
            Some("INI3A"),
            mk_prefs(1, true),
            &social,
            &RankingQuery {
                scope: RankingScope::School,
                sort_by: RankingSortBy::RawAverage,
                ..Default::default()
            },
        );
        assert_eq!(stu_raw.sort_by.as_deref(), Some("raw_average"));
        assert_eq!(stu_raw.leaderboard[0].class_code, "INI3A");
    }
}
