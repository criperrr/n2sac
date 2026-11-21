use std::future::Future;
use std::time::Duration;

use reqwest::header::{COOKIE, HeaderMap, LOCATION, REFERER, SET_COOKIE};
use reqwest::{StatusCode, redirect};
use url::Url;

use crate::error::AppError;
use crate::models::{
    AllYearsResponse, AtrasoRecord, FichaAvaliacaoDetail, FichaAvaliacaoEntry, HomeInfo,
    NsacSessionCookies, PartialSessionCookies, SaidaRecord,
};
use crate::parsers::{
    extract_csrf_token, looks_like_login_page, parse_atrasos, parse_boletim,
    parse_ficha_avaliacao_detail, parse_ficha_avaliacao_list, parse_home, parse_saidas,
};

const DEFAULT_TIMEOUT_SECS: u64 = 12;
const MAX_LOGIN_REDIRECT_HOPS: usize = 3;

#[derive(Clone)]
pub struct NsacClient {
    http: reqwest::Client,
    base_url: String,
}

enum RawRouteResponse {
    Ok {
        body: String,
        rotated: Option<NsacSessionCookies>,
    },
    /// Binary PDF body (the ficha-avaliacao detail is served as a dompdf PDF).
    Pdf {
        bytes: Vec<u8>,
        rotated: Option<NsacSessionCookies>,
    },
    Redirect {
        location: String,
        rotated: Option<NsacSessionCookies>,
    },
}

