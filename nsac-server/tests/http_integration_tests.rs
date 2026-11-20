use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, Request, StatusCode};
use chrono::{Duration as ChronoDuration, Utc};
use nsac_core::models::{
    AllYearsResponse, ClassBiInformation, FullGrades, PersonalBiInformation, RecoveryMessage,
    RecoveryStatusCode, ResultData, UnifiedBimesterData, WarningValue, YearInfo,
};
use nsac_server::auth::seed_service_clients;
use nsac_server::config::{CacheConfig, Config, FcmConfig, SentinelConfig, ServiceKeySeed};
use nsac_server::db::auth::sha256_hex;
use nsac_server::http::build_router;
use nsac_server::state::AppState;
use serde_json::Value;
use sqlx::postgres::PgPoolOptions;
use sqlx::{PgPool, Row};
use tokio::net::TcpListener;
use tokio::sync::OnceCell;
use tower::ServiceExt;
use url::Url;
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, MockServer, Request as WiremockRequest, Respond, ResponseTemplate};
use zeroize::Zeroizing;

static M2_DB_INIT: OnceCell<String> = OnceCell::const_new();
static M2_SEQ: AtomicU64 = AtomicU64::new(1);

const TEST_ENC_KEY: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fn unique_suffix() -> String {
    let seq = M2_SEQ.fetch_add(1, Ordering::Relaxed);
    format!("{}_{}", std::process::id(), seq)
}

fn resolved_m2_test_db_url() -> String {
    if let Ok(url) = std::env::var("TEST_DATABASE_URL") {
        let trimmed = url.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    "postgres://postgres@127.0.0.1:5432/nsac_m2_test".to_string()
}

fn validate_m2_db_url(db_url: &str) -> (String, String) {
    let parsed = Url::parse(db_url).expect("TEST_DATABASE_URL must be a valid URL");
    let db_name = parsed.path().trim_start_matches('/').to_string();
    assert!(!db_name.is_empty(), "Test DB URL must include DB name");
    assert_ne!(
        db_name, "nsac_e2e",
        "STRICT ISOLATION VIOLATION: M2 tests must NEVER connect to nsac_e2e"
    );
    assert_ne!(
        db_name, "postgres",
        "M2 tests must not run migrations against postgres admin DB"
    );

    let mut admin_url = parsed;
    admin_url.set_path("/postgres");
    (db_name, admin_url.to_string())
}

async fn m2_test_pool() -> PgPool {
    let db_url = M2_DB_INIT
        .get_or_init(|| async {
            let target_url = resolved_m2_test_db_url();
            let (db_name, admin_url) = validate_m2_db_url(&target_url);

            let admin_pool = PgPoolOptions::new()
                .max_connections(2)
                .connect(&admin_url)
                .await
                .expect("Failed to connect to PostgreSQL admin database");

            let exists: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM pg_database WHERE datname = $1)",
            )
            .bind(&db_name)
            .fetch_one(&admin_pool)
            .await
            .expect("Failed to check pg_database");

            if !exists {
                let escaped = db_name.replace('"', "\"\"");
                let create_sql = format!("CREATE DATABASE \"{escaped}\"");
                if let Err(err) = sqlx::query(&create_sql).execute(&admin_pool).await {
                    let still_missing = !sqlx::query_scalar::<_, bool>(
                        "SELECT EXISTS(SELECT 1 FROM pg_database WHERE datname = $1)",
                    )
                    .bind(&db_name)
                    .fetch_one(&admin_pool)
                    .await
                    .unwrap_or(false);
                    if still_missing {
                        panic!("Failed to create M2 test DB {db_name}: {err}");
                    }
                }
            }
            admin_pool.close().await;

            let init_pool = PgPoolOptions::new()
                .max_connections(4)
                .connect(&target_url)
                .await
                .expect("Failed to connect to M2 test database");

            nsac_server::run_migrations(&init_pool)
                .await
                .expect("Failed to apply M2 migrations");
            init_pool.close().await;

            target_url
        })
        .await;

    PgPoolOptions::new()
        .max_connections(5)
        .connect(db_url)
        .await
        .expect("Failed to open M2 test PgPool")
}

fn make_test_config(
    nsac_base_url: String,
    service_keys: Vec<ServiceKeySeed>,
    max_concurrent: usize,
) -> Config {
    Config {
        http_bind: "127.0.0.1:0".parse().expect("valid SocketAddr"),
        rust_log: "warn".to_string(),
        database_url: Zeroizing::new(resolved_m2_test_db_url()),
        encryption_key: Zeroizing::new(TEST_ENC_KEY.to_string()),
        service_api_keys: service_keys,
        user_session_ttl_secs: 3600,
        nsac_base_url,
        nsac_timeout_secs: 5,
        nsac_max_concurrent_requests: max_concurrent,
        sentinel: SentinelConfig {
            enabled: false,
            tick_ms: 10_000,
            cold_baseline_secs: 86_400,
            historical_radius_days: 4,
            historical_interval_secs: 7200,
            hot_min_interval_secs: 120,
            hot_max_interval_secs: 180,
            hot_duration_secs: 7200,
            api_volume_weight: 0.15,
            api_cooldown_secs: 1800,
            midnight_backoff_secs: 1800,
            jitter_max_ms: 1500,
        },
        fcm: FcmConfig {
            enabled: false,
            project_id: None,
            credentials_json: None,
            base_url: None,
            endpoint_url: None,
            oauth_token_url: None,
        },
        cache: CacheConfig {
            boletim_ttl_secs: 300,
            snapshot_ttl_secs: 300,
            auth_ttl_secs: 0,
        },
    }
}

async fn setup_app_with_mock(
    server: &MockServer,
    max_concurrent: usize,
) -> (AppState, Router, String, String, PgPool) {
    let pool = m2_test_pool().await;
    let suffix = unique_suffix();
    let svc_name = format!("m2-bot-{suffix}");
    let svc_key = format!("m2-secret-key-{suffix}");

    let cfg = make_test_config(
        format!("{}/nsac", server.uri()),
        vec![ServiceKeySeed {
            name: svc_name.clone(),
            raw_key: Zeroizing::new(svc_key.clone()),
            scopes: vec![
                "nsac:read".to_string(),
                "nsac:write".to_string(),
                "account:link".to_string(),
            ],
        }],
        max_concurrent,
    );

    let state = AppState::with_default_ports(cfg, pool.clone()).expect("build AppState");
    seed_service_clients(&state)
        .await
        .expect("seed service clients");
    let router = build_router(state.clone());
    (state, router, svc_name, svc_key, pool)
}

