use serde::{Deserialize, Serialize};
use unicode_normalization::{UnicodeNormalization, char::is_combining_mark};

use crate::error::AppError;
use crate::models::{AllYearsResponse, FullGrades, RecoveryStatusCode, UnifiedBimesterData, YearInfo};

pub fn normalize_text(input: &str) -> String {
    input
        .nfd()
        .filter(|&c| !is_combining_mark(c))
        .collect::<String>()
        .to_lowercase()
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct NumberFilter {
    pub eq: Option<Vec<f64>>,
    pub neq: Option<Vec<f64>>,
    pub gt: Option<Vec<f64>>,
    pub gte: Option<Vec<f64>>,
    pub lt: Option<Vec<f64>>,
    pub lte: Option<Vec<f64>>,
}

impl NumberFilter {
    pub fn validate(&self) -> Result<(), AppError> {
        let groups = [
            ("eq", &self.eq),
            ("neq", &self.neq),
            ("gt", &self.gt),
            ("gte", &self.gte),
            ("lt", &self.lt),
            ("lte", &self.lte),
        ];

        for (op, group) in groups {
            if let Some(vals) = group {
                if vals.is_empty() {
                    return Err(AppError::invalid_params(format!(
                        "NumberFilter operator '{op}' cannot be empty"
                    )));
                }
                if vals.iter().any(|v| !v.is_finite()) {
                    return Err(AppError::invalid_params(format!(
                        "NumberFilter operator '{op}' contains non-finite float"
                    )));
                }
            }
        }

        Ok(())
    }

    pub fn matches(&self, val: f64) -> bool {
        if !val.is_finite() {
            return false;
        }
        if let Some(ref targets) = self.eq
            && !targets.iter().any(|t| (val - t).abs() <= 1e-9)
        {
            return false;
        }
        if let Some(ref targets) = self.neq
            && !targets.iter().all(|t| (val - t).abs() > 1e-9)
        {
            return false;
        }
        if let Some(ref targets) = self.gt
            && !targets.iter().any(|t| val > *t)
        {
            return false;
        }
        if let Some(ref targets) = self.gte
            && !targets.iter().any(|t| val >= *t)
        {
            return false;
        }
        if let Some(ref targets) = self.lt
            && !targets.iter().any(|t| val < *t)
        {
            return false;
        }
        if let Some(ref targets) = self.lte
            && !targets.iter().any(|t| val <= *t)
        {
            return false;
        }
        true
    }

    pub fn matches_opt(&self, val: Option<f64>) -> bool {
        match val {
            Some(v) => self.matches(v),
            None => false,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct StringFilter {
    pub eq: Option<Vec<String>>,
    pub contains: Option<Vec<String>>,
    #[serde(rename = "startsWith", alias = "starts_with")]
    pub starts_with: Option<Vec<String>>,
}

impl StringFilter {
    pub fn validate(&self) -> Result<(), AppError> {
        let groups = [
            ("eq", &self.eq),
            ("contains", &self.contains),
            ("startsWith", &self.starts_with),
        ];

        for (op, group) in groups {
            if let Some(vals) = group
                && (vals.is_empty() || vals.iter().any(|s| s.trim().is_empty()))
            {
                return Err(AppError::invalid_params(format!(
                    "StringFilter operator '{op}' cannot be empty or contain blank strings"
                )));
            }
        }

        Ok(())
    }

    pub fn matches(&self, raw_value: &str) -> bool {
        let value_norm = normalize_text(raw_value.trim());

        if let Some(ref targets) = self.eq
            && !targets
                .iter()
                .any(|t| value_norm == normalize_text(t.trim()))
        {
            return false;
        }

        if let Some(ref targets) = self.contains
            && !targets
                .iter()
                .any(|t| value_norm.contains(&normalize_text(t.trim())))
        {
            return false;
        }

        if let Some(ref targets) = self.starts_with
            && !targets
                .iter()
                .any(|t| value_norm.starts_with(&normalize_text(t.trim())))
        {
            return false;
        }

        true
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct BooleanFilter {
    pub eq: Option<bool>,
    pub neq: Option<bool>,
}

impl BooleanFilter {
    pub fn matches(&self, val: bool) -> bool {
        self.eq.is_none_or(|e| val == e) && self.neq.is_none_or(|n| val != n)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryFilter {
    #[serde(alias = "school_year")]
    pub school_year: Option<NumberFilter>,
    #[serde(alias = "target_bimester")]
    pub target_bimester: Option<NumberFilter>,
    #[serde(alias = "subject_name")]
    pub subject_name: Option<StringFilter>,
    #[serde(alias = "class_average")]
    pub class_average: Option<NumberFilter>,
    #[serde(alias = "is_recovery")]
    pub is_recovery: Option<BooleanFilter>,
    pub grade: Option<NumberFilter>,
    #[serde(alias = "recovery_code")]
    pub recovery_code: Option<StringFilter>,
    #[serde(alias = "absences")]
    pub absences: Option<NumberFilter>,
    #[serde(alias = "total_absences")]
    pub total_absences: Option<NumberFilter>,
    #[serde(alias = "final_grade")]
    pub final_grade: Option<NumberFilter>,
    #[serde(alias = "delta")]
    pub delta: Option<NumberFilter>,
    #[serde(alias = "posted_only")]
    pub posted_only: Option<BooleanFilter>,
}

enum ParsedFilterKey<'a> {
    Field(&'a str, &'a str),
    RoutingParam,
}

fn parse_filter_key(key: &str) -> Result<ParsedFilterKey<'_>, AppError> {
    let trimmed = key.trim();
    if trimmed == "account_id" || trimmed == "discord_id" {
        return Ok(ParsedFilterKey::RoutingParam);
    }

    if let Some((field, rest)) = trimmed.split_once('[') {
        let Some(op) = rest.strip_suffix(']') else {
            return Err(AppError::invalid_params(format!(
                "Malformed filter operator bracket in key '{trimmed}'"
            )));
        };
        if op.is_empty() || op.contains('[') || op.contains(']') {
            return Err(AppError::invalid_params(format!(
                "Invalid filter operator in key '{trimmed}'"
            )));
        }
        Ok(ParsedFilterKey::Field(field, op))
    } else {
        Ok(ParsedFilterKey::Field(trimmed, "eq"))
    }
}

fn split_non_empty_list(raw: &str, key: &str) -> Result<Vec<String>, AppError> {
    if raw.trim().is_empty() {
        return Err(AppError::invalid_params(format!(
            "Filter value for '{key}' cannot be empty"
        )));
    }
    let mut items = Vec::new();
    for seg in raw.split(',') {
        let trimmed = seg.trim();
        if trimmed.is_empty() {
            return Err(AppError::invalid_params(format!(
                "Filter list for '{key}' contains an empty item"
            )));
        }
        items.push(trimmed.to_string());
    }
    Ok(items)
}

fn parse_number_list(raw: &str, key: &str) -> Result<Vec<f64>, AppError> {
    let items = split_non_empty_list(raw, key)?;
    let mut nums = Vec::with_capacity(items.len());
    for item in items {
        let val = item.parse::<f64>().map_err(|_| {
            AppError::invalid_params(format!("Invalid numeric filter value '{item}' for '{key}'"))
        })?;
        if !val.is_finite() {
            return Err(AppError::invalid_params(format!(
                "Non-finite numeric filter value '{item}' for '{key}'"
            )));
        }
        nums.push(val);
    }
    Ok(nums)
}

fn parse_bool_strict(raw: &str, key: &str) -> Result<bool, AppError> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "true" | "1" => Ok(true),
        "false" | "0" => Ok(false),
        _ => Err(AppError::invalid_params(format!(
            "Invalid boolean filter value '{raw}' for '{key}'"
        ))),
    }
}

fn apply_number_op(
    target: &mut Option<NumberFilter>,
    op: &str,
    raw_val: &str,
    full_key: &str,
) -> Result<(), AppError> {
    let nums = parse_number_list(raw_val, full_key)?;
    let nf = target.get_or_insert_with(NumberFilter::default);
    let slot = match op {
        "eq" => &mut nf.eq,
        "neq" => &mut nf.neq,
        "gt" => &mut nf.gt,
        "gte" => &mut nf.gte,
        "lt" => &mut nf.lt,
        "lte" => &mut nf.lte,
        _ => {
            return Err(AppError::invalid_params(format!(
                "Unsupported numeric operator '{op}' in '{full_key}'"
            )));
        }
    };
    slot.get_or_insert_with(Vec::new).extend(nums);
    Ok(())
}

fn apply_string_op(
    target: &mut Option<StringFilter>,
    op: &str,
    raw_val: &str,
    full_key: &str,
) -> Result<(), AppError> {
    let vals = split_non_empty_list(raw_val, full_key)?;
    let sf = target.get_or_insert_with(StringFilter::default);
    let slot = match op {
        "eq" => &mut sf.eq,
        "contains" => &mut sf.contains,
        "startsWith" | "starts_with" => &mut sf.starts_with,
        _ => {
            return Err(AppError::invalid_params(format!(
                "Unsupported string operator '{op}' in '{full_key}'"
            )));
        }
    };
    slot.get_or_insert_with(Vec::new).extend(vals);
    Ok(())
}

fn apply_boolean_op(
    target: &mut Option<BooleanFilter>,
    op: &str,
    raw_val: &str,
    full_key: &str,
) -> Result<(), AppError> {
    let b = parse_bool_strict(raw_val, full_key)?;
    let bf = target.get_or_insert_with(BooleanFilter::default);
    match op {
        "eq" => bf.eq = Some(b),
        "neq" => bf.neq = Some(b),
        _ => {
            return Err(AppError::invalid_params(format!(
                "Unsupported boolean operator '{op}' in '{full_key}'"
            )));
        }
    }
    Ok(())
}

impl QueryFilter {
    pub fn validate(&self) -> Result<(), AppError> {
        if let Some(ref f) = self.school_year {
            f.validate()?;
        }
        if let Some(ref f) = self.target_bimester {
            f.validate()?;
        }
        if let Some(ref f) = self.subject_name {
            f.validate()?;
        }
        if let Some(ref f) = self.class_average {
            f.validate()?;
        }
        if let Some(ref f) = self.grade {
            f.validate()?;
        }
        if let Some(ref f) = self.recovery_code {
            f.validate()?;
        }
        if let Some(ref f) = self.absences {
            f.validate()?;
        }
        if let Some(ref f) = self.total_absences {
            f.validate()?;
        }
        if let Some(ref f) = self.final_grade {
            f.validate()?;
        }
        if let Some(ref f) = self.delta {
            f.validate()?;
        }
        Ok(())
    }

    pub fn from_query_pairs<I, K, V>(pairs: I) -> Result<Self, AppError>
    where
        I: IntoIterator<Item = (K, V)>,
        K: AsRef<str>,
        V: AsRef<str>,
    {
        let mut filter = Self::default();

        for (raw_key, raw_val) in pairs {
            let key = raw_key.as_ref();
            let val = raw_val.as_ref();

            match parse_filter_key(key)? {
                ParsedFilterKey::RoutingParam => continue,
                ParsedFilterKey::Field(field, op) => match field {
                    "schoolYear" | "school_year" => {
                        apply_number_op(&mut filter.school_year, op, val, key)?;
                    }
                    "targetBimester" | "target_bimester" => {
                        apply_number_op(&mut filter.target_bimester, op, val, key)?;
                    }
                    "classAverage" | "class_average" => {
                        apply_number_op(&mut filter.class_average, op, val, key)?;
                    }
                    "grade" => {
                        apply_number_op(&mut filter.grade, op, val, key)?;
                    }
                    "absences" => {
                        apply_number_op(&mut filter.absences, op, val, key)?;
                    }
                    "totalAbsences" | "total_absences" => {
                        apply_number_op(&mut filter.total_absences, op, val, key)?;
                    }
                    "finalGrade" | "final_grade" => {
                        apply_number_op(&mut filter.final_grade, op, val, key)?;
                    }
                    "delta" => {
                        apply_number_op(&mut filter.delta, op, val, key)?;
                    }
                    "subjectName" | "subject_name" => {
                        apply_string_op(&mut filter.subject_name, op, val, key)?;
                    }
                    "recoveryCode" | "recovery_code" => {
                        apply_string_op(&mut filter.recovery_code, op, val, key)?;
                    }
                    "isRecovery" | "is_recovery" => {
                        apply_boolean_op(&mut filter.is_recovery, op, val, key)?;
                    }
                    "postedOnly" | "posted_only" => {
                        apply_boolean_op(&mut filter.posted_only, op, val, key)?;
                    }
                    _ => {
                        return Err(AppError::invalid_params(format!(
                            "Unknown boletim filter parameter '{field}'"
                        )));
                    }
                },
            }
        }

        filter.validate()?;
        Ok(filter)
    }
}

pub fn is_bimester_posted(b: &UnifiedBimesterData) -> bool {
    let p = b.personal.grade;
    let c = b.class.average_grade;
    if p.is_none() && c.is_none() {
        return false;
    }
    if !b.personal.recovery
        && p.unwrap_or(0.0).abs() <= 1e-9
        && c.unwrap_or(0.0).abs() <= 1e-9
    {
        return false;
    }
    p.is_some()
}

fn bimester_matches(b: &UnifiedBimesterData, query: &QueryFilter) -> bool {
    let posted = is_bimester_posted(b);
    let strict_posted = query.posted_only.as_ref().and_then(|p| p.eq).unwrap_or(false);
    let eff_grade = if strict_posted && !posted {
        None
    } else {
        b.personal.grade
    };
    let eff_class = if strict_posted && !posted {
        None
    } else {
        b.class.average_grade
    };

    if let Some(ref pf) = query.posted_only
        && !pf.matches(posted)
    {
        return false;
    }

    if let Some(ref bf) = query.target_bimester
        && !bf.matches(f64::from(b.bimester))
    {
        return false;
    }

    if let Some(ref gf) = query.grade
        && !gf.matches_opt(eff_grade)
    {
        return false;
    }

    // PF-01 fix: read query.class_average and compare against effective class average.
    if let Some(ref caf) = query.class_average
        && !caf.matches_opt(eff_class)
    {
        return false;
    }

    if let Some(ref af) = query.absences
        && !af.matches(f64::from(b.personal.absences))
    {
        return false;
    }

    if let Some(ref df) = query.delta {
        let delta_val = match (
            b.personal.grade.filter(|_| posted),
            b.class.average_grade.filter(|_| posted),
        ) {
            (Some(p), Some(c)) => Some(p - c),
            _ => None,
        };
        if !df.matches_opt(delta_val) {
            return false;
        }
    }

    // PF-EXTRA-01 fix: BooleanFilter eq/neq.
    if let Some(ref rf) = query.is_recovery
        && !rf.matches(b.personal.recovery)
    {
        return false;
    }

    if let Some(ref rcf) = query.recovery_code {
        let code = b
            .personal
            .recovery_code
            .as_ref()
            .map(RecoveryStatusCode::as_code_str)
            .unwrap_or("NAC");
        if !rcf.matches(code) {
            return false;
        }
    }

    true
}

// PF-02 fix: pure immutable transform taking &AllYearsResponse and returning owned filtered data.
pub fn filter_query(
    data: &AllYearsResponse,
    query: &QueryFilter,
) -> Result<AllYearsResponse, AppError> {
    query.validate()?;

    let mut filtered_years = Vec::new();

    for year_info in &data.data {
        // PF-EXTRA-03 fix: match year_info.year rather than array index.
        if let Some(ref yf) = query.school_year
            && !yf.matches(f64::from(year_info.year))
        {
            continue;
        }

        let mut filtered_grades = Vec::new();

        for subject in &year_info.grades {
            // PF-03 fix: accent-insensitive & lowercase comparison on both subject and filter.
            if let Some(ref sf) = query.subject_name
                && !sf.matches(&subject.subject_name)
            {
                continue;
            }

            if let Some(ref taf) = query.total_absences
                && !taf.matches(f64::from(subject.results.total_absences))
            {
                continue;
            }

            if let Some(ref fgf) = query.final_grade
                && !fgf.matches_opt(subject.results.grade)
            {
                continue;
            }

            let filtered_bimesters: Vec<UnifiedBimesterData> = subject
                .bimesters
                .iter()
                .filter(|b| bimester_matches(b, query))
                .cloned()
                .collect();

            if filtered_bimesters.is_empty() {
                continue;
            }

            filtered_grades.push(FullGrades {
                subject_name: subject.subject_name.clone(),
                results: subject.results.clone(),
                bimesters: filtered_bimesters,
            });
        }

        if filtered_grades.is_empty() {
            continue;
        }

        filtered_years.push(YearInfo {
            title: year_info.title.clone(),
            year: year_info.year,
            status: year_info.status.clone(),
            grades: filtered_grades,
            bimesters_metrics: year_info.bimesters_metrics.clone(),
        });
    }

    Ok(AllYearsResponse {
        warning: data.warning.clone(),
        user_current_year: data.user_current_year,
        data: filtered_years,
    })
}
