use std::fmt;

use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

use crate::crypto::SecretString;
use crate::error::AppError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum RecoveryStatusCode {
    Sat,
    Ins,
    Nc,
    #[default]
    Nac,
}

impl RecoveryStatusCode {
    pub const fn as_code_str(&self) -> &'static str {
        match self {
            Self::Sat => "SAT",
            Self::Ins => "INS",
            Self::Nc => "NC",
            Self::Nac => "NAC",
        }
    }

    pub fn from_cell_text(raw: &str) -> Self {
        match raw.trim().to_ascii_uppercase().as_str() {
            "SAT" => Self::Sat,
            "INS" => Self::Ins,
            "NC" | "NCP" => Self::Nc,
            _ => Self::Nac,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum RecoveryMessage {
    #[serde(rename = "Satisfatório")]
    Satisfatorio,
    #[serde(rename = "Insatisfatório")]
    Insatisfatorio,
    #[serde(rename = "Não Compareceu")]
    NaoCompareceu,
    #[serde(rename = "Não Aconteceu", alias = "Não aconteceu")]
    #[default]
    NaoAconteceu,
}

impl RecoveryMessage {
    pub fn from_title_or_code(title: Option<&str>, code: RecoveryStatusCode) -> Self {
        if let Some(raw_title) = title {
            let normalized = raw_title.trim().to_lowercase();
            if normalized.contains("satisfat") && !normalized.contains("insatisfat") {
                return Self::Satisfatorio;
            }
            if normalized.contains("insatisfat") {
                return Self::Insatisfatorio;
            }
            if normalized.contains("compareceu") {
                return Self::NaoCompareceu;
            }
            if normalized.contains("aconteceu") {
                return Self::NaoAconteceu;
            }
        }

        match code {
            RecoveryStatusCode::Sat => Self::Satisfatorio,
            RecoveryStatusCode::Ins => Self::Insatisfatorio,
            RecoveryStatusCode::Nc => Self::NaoCompareceu,
            RecoveryStatusCode::Nac => Self::NaoAconteceu,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PersonalBiInformation {
    pub grade: Option<f64>,
    pub absences: u32,
    pub recovery: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recovered: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recovery_code: Option<RecoveryStatusCode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recovery_message: Option<RecoveryMessage>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClassBiInformation {
    pub average_grade: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UnifiedBimesterData {
    pub bimester: u8,
    pub personal: PersonalBiInformation,
    pub class: ClassBiInformation,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResultData {
    pub grade: Option<f64>,
    pub total_absences: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FullGrades {
    pub subject_name: String,
    pub results: ResultData,
    pub bimesters: Vec<UnifiedBimesterData>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BimesterData {
    pub user_average: f64,
    pub class_average: f64,
    pub total_absences: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct YearInfo {
    pub title: String,
    pub year: u16,
    pub status: String,
    pub grades: Vec<FullGrades>,
    pub bimesters_metrics: Vec<BimesterData>,
}

impl YearInfo {
    pub fn class_code(&self) -> &str {
        &self.title
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BoletimData {
    pub year_count: usize,
    pub years_info: Vec<YearInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum WarningValue {
    Bool(bool),
    Text(String),
}

impl Default for WarningValue {
    fn default() -> Self {
        Self::Bool(false)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AllYearsResponse {
    pub warning: WarningValue,
    pub user_current_year: usize,
    pub data: Vec<YearInfo>,
}

impl AllYearsResponse {
    pub fn current_class_code(&self) -> Option<&str> {
        self.data.last().map(YearInfo::class_code)
    }

    pub fn into_boletim_data(self) -> BoletimData {
        BoletimData {
            year_count: self.data.len(),
            years_info: self.data,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HomeLinkItem {
    pub title: String,
    pub description: String,
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HomeInfo {
    pub portal_date: Option<String>,
    pub available_routes: Vec<String>,
    pub info_links: Vec<HomeLinkItem>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AtrasoRecord {
    pub date: String,
    pub time: Option<String>,
    pub subject: Option<String>,
    pub reason: Option<String>,
    #[serde(default)]
    pub raw_columns: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaidaRecord {
    pub date: String,
    pub time: Option<String>,
    pub reason: Option<String>,
    pub authorized_by: Option<String>,
    #[serde(default)]
    pub raw_columns: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FichaAvaliacaoEntry {
    pub school_year: Option<u16>,
    pub bimester: u8,
    pub last_viewed: Option<String>,
    pub status_text: String,
    pub available: bool,
    pub detail_id: Option<u8>,
    pub detail_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FichaAvaliacaoDetail {
    pub bimester: u8,
    pub title: Option<String>,
    pub tables: Vec<Vec<Vec<String>>>,
    pub notes: Vec<String>,
}

#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct NsacCredentials {
    pub email: String,
    pub password: SecretString,
}

impl fmt::Debug for NsacCredentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NsacCredentials")
            .field("email", &self.email)
            .field("password", &"[REDACTED]")
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq, Zeroize, ZeroizeOnDrop)]
pub struct NsacSessionCookies {
    xsrf_token: SecretString,
    nsaconline_session: SecretString,
    remember_web: Option<(String, SecretString)>,
}

impl fmt::Debug for NsacSessionCookies {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NsacSessionCookies")
            .field("xsrf_token", &"[REDACTED]")
            .field("nsaconline_session", &"[REDACTED]")
            .field("has_remember_web", &self.remember_web.is_some())
            .finish()
    }
}

#[derive(Default)]
pub(crate) struct PartialSessionCookies {
    pub xsrf_token: Option<SecretString>,
    pub nsaconline_session: Option<SecretString>,
    pub remember_web: Option<(String, SecretString)>,
}

impl PartialSessionCookies {
    pub fn apply_pair(&mut self, name: &str, value: &str) -> bool {
        let name = name.trim();
        let value = value.trim();
        if name.is_empty() || value.is_empty() || value.eq_ignore_ascii_case("deleted") {
            return false;
        }

        if name == "XSRF-TOKEN" {
            self.xsrf_token = Some(Zeroizing::new(value.to_string()));
            true
        } else if name == "nsaconline_session" {
            self.nsaconline_session = Some(Zeroizing::new(value.to_string()));
            true
        } else if name.starts_with("remember_web_") {
            self.remember_web = Some((name.to_string(), Zeroizing::new(value.to_string())));
            true
        } else {
            false
        }
    }

    pub fn try_into_session(self) -> Result<NsacSessionCookies, AppError> {
        let xsrf_token = self.xsrf_token.ok_or_else(|| {
            AppError::nsac_unavailable("Missing XSRF-TOKEN cookie in NSAC response")
        })?;
        let nsaconline_session = self.nsaconline_session.ok_or_else(|| {
            AppError::nsac_unavailable("Missing nsaconline_session cookie in NSAC response")
        })?;

        Ok(NsacSessionCookies {
            xsrf_token,
            nsaconline_session,
            remember_web: self.remember_web,
        })
    }
}

pub(crate) fn parse_set_cookie_line(raw: &str) -> Option<(&str, &str)> {
    let mut segments = raw.split(';');
    let first = segments.next()?.trim();
    let (name, value) = first.split_once('=')?;
    let name = name.trim();
    let value = value.trim();
    if name.is_empty() || value.is_empty() || value.eq_ignore_ascii_case("deleted") {
        return None;
    }

    for attr in segments {
        let attr_norm = attr.trim().to_ascii_lowercase();
        if let Some((k, v)) = attr_norm.split_once('=')
            && k.trim() == "max-age"
            && v.trim().parse::<i64>().is_ok_and(|n| n <= 0)
        {
            return None;
        }
    }

    Some((name, value))
}

impl NsacSessionCookies {
    pub fn new(
        xsrf_token: impl Into<String>,
        nsaconline_session: impl Into<String>,
        remember_web: Option<(String, String)>,
    ) -> Result<Self, AppError> {
        let xsrf = xsrf_token.into().trim().to_string();
        let session = nsaconline_session.into().trim().to_string();
        if xsrf.is_empty() || session.is_empty() {
            return Err(AppError::invalid_params(
                "XSRF-TOKEN and nsaconline_session cookies cannot be empty",
            ));
        }

        let remember = remember_web.and_then(|(name, val)| {
            let n = name.trim().to_string();
            let v = val.trim().to_string();
            if n.starts_with("remember_web_") && !v.is_empty() && !v.eq_ignore_ascii_case("deleted") {
                Some((n, Zeroizing::new(v)))
            } else {
                None
            }
        });

        Ok(Self {
            xsrf_token: Zeroizing::new(xsrf),
            nsaconline_session: Zeroizing::new(session),
            remember_web: remember,
        })
    }

    pub fn from_cookie_header(header: &str) -> Result<Self, AppError> {
        let stripped = header
            .trim()
            .strip_prefix("Cookie:")
            .or_else(|| header.trim().strip_prefix("cookie:"))
            .unwrap_or(header);

        let mut partial = PartialSessionCookies::default();
        for segment in stripped.split(';') {
            let trimmed = segment.trim();
            if let Some((name, value)) = trimmed.split_once('=') {
                partial.apply_pair(name, value);
            }
        }

        partial.try_into_session().map_err(|_| {
            AppError::invalid_params(
                "Cookie header missing required XSRF-TOKEN or nsaconline_session",
            )
        })
    }

    pub fn from_set_cookie_headers<'a, I>(headers: I) -> Result<Self, AppError>
    where
        I: IntoIterator<Item = &'a str>,
    {
        let mut partial = PartialSessionCookies::default();
        for raw in headers {
            if let Some((name, value)) = parse_set_cookie_line(raw) {
                partial.apply_pair(name, value);
            }
        }
        partial.try_into_session()
    }

    pub fn merge_set_cookie_headers<'a, I>(&mut self, headers: I) -> bool
    where
        I: IntoIterator<Item = &'a str>,
    {
        let mut partial = PartialSessionCookies::default();
        for raw in headers {
            if let Some((name, value)) = parse_set_cookie_line(raw) {
                partial.apply_pair(name, value);
            }
        }

        let mut changed = false;
        if let Some(xsrf) = partial.xsrf_token
            && *xsrf != *self.xsrf_token
        {
            self.xsrf_token = xsrf;
            changed = true;
        }
        if let Some(sess) = partial.nsaconline_session
            && *sess != *self.nsaconline_session
        {
            self.nsaconline_session = sess;
            changed = true;
        }
        if let Some((name, val)) = partial.remember_web {
            let is_different = self
                .remember_web
                .as_ref()
                .is_none_or(|(cur_name, cur_val)| *cur_name != name || **cur_val != *val);
            if is_different {
                self.remember_web = Some((name, val));
                changed = true;
            }
        }
        changed
    }

    pub fn merged_with_set_cookie_headers<'a, I>(&self, headers: I) -> Option<Self>
    where
        I: IntoIterator<Item = &'a str>,
    {
        let mut cloned = self.clone();
        if cloned.merge_set_cookie_headers(headers) {
            Some(cloned)
        } else {
            None
        }
    }

    pub fn to_cookie_header(&self) -> SecretString {
        match &self.remember_web {
            Some((name, val)) => Zeroizing::new(format!(
                "XSRF-TOKEN={}; nsaconline_session={}; {}={}",
                self.xsrf_token.as_str(),
                self.nsaconline_session.as_str(),
                name,
                val.as_str()
            )),
            None => Zeroizing::new(format!(
                "XSRF-TOKEN={}; nsaconline_session={}",
                self.xsrf_token.as_str(),
                self.nsaconline_session.as_str()
            )),
        }
    }

    pub fn xsrf_token(&self) -> &str {
        &self.xsrf_token
    }

    pub fn session(&self) -> &str {
        &self.nsaconline_session
    }

    pub fn remember_web(&self) -> Option<(&str, &str)> {
        self.remember_web
            .as_ref()
            .map(|(n, v)| (n.as_str(), v.as_str()))
    }

    pub fn has_remember_cookie(&self) -> bool {
        self.remember_web.is_some()
    }
}