async fn mount_standard_nsac_fixtures(server: &MockServer, expected_pass: &str, boletim_html: &str) {
    Mock::given(method("GET"))
        .and(path("/nsac"))
        .respond_with(
            ResponseTemplate::new(200)
                .append_header("Set-Cookie", "XSRF-TOKEN=init_xsrf; Path=/")
                .append_header("Set-Cookie", "nsaconline_session=init_sess; Path=/")
                .set_body_string(include_str!("../../nsac-core/tests/fixtures/login_page.html")),
        )
        .mount(server)
        .await;

    let pass_needle = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("password", expected_pass)
        .finish();
    Mock::given(method("POST"))
        .and(path("/nsac/login"))
        .and(body_string_contains(
            "_token=SYNTHETIC_CSRF_TOKEN_40_CHARACTERS_LONG1",
        ))
        .and(body_string_contains(pass_needle.as_str()))
        .respond_with(
            ResponseTemplate::new(302)
                .insert_header("Location", format!("{}/nsac/home", server.uri()))
                .append_header("Set-Cookie", "XSRF-TOKEN=auth_xsrf_1; Path=/")
                .append_header("Set-Cookie", "nsaconline_session=auth_sess_1; Path=/")
                .append_header(
                    "Set-Cookie",
                    "remember_web_guard_01=rem_tok_initial; Path=/; HttpOnly",
                ),
        )
        .mount(server)
        .await;

    // Fallback wrong-password login route (lower priority because more specific matcher wins)
    Mock::given(method("POST"))
        .and(path("/nsac/login"))
        .respond_with(
            ResponseTemplate::new(302)
                .insert_header("Location", format!("{}/nsac/login", server.uri())),
        )
        .with_priority(10)
        .mount(server)
        .await;

    Mock::given(method("GET"))
        .and(path("/nsac/home"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(include_str!("../../nsac-core/tests/fixtures/home_authenticated.html")),
        )
        .mount(server)
        .await;

    Mock::given(method("GET"))
        .and(path("/nsac/aluno/boletim"))
        .respond_with(ResponseTemplate::new(200).set_body_string(boletim_html))
        .mount(server)
        .await;

    Mock::given(method("GET"))
        .and(path("/nsac/aluno/atrasos"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(include_str!("../../nsac-core/tests/fixtures/atrasos_populated.html")),
        )
        .mount(server)
        .await;

    Mock::given(method("GET"))
        .and(path("/nsac/aluno/saidas"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(include_str!("../../nsac-core/tests/fixtures/saidas_populated.html")),
        )
        .mount(server)
        .await;

    Mock::given(method("GET"))
        .and(path("/nsac/aluno/ficha-avaliacao"))
        .respond_with(ResponseTemplate::new(200).set_body_string(include_str!(
            "../../nsac-core/tests/fixtures/ficha_avaliacao_list.html"
        )))
        .mount(server)
        .await;

    Mock::given(method("GET"))
        .and(path("/nsac/aluno/ficha-avaliacao/2"))
        .respond_with(ResponseTemplate::new(200).set_body_string(include_str!(
            "../../nsac-core/tests/fixtures/ficha_avaliacao_detail.html"
        )))
        .mount(server)
        .await;

    for unavail in [1u8, 3, 4] {
        Mock::given(method("GET"))
            .and(path(format!("/nsac/aluno/ficha-avaliacao/{unavail}")))
            .respond_with(ResponseTemplate::new(302).insert_header(
                "Location",
                format!("{}/nsac/aluno/ficha-avaliacao", server.uri()),
            ))
            .mount(server)
            .await;
    }
}

struct HttpResp {
    status: StatusCode,
    headers: HeaderMap,
    raw_body: Vec<u8>,
}

impl HttpResp {
    fn json(&self) -> Value {
        serde_json::from_slice(&self.raw_body).expect("response body should be valid JSON")
    }

    fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|v| v.to_str().ok())
    }
}

async fn send_req(
    router: &Router,
    method: Method,
    uri: &str,
    headers: Vec<(&str, &str)>,
    body: Option<Vec<u8>>,
) -> HttpResp {
    let mut builder = Request::builder().method(method).uri(uri);
    for (k, v) in headers {
        builder = builder.header(
            HeaderName::from_bytes(k.as_bytes()).expect("header name"),
            HeaderValue::from_str(v).expect("header val"),
        );
    }
    let req_body = match body {
        Some(bytes) => Body::from(bytes),
        None => Body::empty(),
    };
    let req = builder.body(req_body).expect("build request");
    let resp = router.clone().oneshot(req).await.expect("router oneshot");
    let status = resp.status();
    let resp_headers = resp.headers().clone();
    let bytes = to_bytes(resp.into_body(), 4 * 1024 * 1024)
        .await
        .expect("read body");
    HttpResp {
        status,
        headers: resp_headers,
        raw_body: bytes.to_vec(),
    }
}

async fn send_json(
    router: &Router,
    method: Method,
    uri: &str,
    headers: Vec<(&str, &str)>,
    payload: Value,
) -> HttpResp {
    let mut all_headers = headers;
    all_headers.push(("Content-Type", "application/json"));
    send_req(
        router,
        method,
        uri,
        all_headers,
        Some(serde_json::to_vec(&payload).expect("serialize json")),
    )
    .await
}

fn assert_error_json(resp: &HttpResp, expected_status: StatusCode, expected_code: &str) {
    assert_eq!(
        resp.status,
        expected_status,
        "unexpected status {}; body={}",
        resp.status,
        String::from_utf8_lossy(&resp.raw_body)
    );
    let ct = resp.header("content-type").unwrap_or("");
    assert!(
        ct.contains("application/json"),
        "error response must be JSON, got content-type={ct:?}"
    );
    let body = resp.json();
    assert_eq!(body["success"], Value::Bool(false));
    let errors = body["errors"].as_array().expect("errors array");
    assert!(!errors.is_empty());
    assert_eq!(errors[0]["code"], Value::String(expected_code.to_string()));
    assert!(
        !errors[0]["message"]
            .as_str()
            .unwrap_or("")
            .trim()
            .is_empty()
    );
}

fn assert_no_secrets_in_resp(resp: &HttpResp, secrets: &[&str]) {
    let body_text = String::from_utf8_lossy(&resp.raw_body);
    for secret in secrets {
        assert!(
            !body_text.contains(secret),
            "secret {secret:?} leaked in response body: {body_text}"
        );
        for (k, v) in &resp.headers {
            let val = v.to_str().unwrap_or("");
            assert!(
                !val.contains(secret),
                "secret {secret:?} leaked in header {k}: {val}"
            );
        }
    }
}

async fn login_student(
    router: &Router,
    email: &str,
    password: &str,
    discord_id: Option<&str>,
    fcm_token: Option<&str>,
) -> (i64, String, Option<String>) {
    let mut payload = serde_json::json!({
        "nsac_email": email,
        "nsac_password": password,
    });
    if let Some(d) = discord_id {
        payload["discord_id"] = Value::String(d.to_string());
    }
    if let Some(f) = fcm_token {
        payload["fcm_token"] = Value::String(f.to_string());
    }
    let resp = send_json(router, Method::POST, "/api/v1/auth/login", vec![], payload).await;
    assert_eq!(
        resp.status,
        StatusCode::OK,
        "login failed: {}",
        String::from_utf8_lossy(&resp.raw_body)
    );
    assert_no_secrets_in_resp(
        &resp,
        &[password, "auth_xsrf_1", "auth_sess_1", "rem_tok_initial"],
    );
    let json = resp.json();
    assert_eq!(json["success"], Value::Bool(true));
    let data = &json["data"];
    let account_id = data["account_id"].as_i64().expect("account_id");
    let token = data["token"].as_str().expect("token").to_string();
    let class_code = data["class_code"].as_str().map(str::to_string);
    (account_id, token, class_code)
}

#[tokio::test]
async fn health_and_tcp_server_smoke_test() {
    let server = MockServer::start().await;
    let (_state, router, _, _, _pool) = setup_app_with_mock(&server, 4).await;

    let resp = send_req(&router, Method::GET, "/api/v1/health", vec![], None).await;
    assert_eq!(resp.status, StatusCode::OK);
    let body = resp.json();
    assert_eq!(body["success"], Value::Bool(true));
    assert_eq!(body["data"]["status"], Value::String("ok".to_string()));

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind tcp");
    let addr = listener.local_addr().expect("local addr");
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let serve_handle = tokio::spawn(async move {
        let _ = axum::serve(listener, router)
            .with_graceful_shutdown(async {
                let _ = rx.await;
            })
            .await;
    });

    let http_client = reqwest::Client::new();
    let tcp_resp = http_client
        .get(format!("http://{addr}/api/v1/health"))
        .send()
        .await
        .expect("tcp health request");
    assert_eq!(tcp_resp.status(), StatusCode::OK);
    let tcp_json: Value = tcp_resp.json().await.expect("json");
    assert_eq!(tcp_json["success"], Value::Bool(true));

    let _ = tx.send(());
    serve_handle.await.expect("graceful shutdown");
}

#[tokio::test]
async fn login_populates_db_sentinel_boletim_and_logout_revokes_session() {
    let server = MockServer::start().await;
    let pass = "LoginPass_M2_01!";
    mount_standard_nsac_fixtures(
        &server,
        pass,
        include_str!("../../nsac-core/tests/fixtures/boletim_noturno_inf1.html"),
    )
    .await;

    let (state, router, _, _, pool) = setup_app_with_mock(&server, 4).await;
    let suffix = unique_suffix();
    let email = format!("m2_login_{suffix}@unesp.br");

    // Empty & malformed JSON -> 400 JSON envelope
    let empty_resp = send_json(
        &router,
        Method::POST,
        "/api/v1/auth/login",
        vec![],
        serde_json::json!({"nsac_email": "", "nsac_password": ""}),
    )
    .await;
    assert_error_json(&empty_resp, StatusCode::BAD_REQUEST, "INVALID_PARAMS");

    let malformed_resp = send_req(
        &router,
        Method::POST,
        "/api/v1/auth/login",
        vec![("Content-Type", "application/json")],
        Some(b"{not-valid-json".to_vec()),
    )
    .await;
    assert_error_json(&malformed_resp, StatusCode::BAD_REQUEST, "INVALID_PARAMS");

    let missing_field_resp = send_json(
        &router,
        Method::POST,
        "/api/v1/auth/login",
        vec![],
        serde_json::json!({}),
    )
    .await;
    assert_error_json(&missing_field_resp, StatusCode::BAD_REQUEST, "INVALID_PARAMS");

    // Invalid NSAC credentials -> 401 INVALID_NSAC_CREDENTIALS, 0 accounts created
    let bad_email = format!("m2_bad_{suffix}@unesp.br");
    let bad_resp = send_json(
        &router,
        Method::POST,
        "/api/v1/auth/login",
        vec![],
        serde_json::json!({"nsac_email": bad_email, "nsac_password": "WrongPassword!"}),
    )
    .await;
    assert_error_json(&bad_resp, StatusCode::UNAUTHORIZED, "INVALID_NSAC_CREDENTIALS");
    assert_no_secrets_in_resp(&bad_resp, &["WrongPassword!"]);
    let bad_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM accounts WHERE nsac_email = $1")
        .bind(&bad_email)
        .fetch_one(&pool)
        .await
        .expect("count accounts");
    assert_eq!(bad_count, 0);

    // Valid login
    let (account_id, token, class_code) = login_student(&router, &email, pass, None, None).await;
    assert_eq!(class_code.as_deref(), Some("INF1"));

    // Verify DB encryption and hashed session token (zero plaintext secrets)
    let acct_row = sqlx::query("SELECT nsac_pass_enc, nsac_cookies_enc, class_code FROM accounts WHERE id = $1")
        .bind(account_id)
        .fetch_one(&pool)
        .await
        .expect("fetch account row");
    let pass_enc: String = acct_row.get("nsac_pass_enc");
    let cookies_enc: String = acct_row.get("nsac_cookies_enc");
    assert!(pass_enc.starts_with("v1:"));
    assert!(cookies_enc.starts_with("v1:"));
    assert!(!pass_enc.contains(pass));
    assert!(!cookies_enc.contains("auth_sess_1"));

    let token_hash = sha256_hex(&token);
    let sess_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM user_sessions WHERE account_id = $1 AND token_hash = $2 AND revoked_at IS NULL",
    )
    .bind(account_id)
    .bind(&token_hash)
    .fetch_one(&pool)
    .await
    .expect("session count");
    assert_eq!(sess_count, 1);

    // Verify BoletimRepo & sentinel_class_state populated right after login
    let stored_boletim = state
        .boletim()
        .get_boletim(account_id)
        .await
        .expect("get_boletim")
        .expect("boletim populated on login");
    assert_eq!(stored_boletim.data.len(), 1);

    let sentinel = state
        .sentinel()
        .get_class_state("INF1")
        .await
        .expect("get_class_state")
        .expect("sentinel_class_state initialized on login");
    assert!(sentinel.last_class_hash.as_deref().is_some_and(|h| h.len() == 64));
    assert!(sentinel.current_interval_secs > 0);

    // Logout revokes session -> subsequent Bearer request returns 401
    let bearer_val = format!("Bearer {token}");
    let logout_resp = send_req(
        &router,
        Method::POST,
        "/api/v1/auth/logout",
        vec![("Authorization", &bearer_val)],
        None,
    )
    .await;
    assert_eq!(logout_resp.status, StatusCode::OK);
    assert_eq!(logout_resp.json()["data"]["revoked"], Value::Bool(true));

    let after_logout = send_req(
        &router,
        Method::GET,
        "/api/v1/account",
        vec![("Authorization", &bearer_val)],
        None,
    )
    .await;
    assert_error_json(&after_logout, StatusCode::UNAUTHORIZED, "UNAUTHORIZED");
}