fn collect_set_cookie_strs(headers: &HeaderMap) -> Vec<&str> {
    headers
        .get_all(SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .collect()
}

fn apply_set_cookies_to_partial(headers: &HeaderMap, partial: &mut PartialSessionCookies) {
    for raw in collect_set_cookie_strs(headers) {
        if let Some((name, value)) = crate::models::parse_set_cookie_line(raw) {
            partial.apply_pair(name, value);
        }
    }
}

fn same_origin(a: &Url, b: &Url) -> bool {
    a.scheme() == b.scheme()
        && a.host_str() == b.host_str()
        && a.port_or_known_default() == b.port_or_known_default()
}

fn location_header_str(headers: &HeaderMap) -> Option<&str> {
    headers.get(LOCATION).and_then(|v| v.to_str().ok())
}

fn location_points_to_login(location: &str) -> bool {
    let raw = location.trim();
    if raw.is_empty() {
        return false;
    }

    let path = if let Ok(parsed) = Url::parse(raw) {
        parsed.path().to_ascii_lowercase()
    } else {
        raw.split(['?', '#'])
            .next()
            .unwrap_or("")
            .to_ascii_lowercase()
    };

    let normalized = path.trim_end_matches('/');
    normalized == "/nsac"
        || normalized == "nsac"
        || normalized.ends_with("/nsac")
        || normalized == "/login"
        || normalized == "login"
        || normalized.ends_with("/login")
}

fn location_points_to_ficha_list(location: &str) -> bool {
    let trimmed = location
        .trim()
        .split('?')
        .next()
        .unwrap_or("")
        .trim_end_matches('/')
        .to_ascii_lowercase();
    // The list itself, or another ficha (the NSAC redirects a not-yet-released bimester to an
    // available one, e.g. /ficha-avaliacao/2 → /ficha-avaliacao/1).
    let tail = trimmed.rsplit("/ficha-avaliacao").next().unwrap_or("x");
    trimmed.contains("/ficha-avaliacao")
        && (tail.is_empty() || tail.trim_start_matches('/').chars().all(|c| c.is_ascii_digit()))
}

impl NsacClient {
    pub fn new(base_url: impl Into<String>) -> Result<Self, AppError> {
        Self::with_timeout(base_url, Duration::from_secs(DEFAULT_TIMEOUT_SECS))
    }

    pub fn with_timeout(base_url: impl Into<String>, timeout: Duration) -> Result<Self, AppError> {
        let normalized = base_url.into().trim().trim_end_matches('/').to_string();
        if normalized.is_empty() {
            return Err(AppError::invalid_params("NSAC base_url cannot be empty"));
        }

        let parsed = Url::parse(&normalized)
            .map_err(|_| AppError::invalid_params("Invalid NSAC base_url"))?;
        if parsed.scheme() != "http" && parsed.scheme() != "https" {
            return Err(AppError::invalid_params(
                "NSAC base_url must use http or https scheme",
            ));
        }

        let connect_timeout = timeout.min(Duration::from_secs(5));
        let http = reqwest::Client::builder()
            .redirect(redirect::Policy::none())
            .timeout(timeout)
            .connect_timeout(connect_timeout)
            .user_agent("nsac-server/2.0")
            .build()
            .map_err(|e| {
                AppError::with_source(
                    crate::error::ErrorCode::InternalError,
                    "Failed to build reqwest HTTP client",
                    e,
                )
            })?;

        Ok(Self {
            http,
            base_url: normalized,
        })
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    fn route_url(&self, path: &str) -> String {
        let trimmed = path.trim_start_matches('/');
        if trimmed.is_empty() {
            self.base_url.clone()
        } else {
            format!("{}/{}", self.base_url, trimmed)
        }
    }

    async fn fetch_login_page(
        &self,
    ) -> Result<(String, NsacSessionCookies, String), AppError> {
        let mut current_url = Url::parse(&self.base_url)
            .map_err(|_| AppError::invalid_params("Invalid NSAC base_url"))?;
        let origin_url = current_url.clone();
        let mut partial = PartialSessionCookies::default();

        for hop in 0..=MAX_LOGIN_REDIRECT_HOPS {
            let response = self
                .http
                .get(current_url.as_str())
                .send()
                .await
                .map_err(|e| {
                    AppError::with_source(
                        crate::error::ErrorCode::NsacUnavailable,
                        "Failed to GET NSAC login page",
                        e,
                    )
                })?;

            apply_set_cookies_to_partial(response.headers(), &mut partial);
            let status = response.status();

            if status.is_redirection() {
                if hop == MAX_LOGIN_REDIRECT_HOPS {
                    return Err(AppError::nsac_unavailable(
                        "Too many redirects when loading NSAC login page",
                    ));
                }

                let location = location_header_str(response.headers()).ok_or_else(|| {
                    AppError::nsac_unavailable("NSAC login redirect missing Location header")
                })?;

                let next_url = current_url.join(location).map_err(|_| {
                    AppError::nsac_unavailable("Invalid Location URL in NSAC login redirect")
                })?;

                if !same_origin(&origin_url, &next_url) {
                    return Err(AppError::nsac_unavailable(
                        "Refusing cross-origin redirect from NSAC login page",
                    ));
                }

                current_url = next_url;
                continue;
            }

            if status != StatusCode::OK {
                return Err(AppError::nsac_unavailable(format!(
                    "NSAC login page returned unexpected HTTP status {status}"
                )));
            }

            let html = response.text().await.map_err(|e| {
                AppError::with_source(
                    crate::error::ErrorCode::NsacUnavailable,
                    "Failed to read NSAC login page body",
                    e,
                )
            })?;

            let csrf = extract_csrf_token(&html)?;
            let cookies = partial.try_into_session()?;
            return Ok((csrf, cookies, current_url.to_string()));
        }

        Err(AppError::nsac_unavailable(
            "Exceeded maximum redirects fetching NSAC login page",
        ))
    }

    pub async fn login(
        &self,
        email: &str,
        password: &str,
    ) -> Result<NsacSessionCookies, AppError> {
        if email.trim().is_empty() || password.is_empty() {
            return Err(AppError::invalid_params(
                "Email and password are required for NSAC login",
            ));
        }

        let (csrf_token, mut session_cookies, referer_url) = self.fetch_login_page().await?;
        let cookie_header = session_cookies.to_cookie_header();

        let post_response = self
            .http
            .post(self.route_url("login"))
            .header(REFERER, referer_url)
            .header(COOKIE, cookie_header.as_str())
            .form(&[
                ("_token", csrf_token.as_str()),
                ("email", email),
                ("password", password),
                ("remember", "on"),
            ])
            .send()
            .await
            .map_err(|e| {
                AppError::with_source(
                    crate::error::ErrorCode::NsacUnavailable,
                    "Failed to POST NSAC login form",
                    e,
                )
            })?;

        let status = post_response.status();
        if status.is_server_error() {
            return Err(AppError::nsac_unavailable(format!(
                "NSAC login returned server error {status}"
            )));
        }

        if status == StatusCode::OK
            || status == StatusCode::UNAUTHORIZED
            || status == StatusCode::UNPROCESSABLE_ENTITY
        {
            return Err(AppError::invalid_credentials(format!(
                "NSAC login rejected credentials with HTTP status {status}"
            )));
        }

        if !status.is_redirection() {
            return Err(AppError::nsac_unavailable(format!(
                "Unexpected HTTP status {status} from NSAC login"
            )));
        }

        if let Some(location) = location_header_str(post_response.headers())
            && location_points_to_login(location)
        {
            return Err(AppError::invalid_credentials(
                "NSAC redirected back to login page after credential submission",
            ));
        }

        let set_cookies = collect_set_cookie_strs(post_response.headers());
        session_cookies.merge_set_cookie_headers(set_cookies);

        // Confirm the newly issued cookies actually grant access to /home.
        let verify_cookie_header = session_cookies.to_cookie_header();
        let home_resp = self
            .http
            .get(self.route_url("home"))
            .header(COOKIE, verify_cookie_header.as_str())
            .send()
            .await
            .map_err(|e| {
                AppError::with_source(
                    crate::error::ErrorCode::NsacUnavailable,
                    "Failed to verify NSAC session on /home after login",
                    e,
                )
            })?;

        let home_status = home_resp.status();
        if home_status.is_redirection()
            || home_status == StatusCode::UNAUTHORIZED
            || home_status == StatusCode::FORBIDDEN
        {
            return Err(AppError::invalid_credentials(
                "NSAC /home rejected session immediately after login",
            ));
        }

        if home_status != StatusCode::OK {
            return Err(AppError::nsac_unavailable(format!(
                "NSAC /home returned unexpected HTTP status {home_status} after login"
            )));
        }

        let home_set_cookies = collect_set_cookie_strs(home_resp.headers());
        session_cookies.merge_set_cookie_headers(home_set_cookies);

        let home_body = home_resp.text().await.map_err(|e| {
            AppError::with_source(
                crate::error::ErrorCode::NsacUnavailable,
                "Failed to read NSAC /home verification body",
                e,
            )
        })?;
        if looks_like_login_page(&home_body) {
            return Err(AppError::invalid_credentials(
                "NSAC /home rendered login form after login",
            ));
        }

        Ok(session_cookies)
    }

    pub async fn verify_cookie(
        &self,
        cookies: &NsacSessionCookies,
    ) -> Result<NsacSessionCookies, AppError> {
        type NoopRefresh = fn() -> std::future::Ready<Result<NsacSessionCookies, AppError>>;
        self.verify_cookie_with_refresh(cookies, Option::<NoopRefresh>::None)
            .await
    }

    pub async fn verify_cookie_with_refresh<F, Fut>(
        &self,
        cookies: &NsacSessionCookies,
        refresh_fn: Option<F>,
    ) -> Result<NsacSessionCookies, AppError>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<NsacSessionCookies, AppError>>,
    {
        let cookie_header = cookies.to_cookie_header();
        let response = self
            .http
            .get(self.route_url("home"))
            .header(COOKIE, cookie_header.as_str())
            .send()
            .await
            .map_err(|e| {
                AppError::with_source(
                    crate::error::ErrorCode::NsacUnavailable,
                    "Failed to verify NSAC cookie against /home",
                    e,
                )
            })?;

        let status = response.status();
        if status == StatusCode::OK {
            let rotated =
                cookies.merged_with_set_cookie_headers(collect_set_cookie_strs(response.headers()));
            let body = response.text().await.map_err(|e| {
                AppError::with_source(
                    crate::error::ErrorCode::NsacUnavailable,
                    "Failed to read NSAC /home body during cookie verification",
                    e,
                )
            })?;

            if !looks_like_login_page(&body) {
                return Ok(rotated.unwrap_or_else(|| cookies.clone()));
            }
            // If 200 OK rendered the login page, treat session as expired and attempt refresh.
        } else if !status.is_redirection()
            && status != StatusCode::UNAUTHORIZED
            && status != StatusCode::FORBIDDEN
        {
            return Err(AppError::nsac_unavailable(format!(
                "NSAC /home returned unexpected HTTP status {status} during verification"
            )));
        }

        if let Some(refresh) = refresh_fn {
            refresh().await
        } else {
            Err(AppError::auth_expired(
                "NSAC session cookie expired and no refresh callback was provided",
            ))
        }
    }

    async fn get_route_raw(
        &self,
        cookies: &NsacSessionCookies,
        path: &str,
    ) -> Result<RawRouteResponse, AppError> {
        let cookie_header = cookies.to_cookie_header();
        let response = self
            .http
            .get(self.route_url(path))
            .header(COOKIE, cookie_header.as_str())
            .send()
            .await
            .map_err(|e| {
                AppError::with_source(
                    crate::error::ErrorCode::NsacUnavailable,
                    format!("Failed to GET NSAC route '{path}'"),
                    e,
                )
            })?;

        let status = response.status();
        let rotated =
            cookies.merged_with_set_cookie_headers(collect_set_cookie_strs(response.headers()));

        if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
            return Err(AppError::auth_expired(format!(
                "NSAC route '{path}' returned {status}"
            )));
        }

        if status.is_redirection() {
            let location = location_header_str(response.headers())
                .unwrap_or_default()
                .to_string();
            if location_points_to_login(&location) || location.is_empty() {
                return Err(AppError::auth_expired(format!(
                    "NSAC route '{path}' redirected to login"
                )));
            }
            return Ok(RawRouteResponse::Redirect { location, rotated });
        }

        if status != StatusCode::OK {
            return Err(AppError::nsac_unavailable(format!(
                "NSAC route '{path}' returned unexpected HTTP status {status}"
            )));
        }

        let is_pdf_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.trim().to_ascii_lowercase().starts_with("application/pdf"));
        let bytes = response.bytes().await.map_err(|e| {
            AppError::with_source(
                crate::error::ErrorCode::NsacUnavailable,
                format!("Failed to read NSAC route '{path}' response body"),
                e,
            )
        })?;
        // Reading as text would mangle binary bodies: PDFs are returned as raw bytes.
        if is_pdf_type || bytes.starts_with(b"%PDF-") {
            return Ok(RawRouteResponse::Pdf { bytes: bytes.to_vec(), rotated });
        }
        let body = String::from_utf8_lossy(&bytes).into_owned();

        if looks_like_login_page(&body) {
            return Err(AppError::auth_expired(format!(
                "NSAC route '{path}' returned login HTML with 200 OK"
            )));
        }

        Ok(RawRouteResponse::Ok { body, rotated })
    }

    pub async fn fetch_home(
        &self,
        cookies: &NsacSessionCookies,
    ) -> Result<(HomeInfo, Option<NsacSessionCookies>), AppError> {
        match self.get_route_raw(cookies, "home").await? {
            RawRouteResponse::Ok { body, rotated } => Ok((parse_home(&body)?, rotated)),
            RawRouteResponse::Pdf { .. } => Err(AppError::nsac_unavailable("Unexpected PDF response")),
            RawRouteResponse::Redirect { .. } => Err(AppError::auth_expired(
                "NSAC /home returned unexpected redirect",
            )),
        }
    }

    pub async fn fetch_boletim(
        &self,
        cookies: &NsacSessionCookies,
    ) -> Result<(AllYearsResponse, Option<NsacSessionCookies>), AppError> {
        match self.get_route_raw(cookies, "aluno/boletim").await? {
            RawRouteResponse::Ok { body, rotated } => Ok((parse_boletim(&body)?, rotated)),
            RawRouteResponse::Pdf { .. } => Err(AppError::nsac_unavailable("Unexpected PDF response")),
            RawRouteResponse::Redirect { .. } => Err(AppError::auth_expired(
                "NSAC /aluno/boletim returned unexpected redirect",
            )),
        }
    }

    pub async fn fetch_atrasos(
        &self,
        cookies: &NsacSessionCookies,
    ) -> Result<(Vec<AtrasoRecord>, Option<NsacSessionCookies>), AppError> {
        match self.get_route_raw(cookies, "aluno/atrasos").await? {
            RawRouteResponse::Ok { body, rotated } => Ok((parse_atrasos(&body)?, rotated)),
            RawRouteResponse::Pdf { .. } => Err(AppError::nsac_unavailable("Unexpected PDF response")),
            RawRouteResponse::Redirect { .. } => Err(AppError::auth_expired(
                "NSAC /aluno/atrasos returned unexpected redirect",
            )),
        }
    }

    pub async fn fetch_saidas(
        &self,
        cookies: &NsacSessionCookies,
    ) -> Result<(Vec<SaidaRecord>, Option<NsacSessionCookies>), AppError> {
        match self.get_route_raw(cookies, "aluno/saidas").await? {
            RawRouteResponse::Ok { body, rotated } => Ok((parse_saidas(&body)?, rotated)),
            RawRouteResponse::Pdf { .. } => Err(AppError::nsac_unavailable("Unexpected PDF response")),
            RawRouteResponse::Redirect { .. } => Err(AppError::auth_expired(
                "NSAC /aluno/saidas returned unexpected redirect",
            )),
        }
    }

    pub async fn fetch_ficha_avaliacao(
        &self,
        cookies: &NsacSessionCookies,
    ) -> Result<(Vec<FichaAvaliacaoEntry>, Option<NsacSessionCookies>), AppError> {
        match self.get_route_raw(cookies, "aluno/ficha-avaliacao").await? {
            RawRouteResponse::Ok { body, rotated } => {
                Ok((parse_ficha_avaliacao_list(&body)?, rotated))
            }
            RawRouteResponse::Pdf { .. } => Err(AppError::nsac_unavailable("Unexpected PDF response")),
            RawRouteResponse::Redirect { .. } => Err(AppError::auth_expired(
                "NSAC /aluno/ficha-avaliacao returned unexpected redirect",
            )),
        }
    }

    pub async fn fetch_ficha_avaliacao_detail(
        &self,
        cookies: &NsacSessionCookies,
        id: u8,
    ) -> Result<(Option<FichaAvaliacaoDetail>, Option<NsacSessionCookies>), AppError> {
        if !(1..=4).contains(&id) {
            return Err(AppError::invalid_params(
                "Invalid ficha-avaliacao bimester id: must be between 1 and 4",
            ));
        }

        let route = format!("aluno/ficha-avaliacao/{id}");
        match self.get_route_raw(cookies, &route).await? {
            RawRouteResponse::Ok { body, rotated } => {
                let detail = parse_ficha_avaliacao_detail(&body, id)?;
                Ok((Some(detail), rotated))
            }
            RawRouteResponse::Pdf { .. } => Err(AppError::nsac_unavailable(
                "Ficha detail is served as a PDF (use fetch_ficha_avaliacao_pdf)",
            )),
            RawRouteResponse::Redirect { location, rotated } => {
                if location_points_to_ficha_list(&location) {
                    Ok((None, rotated))
                } else {
                    Err(AppError::nsac_unavailable(format!(
                        "Unexpected redirect from '{route}' to '{location}'"
                    )))
                }
            }
        }
    }

    /// The ficha as the NSAC serves it: the original PDF (to open/print as is). `None` when the
    /// bimester isn't released yet (the NSAC redirects to the list or to another ficha).
    pub async fn fetch_ficha_avaliacao_pdf(
        &self,
        cookies: &NsacSessionCookies,
        id: u8,
    ) -> Result<(Option<Vec<u8>>, Option<NsacSessionCookies>), AppError> {
        if !(1..=4).contains(&id) {
            return Err(AppError::invalid_params(
                "Invalid ficha-avaliacao bimester id: must be between 1 and 4",
            ));
        }
        let route = format!("aluno/ficha-avaliacao/{id}");
        match self.get_route_raw(cookies, &route).await? {
            RawRouteResponse::Pdf { bytes, rotated } => Ok((Some(bytes), rotated)),
            RawRouteResponse::Redirect { location, rotated } if location_points_to_ficha_list(&location) => {
                Ok((None, rotated))
            }
            RawRouteResponse::Redirect { location, .. } => Err(AppError::nsac_unavailable(format!(
                "Unexpected redirect from '{route}' to '{location}'"
            ))),
            RawRouteResponse::Ok { .. } => Err(AppError::nsac_unavailable(format!(
                "Expected a PDF from '{route}', got HTML"
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{location_points_to_ficha_list, location_points_to_login};

    #[test]
    fn ficha_redirects_to_list_or_other_ficha_mean_not_released() {
        assert!(location_points_to_ficha_list("http://200.145.153.1/nsac/aluno/ficha-avaliacao"));
        assert!(location_points_to_ficha_list("/nsac/aluno/ficha-avaliacao/"));
        assert!(location_points_to_ficha_list("http://200.145.153.1/nsac/aluno/ficha-avaliacao/1"));
        assert!(!location_points_to_ficha_list("http://200.145.153.1/nsac/aluno/boletim"));
        assert!(!location_points_to_ficha_list("/nsac/aluno/ficha-avaliacao/1/print"));
    }

    #[test]
    fn location_points_to_login_normalizes_paths_and_ignores_query_params() {
        assert!(location_points_to_login("/nsac"));
        assert!(location_points_to_login("/nsac/"));
        assert!(location_points_to_login("http://200.145.153.1/nsac"));
        assert!(location_points_to_login("http://200.145.153.1/nsac?expired=1"));
        assert!(location_points_to_login("/nsac/login"));
        assert!(location_points_to_login("http://200.145.153.1/nsac/login?next=/nsac/home"));
        assert!(location_points_to_login("nsac"));
        assert!(location_points_to_login("login"));

        assert!(!location_points_to_login(""));
        assert!(!location_points_to_login("/nsac/home"));
        assert!(!location_points_to_login("http://200.145.153.1/nsac/home"));
        assert!(!location_points_to_login("/nsac/aluno/ficha-avaliacao"));
        assert!(!location_points_to_login("/nsac/home?next=/nsac"));
        assert!(!location_points_to_login(
            "/nsac/aluno/ficha-avaliacao?next=/login?x=1"
        ));
    }
}