#[tokio::test]
async fn dual_auth_isolation_scopes_targets_and_account_endpoints() {
    let server = MockServer::start().await;
    let pass = "DualAuthPass_M2!";
    mount_standard_nsac_fixtures(
        &server,
        pass,
        include_str!("../../nsac-core/tests/fixtures/boletim_diurno_multiyear.html"),
    )
    .await;

    let (state, router, _, svc_key, pool) = setup_app_with_mock(&server, 4).await;
    let suffix = unique_suffix();
    let email_a = format!("m2_dual_a_{suffix}@unesp.br");
    let email_b = format!("m2_dual_b_{suffix}@unesp.br");
    let discord_a = format!("disc_a_{suffix}");
    let discord_b = format!("disc_b_{suffix}");

    let (id_a, tok_a, class_a) = login_student(&router, &email_a, pass, Some(&discord_a), None).await;
    let (id_b, _tok_b, _) = login_student(&router, &email_b, pass, Some(&discord_b), None).await;
    assert_eq!(class_a.as_deref(), Some("INI3A"));

    let bearer_a = format!("Bearer {tok_a}");

    // Missing / malformed / unknown Bearer -> 401
    let r_missing = send_req(&router, Method::GET, "/api/v1/account", vec![], None).await;
    assert_error_json(&r_missing, StatusCode::UNAUTHORIZED, "UNAUTHORIZED");

    let r_basic = send_req(
        &router,
        Method::GET,
        "/api/v1/account",
        vec![("Authorization", "Basic dXNlcjpwYXNz")],
        None,
    )
    .await;
    assert_error_json(&r_basic, StatusCode::UNAUTHORIZED, "UNAUTHORIZED");

    let r_unknown = send_req(
        &router,
        Method::GET,
        "/api/v1/account",
        vec![("Authorization", "Bearer deadbeef0000")],
        None,
    )
    .await;
    assert_error_json(&r_unknown, StatusCode::UNAUTHORIZED, "UNAUTHORIZED");

    // Expired Bearer -> 401
    sqlx::query("UPDATE user_sessions SET expires_at = $1 WHERE token_hash = $2")
        .bind(Utc::now() - ChronoDuration::minutes(5))
        .bind(sha256_hex(&tok_a))
        .execute(&pool)
        .await
        .expect("expire session");
    let r_expired = send_req(
        &router,
        Method::GET,
        "/api/v1/account",
        vec![("Authorization", &bearer_a)],
        None,
    )
    .await;
    assert_error_json(&r_expired, StatusCode::UNAUTHORIZED, "UNAUTHORIZED");

    // Re-login to get fresh active session for A
    let (_, tok_a, _) = login_student(&router, &email_a, pass, None, None).await;
    let bearer_a = format!("Bearer {tok_a}");

    // Bearer own account OK, no secrets or fcm_token string leaked
    let r_own = send_req(
        &router,
        Method::GET,
        "/api/v1/account",
        vec![("Authorization", &bearer_a)],
        None,
    )
    .await;
    assert_eq!(r_own.status, StatusCode::OK);
    let own_data = &r_own.json()["data"];
    assert_eq!(own_data["account_id"], Value::Number(id_a.into()));
    assert_eq!(own_data["has_fcm_token"], Value::Bool(false));
    assert!(own_data.get("fcm_token").is_none());
    assert!(own_data.get("nsac_pass_enc").is_none());

    // Bearer cross-account -> 403 FORBIDDEN
    let r_cross_id = send_req(
        &router,
        Method::GET,
        &format!("/api/v1/nsac/boletim?account_id={id_b}"),
        vec![("Authorization", &bearer_a)],
        None,
    )
    .await;
    assert_error_json(&r_cross_id, StatusCode::FORBIDDEN, "FORBIDDEN");

    let r_cross_disc = send_req(
        &router,
        Method::GET,
        &format!("/api/v1/account?discord_id={discord_b}"),
        vec![("Authorization", &bearer_a)],
        None,
    )
    .await;
    assert_error_json(&r_cross_disc, StatusCode::FORBIDDEN, "FORBIDDEN");

    // Service auth via X-Service-Key AND Authorization: Service
    let svc_auth_hdr = format!("Service {svc_key}");
    let r_svc_header = send_req(
        &router,
        Method::GET,
        &format!("/api/v1/account?account_id={id_a}"),
        vec![("X-Service-Key", &svc_key)],
        None,
    )
    .await;
    assert_eq!(r_svc_header.status, StatusCode::OK);

    let r_svc_bearer = send_req(
        &router,
        Method::GET,
        &format!("/api/v1/account?discord_id={discord_a}"),
        vec![("Authorization", &svc_auth_hdr)],
        None,
    )
    .await;
    assert_eq!(r_svc_bearer.status, StatusCode::OK);

    // Service calling /auth/logout -> 403
    let r_svc_logout = send_req(
        &router,
        Method::POST,
        "/api/v1/auth/logout",
        vec![("X-Service-Key", &svc_key)],
        None,
    )
    .await;
    assert_error_json(&r_svc_logout, StatusCode::FORBIDDEN, "FORBIDDEN");

    // Service missing target -> 400; mismatched target -> 400; unknown target -> 404
    let r_svc_no_target = send_req(
        &router,
        Method::GET,
        "/api/v1/account",
        vec![("X-Service-Key", &svc_key)],
        None,
    )
    .await;
    assert_error_json(&r_svc_no_target, StatusCode::BAD_REQUEST, "INVALID_PARAMS");

    let r_svc_mismatch = send_req(
        &router,
        Method::GET,
        &format!("/api/v1/account?account_id={id_a}&discord_id={discord_b}"),
        vec![("X-Service-Key", &svc_key)],
        None,
    )
    .await;
    assert_error_json(&r_svc_mismatch, StatusCode::BAD_REQUEST, "INVALID_PARAMS");

    let r_svc_unknown = send_req(
        &router,
        Method::GET,
        "/api/v1/account?account_id=999999999",
        vec![("X-Service-Key", &svc_key)],
        None,
    )
    .await;
    assert_error_json(&r_svc_unknown, StatusCode::NOT_FOUND, "USER_NOT_LINKED");

    // Service scopes & inactive service client
    let ro_key = format!("ro-key-{suffix}");
    let inactive_key = format!("inactive-key-{suffix}");
    state
        .auth()
        .seed_service_clients(&[
            ServiceKeySeed {
                name: format!("ro-client-{suffix}"),
                raw_key: Zeroizing::new(ro_key.clone()),
                scopes: vec!["nsac:read".to_string()],
            },
            ServiceKeySeed {
                name: format!("inactive-client-{suffix}"),
                raw_key: Zeroizing::new(inactive_key.clone()),
                scopes: vec!["nsac:read".to_string()],
            },
        ])
        .await
        .expect("seed extra clients");
    sqlx::query("UPDATE service_clients SET active = FALSE WHERE key_hash = $1")
        .bind(sha256_hex(&inactive_key))
        .execute(&pool)
        .await
        .expect("deactivate client");

    let r_inactive_svc = send_req(
        &router,
        Method::GET,
        &format!("/api/v1/account?account_id={id_a}"),
        vec![("X-Service-Key", &inactive_key)],
        None,
    )
    .await;
    assert_error_json(&r_inactive_svc, StatusCode::UNAUTHORIZED, "UNAUTHORIZED");

    let r_ro_link = send_json(
        &router,
        Method::PUT,
        "/api/v1/account/discord-link",
        vec![("X-Service-Key", &ro_key)],
        serde_json::json!({"account_id": id_b, "discord_id": format!("new_disc_{suffix}")}),
    )
    .await;
    assert_error_json(&r_ro_link, StatusCode::FORBIDDEN, "FORBIDDEN");

    // PUT /account/fcm-token validation & update
    let r_bad_fcm = send_json(
        &router,
        Method::PUT,
        "/api/v1/account/fcm-token",
        vec![("Authorization", &bearer_a)],
        serde_json::json!({"fcm_token": "   "}),
    )
    .await;
    assert_error_json(&r_bad_fcm, StatusCode::BAD_REQUEST, "INVALID_PARAMS");

    let fcm_secret = format!("fcm_device_secret_{suffix}");
    let r_ok_fcm = send_json(
        &router,
        Method::PUT,
        "/api/v1/account/fcm-token",
        vec![("Authorization", &bearer_a)],
        serde_json::json!({"fcm_token": fcm_secret}),
    )
    .await;
    assert_eq!(r_ok_fcm.status, StatusCode::OK);
    assert_eq!(r_ok_fcm.json()["data"]["has_fcm_token"], Value::Bool(true));
    assert_no_secrets_in_resp(&r_ok_fcm, &[&fcm_secret]);

    // PUT /account/discord-link duplicate -> 409 CONFLICT
    let r_dup_disc = send_json(
        &router,
        Method::PUT,
        "/api/v1/account/discord-link",
        vec![("X-Service-Key", &svc_key)],
        serde_json::json!({"account_id": id_b, "discord_id": discord_a}),
    )
    .await;
    assert_error_json(&r_dup_disc, StatusCode::CONFLICT, "CONFLICT");

    // Deactivated target account blocked for both Bearer & Service
    state
        .accounts()
        .set_active(id_a, false, Utc::now())
        .await
        .expect("deactivate account");
    let r_deact_bearer = send_req(
        &router,
        Method::GET,
        "/api/v1/account",
        vec![("Authorization", &bearer_a)],
        None,
    )
    .await;
    assert_error_json(&r_deact_bearer, StatusCode::UNAUTHORIZED, "UNAUTHORIZED");

    let r_deact_svc = send_req(
        &router,
        Method::GET,
        &format!("/api/v1/nsac/boletim?account_id={id_a}"),
        vec![("X-Service-Key", &svc_key)],
        None,
    )
    .await;
    assert_error_json(&r_deact_svc, StatusCode::FORBIDDEN, "FORBIDDEN");
}

#[tokio::test]
async fn five_nsac_routes_filters_post_filter_etag_304_and_moka_cache() {
    let server = MockServer::start().await;
    let pass = "RoutesPass_M2!";
    mount_standard_nsac_fixtures(
        &server,
        pass,
        include_str!("../../nsac-core/tests/fixtures/boletim_noturno_inf1.html"),
    )
    .await;

    let (state, router, _, _, _pool) = setup_app_with_mock(&server, 4).await;
    let suffix = unique_suffix();
    let email = format!("m2_routes_{suffix}@unesp.br");
    let (_, token, _) = login_student(&router, &email, pass, None, None).await;
    let bearer = format!("Bearer {token}");

    // 1. /home, /atrasos, /saidas, /ficha-avaliacao, /ficha-avaliacao/{1..4}
    let home_1 = send_req(
        &router,
        Method::GET,
        "/api/v1/nsac/home",
        vec![("Authorization", &bearer)],
        None,
    )
    .await;
    assert_eq!(home_1.status, StatusCode::OK);
    let home_etag = home_1.header("etag").expect("home etag").to_string();
    assert!(home_etag.starts_with('"') && home_etag.ends_with('"'));
    assert!(home_1.json()["next_poll_hint_secs"].as_u64().is_some());

    // Moka cache absorbs repeated /home read (wiremock saw 1 login GET /home + 1 aux GET /home = 2)
    let home_2 = send_req(
        &router,
        Method::GET,
        "/api/v1/nsac/home",
        vec![("Authorization", &bearer)],
        None,
    )
    .await;
    assert_eq!(home_2.status, StatusCode::OK);

    // /atrasos & /saidas
    let atrasos_1 = send_req(
        &router,
        Method::GET,
        "/api/v1/nsac/atrasos",
        vec![("Authorization", &bearer)],
        None,
    )
    .await;
    assert_eq!(atrasos_1.status, StatusCode::OK);
    assert!(atrasos_1.json()["data"].is_array());
    let atrasos_etag = atrasos_1.header("etag").expect("atrasos etag").to_string();

    let saidas_1 = send_req(
        &router,
        Method::GET,
        "/api/v1/nsac/saidas",
        vec![("Authorization", &bearer)],
        None,
    )
    .await;
    assert_eq!(saidas_1.status, StatusCode::OK);
    assert!(saidas_1.json()["data"].is_array());

    // /ficha-avaliacao + detail 2 (200) + detail 1 (302 -> null + ETag + 304) + invalid 0/5/abc (400)
    let ficha_list = send_req(
        &router,
        Method::GET,
        "/api/v1/nsac/ficha-avaliacao",
        vec![("Authorization", &bearer)],
        None,
    )
    .await;
    assert_eq!(ficha_list.status, StatusCode::OK);
    assert!(ficha_list.json()["data"].is_array());

    let ficha_2 = send_req(
        &router,
        Method::GET,
        "/api/v1/nsac/ficha-avaliacao/2",
        vec![("Authorization", &bearer)],
        None,
    )
    .await;
    assert_eq!(ficha_2.status, StatusCode::OK);
    assert_eq!(ficha_2.json()["data"]["bimester"], Value::Number(2.into()));

    let ficha_1 = send_req(
        &router,
        Method::GET,
        "/api/v1/nsac/ficha-avaliacao/1",
        vec![("Authorization", &bearer)],
        None,
    )
    .await;
    assert_eq!(ficha_1.status, StatusCode::OK);
    assert_eq!(ficha_1.json()["data"], Value::Null);
    let ficha_1_etag = ficha_1.header("etag").expect("null ficha ETag").to_string();
    let ficha_1_304 = send_req(
        &router,
        Method::GET,
        "/api/v1/nsac/ficha-avaliacao/1",
        vec![
            ("Authorization", &bearer),
            ("If-None-Match", &ficha_1_etag),
        ],
        None,
    )
    .await;
    assert_eq!(ficha_1_304.status, StatusCode::NOT_MODIFIED);
    assert!(ficha_1_304.raw_body.is_empty());
    assert_eq!(ficha_1_304.header("etag"), Some(ficha_1_etag.as_str()));

    for bad_id in ["0", "5", "abc"] {
        let r_bad = send_req(
            &router,
            Method::GET,
            &format!("/api/v1/nsac/ficha-avaliacao/{bad_id}"),
            vec![("Authorization", &bearer)],
            None,
        )
        .await;
        assert_error_json(&r_bad, StatusCode::BAD_REQUEST, "INVALID_PARAMS");
    }

    // 2. /boletim + QueryFilter (PF-01..PF-03) + invalid filter 400 + refresh stripping
    let bol_1 = send_req(
        &router,
        Method::GET,
        "/api/v1/nsac/boletim?refresh=false",
        vec![("Authorization", &bearer)],
        None,
    )
    .await;
    assert_eq!(bol_1.status, StatusCode::OK);
    let bol_etag = bol_1.header("etag").expect("boletim etag").to_string();
    assert!(bol_1.json()["next_poll_hint_secs"].as_u64().is_some());

    // Tolerant If-None-Match formats: exact, W/, unquoted, CSV, *
    let unquoted = bol_etag.trim_matches('"');
    let weak = format!("W/{bol_etag}");
    let csv = format!("\"stale-1\", {bol_etag}");
    for inm in [bol_etag.as_str(), weak.as_str(), unquoted, csv.as_str(), "*"] {
        let r_304 = send_req(
            &router,
            Method::GET,
            "/api/v1/nsac/boletim",
            vec![("Authorization", &bearer), ("If-None-Match", inm)],
            None,
        )
        .await;
        assert_eq!(r_304.status, StatusCode::NOT_MODIFIED, "If-None-Match={inm}");
        assert!(r_304.raw_body.is_empty());
        assert_eq!(r_304.header("etag"), Some(bol_etag.as_str()));
    }

    // Post-filter ETag across targetBimester=1 vs 2
    let bol_b1 = send_req(
        &router,
        Method::GET,
        "/api/v1/nsac/boletim?targetBimester=1",
        vec![("Authorization", &bearer)],
        None,
    )
    .await;
    assert_eq!(bol_b1.status, StatusCode::OK);
    let etag_b1 = bol_b1.header("etag").expect("b1 etag").to_string();

    let bol_b2 = send_req(
        &router,
        Method::GET,
        "/api/v1/nsac/boletim?targetBimester=2",
        vec![("Authorization", &bearer), ("If-None-Match", &etag_b1)],
        None,
    )
    .await;
    assert_eq!(bol_b2.status, StatusCode::OK);
    let etag_b2 = bol_b2.header("etag").expect("b2 etag").to_string();
    assert_ne!(etag_b1, etag_b2);

    // PF-01 classAverage[gt], PF-03 accent-insensitive subjectName[contains], PF-02 immutability
    let bol_filt = send_req(
        &router,
        Method::GET,
        "/api/v1/nsac/boletim?classAverage[gt]=7.0&subjectName[contains]=programacao",
        vec![("Authorization", &bearer)],
        None,
    )
    .await;
    assert_eq!(bol_filt.status, StatusCode::OK);
    let years_filt = bol_filt.json()["data"]["data"].as_array().expect("years").clone();
    assert_eq!(years_filt.len(), 1);
    assert_eq!(years_filt[0]["grades"].as_array().expect("grades").len(), 1);

    // Confirm PF-02: cached unfiltered boletim still intact
    let bol_unfilt_after = send_req(
        &router,
        Method::GET,
        "/api/v1/nsac/boletim",
        vec![("Authorization", &bearer)],
        None,
    )
    .await;
    let years_full = bol_unfilt_after.json()["data"]["data"]
        .as_array()
        .expect("years")
        .clone();
    assert!(years_full[0]["grades"].as_array().expect("grades").len() >= 2);

    // Invalid filter param / bracket -> 400 INVALID_PARAMS
    for bad_q in [
        "unknownField=1",
        "grade[gt=5",
        "refresh=maybe",
        "grade[gte]=notanumber",
    ] {
        let r_bad = send_req(
            &router,
            Method::GET,
            &format!("/api/v1/nsac/boletim?{bad_q}"),
            vec![("Authorization", &bearer)],
            None,
        )
        .await;
        assert_error_json(&r_bad, StatusCode::BAD_REQUEST, "INVALID_PARAMS");
    }

    // 3. Verify Moka cache absorption (1 login + 1 aux /home = 2 home requests, only 1 boletim request)
    let reqs = server.received_requests().await.expect("received_requests");
    let boletim_hits = reqs
        .iter()
        .filter(|r| r.url.path() == "/nsac/aluno/boletim")
        .count();
    let home_hits = reqs
        .iter()
        .filter(|r| r.url.path() == "/nsac/home")
        .count();
    assert_eq!(boletim_hits, 1, "moka must absorb repeated /boletim reads");
    assert_eq!(home_hits, 2, "moka must absorb repeated /home reads (1 login + 1 aux)");

    // 4. Simulate server restart (clear Moka) + NSAC 503 -> DB fallback ETag parity!
    state.caches.boletim.invalidate_all();
    state.caches.snapshots.invalidate_all();
    state.caches.boletim.run_pending_tasks().await;
    state.caches.snapshots.run_pending_tasks().await;

    server.reset().await;
    Mock::given(method("GET"))
        .and(path("/nsac/aluno/boletim"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/nsac/aluno/atrasos"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&server)
        .await;

    let fallback_bol_304 = send_req(
        &router,
        Method::GET,
        "/api/v1/nsac/boletim",
        vec![("Authorization", &bearer), ("If-None-Match", &bol_etag)],
        None,
    )
    .await;
    assert_eq!(
        fallback_bol_304.status,
        StatusCode::NOT_MODIFIED,
        "Pre-restart and post-restart DB-fallback boletim ETag must match"
    );

    let fallback_atrasos_304 = send_req(
        &router,
        Method::GET,
        "/api/v1/nsac/atrasos",
        vec![("Authorization", &bearer), ("If-None-Match", &atrasos_etag)],
        None,
    )
    .await;
    assert_eq!(fallback_atrasos_304.status, StatusCode::NOT_MODIFIED);

    // Verify wiremock saw only initial login fetch for boletim (cached before reset)
    let _ = home_etag;
}

struct CookieRotatingResponder {
    step: Arc<AtomicUsize>,
    server_uri: String,
    body_html: String,
}

impl Respond for CookieRotatingResponder {
    fn respond(&self, req: &WiremockRequest) -> ResponseTemplate {
        let count = self.step.fetch_add(1, Ordering::SeqCst);
        let cookie_hdr = req
            .headers
            .get("cookie")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert!(
            cookie_hdr.contains("remember_web_guard_01=rem_tok_initial"),
            "remember_web_* must be preserved across rotations, got: {cookie_hdr}"
        );

        if count == 0 {
            // First request: 200 with rotated XSRF/session (remember_web omitted!)
            ResponseTemplate::new(200)
                .append_header("Set-Cookie", "XSRF-TOKEN=rotated_xsrf_2; Path=/")
                .append_header(
                    "Set-Cookie",
                    "nsaconline_session=rotated_sess_2; Path=/; HttpOnly",
                )
                .set_body_string(&self.body_html)
        } else if count == 1 {
            // Second request: expired session -> 302 to /nsac/login
            ResponseTemplate::new(302)
                .insert_header("Location", format!("{}/nsac/login", self.server_uri))
        } else {
            // Retry after transparent re-login: 200
            ResponseTemplate::new(200).set_body_string(&self.body_html)
        }
    }
}

#[tokio::test]
async fn cookie_rotation_and_auto_refresh_with_semaphore_1_no_deadlock() {
    let server = MockServer::start().await;
    let pass = "SemaphorePass_M2!";

    // Mount login & boletim for initial login
    Mock::given(method("GET"))
        .and(path("/nsac"))
        .respond_with(
            ResponseTemplate::new(200)
                .append_header("Set-Cookie", "XSRF-TOKEN=init_xsrf; Path=/")
                .append_header("Set-Cookie", "nsaconline_session=init_sess; Path=/")
                .set_body_string(include_str!("../../nsac-core/tests/fixtures/login_page.html")),
        )
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/nsac/login"))
        .respond_with(
            ResponseTemplate::new(302)
                .insert_header("Location", format!("{}/nsac/home", server.uri()))
                .append_header("Set-Cookie", "XSRF-TOKEN=auth_xsrf_1; Path=/")
                .append_header("Set-Cookie", "nsaconline_session=auth_sess_1; Path=/")
                .append_header(
                    "Set-Cookie",
                    "remember_web_guard_01=rem_tok_initial; Path=/; HttpOnly",
                ),
        )
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/nsac/home"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(include_str!("../../nsac-core/tests/fixtures/home_authenticated.html")),
        )
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/nsac/aluno/boletim"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(include_str!("../../nsac-core/tests/fixtures/boletim_noturno_inf1.html")),
        )
        .mount(&server)
        .await;

    let step = Arc::new(AtomicUsize::new(0));
    Mock::given(method("GET"))
        .and(path("/nsac/aluno/atrasos"))
        .respond_with(CookieRotatingResponder {
            step: Arc::clone(&step),
            server_uri: server.uri(),
            body_html: include_str!("../../nsac-core/tests/fixtures/atrasos_populated.html")
                .to_string(),
        })
        .mount(&server)
        .await;

    // Max concurrent NSAC requests = 1 to prove zero semaphore deadlock during auto-refresh!
    let (state, router, _, _, _pool) = setup_app_with_mock(&server, 1).await;
    let email = format!("m2_sem_{}@unesp.br", unique_suffix());
    let (account_id, token, _) = login_student(&router, &email, pass, None, None).await;
    let bearer = format!("Bearer {token}");

    // Call 1: rotates cookies on 200 and preserves remember_web_guard_01
    let r1 = send_req(
        &router,
        Method::GET,
        "/api/v1/nsac/atrasos",
        vec![("Authorization", &bearer)],
        None,
    )
    .await;
    assert_eq!(r1.status, StatusCode::OK);
    let creds_after_rot = state
        .accounts()
        .get_decrypted_credentials(&state.crypto, account_id)
        .await
        .expect("creds")
        .expect("some creds");
    let rotated_cookies = creds_after_rot.cookies.expect("cookies");
    assert_eq!(rotated_cookies.xsrf_token(), "rotated_xsrf_2");
    assert_eq!(rotated_cookies.session(), "rotated_sess_2");
    assert_eq!(
        rotated_cookies.remember_web(),
        Some(("remember_web_guard_01", "rem_tok_initial"))
    );

    // Call 2 (?refresh=true): hits 302 -> /nsac/login -> auto-refresh login + retry with Semaphore(1)!
    let r2 = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        send_req(
            &router,
            Method::GET,
            "/api/v1/nsac/atrasos?refresh=true",
            vec![("Authorization", &bearer)],
            None,
        ),
    )
    .await
    .expect("must not deadlock on NSAC_MAX_CONCURRENT_REQUESTS=1");
    assert_eq!(r2.status, StatusCode::OK);
    assert_eq!(state.semaphore.active_count(), 0);
    assert_eq!(state.semaphore.peak_count(), 1);
}

#[tokio::test]
async fn boletim_repo_drain_preserves_subjects_without_final_results() {
    let pool = m2_test_pool().await;
    let state = AppState::with_default_ports(
        make_test_config("http://127.0.0.1:1/nsac".to_string(), vec![], 2),
        pool.clone(),
    )
    .expect("state");

    let email = format!("m2_drain_{}@unesp.br", unique_suffix());
    let crypto = &state.crypto;
    let cookies = nsac_core::models::NsacSessionCookies::from_cookie_header(
        "XSRF-TOKEN=x; nsaconline_session=s",
    )
    .expect("cookies");

    let acct = state
        .accounts()
        .upsert_on_login(
            crypto,
            &email,
            "pass",
            &cookies,
            Some("INF1"),
            None,
            None,
            Utc::now(),
        )
        .await
        .expect("upsert");

    let boletim = AllYearsResponse {
        warning: WarningValue::Bool(false),
        user_current_year: 1,
        data: vec![YearInfo {
            title: "INF1".to_string(),
            year: 2026,
            status: "Cursando".to_string(),
            grades: vec![
                FullGrades {
                    subject_name: "Artes".to_string(),
                    results: ResultData {
                        grade: None,
                        total_absences: 0,
                    },
                    bimesters: vec![UnifiedBimesterData {
                        bimester: 1,
                        personal: PersonalBiInformation {
                            grade: Some(8.0),
                            absences: 0,
                            recovery: false,
                            recovered: None,
                            recovery_code: Some(RecoveryStatusCode::Nac),
                            recovery_message: Some(RecoveryMessage::NaoAconteceu),
                        },
                        class: ClassBiInformation {
                            average_grade: Some(7.5),
                        },
                    }],
                },
                FullGrades {
                    subject_name: "Biologia".to_string(),
                    results: ResultData {
                        grade: Some(9.0),
                        total_absences: 2,
                    },
                    bimesters: vec![UnifiedBimesterData {
                        bimester: 1,
                        personal: PersonalBiInformation {
                            grade: Some(9.0),
                            absences: 2,
                            recovery: false,
                            recovered: None,
                            recovery_code: Some(RecoveryStatusCode::Nac),
                            recovery_message: Some(RecoveryMessage::NaoAconteceu),
                        },
                        class: ClassBiInformation {
                            average_grade: Some(8.0),
                        },
                    }],
                },
            ],
            bimesters_metrics: vec![],
        }],
    };

    state
        .boletim()
        .upsert_boletim(acct.id, &boletim)
        .await
        .expect("upsert_boletim");

    // Delete subject_final_results ONLY for Artes (while keeping Biologia)
    sqlx::query(
        r#"
        DELETE FROM subject_final_results sfr
        USING subjects s
        WHERE sfr.subject_id = s.id
          AND sfr.account_id = $1
          AND s.name = 'Artes'
        "#,
    )
    .bind(acct.id)
    .execute(&pool)
    .await
    .expect("delete partial final result");

    let loaded = state
        .boletim()
        .get_boletim(acct.id)
        .await
        .expect("get_boletim")
        .expect("some");
    let names: Vec<&str> = loaded.data[0]
        .grades
        .iter()
        .map(|g| g.subject_name.as_str())
        .collect();
    assert_eq!(names, vec!["Artes", "Biologia"]);
}

#[tokio::test]
async fn expired_nsac_session_with_changed_password_returns_401_without_leaking_secrets() {
    let server = MockServer::start().await;
    let old_pass = "OldPassword_M2_Secret!";
    let new_pass = "ChangedPassword_M2_Secret!";

    Mock::given(method("GET"))
        .and(path("/nsac"))
        .respond_with(
            ResponseTemplate::new(200)
                .append_header("Set-Cookie", "XSRF-TOKEN=init_xsrf; Path=/")
                .append_header("Set-Cookie", "nsaconline_session=init_sess; Path=/")
                .set_body_string(include_str!("../../nsac-core/tests/fixtures/login_page.html")),
        )
        .mount(&server)
        .await;

    let old_pass_needle = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("password", old_pass)
        .finish();
    Mock::given(method("POST"))
        .and(path("/nsac/login"))
        .and(body_string_contains(old_pass_needle.as_str()))
        .respond_with(
            ResponseTemplate::new(302)
                .insert_header("Location", format!("{}/nsac/home", server.uri()))
                .append_header("Set-Cookie", "XSRF-TOKEN=auth_xsrf_old; Path=/")
                .append_header("Set-Cookie", "nsaconline_session=auth_sess_old; Path=/"),
        )
        .up_to_n_times(1)
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/nsac/login"))
        .respond_with(
            ResponseTemplate::new(302)
                .insert_header("Location", format!("{}/nsac/login", server.uri())),
        )
        .with_priority(10)
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/nsac/home"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(include_str!("../../nsac-core/tests/fixtures/home_authenticated.html")),
        )
        .up_to_n_times(1)
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/nsac/aluno/boletim"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(include_str!("../../nsac-core/tests/fixtures/boletim_noturno_inf1.html")),
        )
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/nsac/aluno/saidas"))
        .respond_with(
            ResponseTemplate::new(302)
                .insert_header("Location", format!("{}/nsac/login", server.uri())),
        )
        .mount(&server)
        .await;

    let (_state, router, _, _, _pool) = setup_app_with_mock(&server, 2).await;
    let email = format!("m2_chgpw_{}@unesp.br", unique_suffix());
    let (_aid, token, _) = login_student(&router, &email, old_pass, None, None).await;
    let bearer = format!("Bearer {token}");

    let r_fail = send_req(
        &router,
        Method::GET,
        "/api/v1/nsac/saidas",
        vec![("Authorization", &bearer)],
        None,
    )
    .await;
    assert_error_json(&r_fail, StatusCode::UNAUTHORIZED, "INVALID_NSAC_CREDENTIALS");
    assert_no_secrets_in_resp(&r_fail, &[old_pass, new_pass, &token, "auth_sess_old"]);
}
