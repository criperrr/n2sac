use std::collections::HashMap;
use std::net::SocketAddr;

use axum::response::IntoResponse;
use http_body_util::BodyExt;
use nsac_core::error::{AppError, ErrorCode};
use nsac_server::config::Config;
use nsac_server::error::{ConfigError, ServerError};

fn base_env_map() -> HashMap<&'static str, &'static str> {
    HashMap::from([
        (
            "DATABASE_URL",
            "postgres://postgres:supersecretpw@127.0.0.1:5432/nsac_m1_test",
        ),
        (
            "ENCRYPTION_KEY",
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        ),
    ])
}

#[test]
fn config_supports_dotenv_example_variables_and_redacts_debug() {
    let mut env = base_env_map();
    env.insert("HTTP_BIND", "0.0.0.0:3000");
    env.insert("RUST_LOG", "nsac_server=debug");
    env.insert(
        "SERVICE_API_KEYS",
        "bot-dev:my-top-secret-bot-key, raw-only-secret-key",
    );
    env.insert("USER_SESSION_TTL_SECS", "7200");
    env.insert("NSAC_BASE_URL", "http://200.145.153.1/nsac/");
    env.insert("NSAC_HTTP_TIMEOUT_SECS", "15");
    env.insert("NSAC_MAX_CONCURRENT_REQUESTS", "8");
    env.insert("SENTINEL_ENABLED", "true");
    env.insert("SENTINEL_BASELINE_INTERVAL_SECS", "43200");
    env.insert("SENTINEL_HISTORICAL_RADIUS_DAYS", "5");
    env.insert("SENTINEL_HISTORICAL_MIN_INTERVAL_SECS", "3600");
    env.insert("SENTINEL_HOT_MIN_INTERVAL_SECS", "90");
    env.insert("SENTINEL_HOT_MAX_INTERVAL_SECS", "240");
    env.insert("SENTINEL_HOT_DURATION_SECS", "5400");
    env.insert("SENTINEL_API_VOLUME_WEIGHT", "0.12");
    env.insert("SENTINEL_API_COOLDOWN_SECS", "900");
    env.insert("SENTINEL_MIDNIGHT_BACKOFF_SECS", "1200");
    env.insert("SENTINEL_FANOUT_JITTER_MAX_MS", "800");
    env.insert("FCM_ENABLED", "false");
    env.insert("FCM_PROJECT_ID", "proj-1");
    env.insert("FCM_CREDENTIALS_JSON", "");
    env.insert("FCM_ENDPOINT_OVERRIDE", "");

    let cfg = Config::from_lookup(|k| env.get(k).map(|v| (*v).to_string())).unwrap();

    assert_eq!(cfg.http_bind, "0.0.0.0:3000".parse::<SocketAddr>().unwrap());
    assert_eq!(cfg.nsac_base_url, "http://200.145.153.1/nsac");
    assert_eq!(cfg.nsac_timeout_secs, 15);
    assert_eq!(cfg.nsac_max_concurrent_requests, 8);
    assert_eq!(cfg.user_session_ttl_secs, 7200);
    assert_eq!(cfg.service_api_keys.len(), 2);
    assert_eq!(cfg.service_api_keys[0].name, "bot-dev");
    assert_eq!(
        cfg.service_api_keys[0].raw_key.as_str(),
        "my-top-secret-bot-key"
    );
    assert_eq!(cfg.service_api_keys[1].name, "service-2");
    assert_eq!(cfg.sentinel.cold_baseline_secs, 43200);
    assert_eq!(cfg.sentinel.historical_radius_days, 5);
    assert_eq!(cfg.sentinel.historical_interval_secs, 3600);
    assert_eq!(cfg.sentinel.hot_min_interval_secs, 90);
    assert_eq!(cfg.sentinel.hot_max_interval_secs, 240);
    assert_eq!(cfg.sentinel.hot_duration_secs, 5400);
    assert!((cfg.sentinel.api_volume_weight - 0.12).abs() < f64::EPSILON);
    assert_eq!(cfg.sentinel.api_cooldown_secs, 900);
    assert_eq!(cfg.sentinel.midnight_backoff_secs, 1200);
    assert_eq!(cfg.sentinel.jitter_max_ms, 800);
    assert!(!cfg.fcm.enabled);
    assert!(cfg.fcm.credentials_json.is_none());
    assert!(cfg.fcm.endpoint_url.is_none());
    assert_eq!(cfg.cache.boletim_ttl_secs, 300);
    assert_eq!(cfg.cache.snapshot_ttl_secs, 300);
    assert_eq!(cfg.cache.auth_ttl_secs, 0);

    let debug_str = format!("{cfg:?}");
    assert!(!debug_str.contains("supersecretpw"));
    assert!(!debug_str.contains("0123456789abcdef"));
    assert!(!debug_str.contains("my-top-secret-bot-key"));
    assert!(!debug_str.contains("raw-only-secret-key"));
    assert!(debug_str.contains("[REDACTED]"));
    assert!(debug_str.contains("bot-dev"));
}

#[test]
fn config_supports_e2e_harness_and_project_md_aliases() {
    let mut env = base_env_map();
    env.insert("USER_TOKEN_TTL_SECS", "1234");
    env.insert("NSAC_TIMEOUT_SECS", "3");
    env.insert("SENTINEL_TICK_MS", "250");
    env.insert("SENTINEL_COLD_BASELINE_SECS", "30");
    env.insert("SENTINEL_HISTORICAL_INTERVAL_SECS", "2");
    env.insert("SENTINEL_HOT_INTERVAL_SECS", "1");
    env.insert("SENTINEL_API_VOLUME_COOLDOWN_SECS", "2");
    env.insert("SENTINEL_JITTER_MAX_MS", "50");
    env.insert("FCM_PROJECT_ID", "nsac-e2e-test");
    env.insert("FCM_BASE_URL", "http://127.0.0.1:19000/");
    env.insert(
        "FCM_ENDPOINT_URL",
        "http://127.0.0.1:19000/v1/projects/nsac-e2e-test/messages:send",
    );
    env.insert("FCM_OAUTH_TOKEN_URL", "http://127.0.0.1:19000/oauth2/token");
    env.insert("FCM_CREDENTIALS_JSON", "{\"private_key\":\"secret-pem\"}");
    env.insert("CACHE_BOLETIM_TTL_SECS", "120");
    env.insert("CACHE_SNAPSHOT_TTL_SECS", "60");
    env.insert("CACHE_AUTH_TTL_SECS", "30");

    let cfg = Config::from_lookup(|k| env.get(k).map(|v| (*v).to_string())).unwrap();

    assert_eq!(cfg.user_session_ttl_secs, 1234);
    assert_eq!(cfg.nsac_timeout_secs, 3);
    assert_eq!(cfg.sentinel.tick_ms, 250);
    assert_eq!(cfg.sentinel.cold_baseline_secs, 30);
    assert_eq!(cfg.sentinel.historical_interval_secs, 2);
    assert_eq!(cfg.sentinel.hot_min_interval_secs, 1);
    assert_eq!(cfg.sentinel.hot_max_interval_secs, 1);
    assert_eq!(cfg.sentinel.api_cooldown_secs, 2);
    assert_eq!(cfg.sentinel.jitter_max_ms, 50);
    assert!(cfg.fcm.enabled);
    assert_eq!(
        cfg.fcm.endpoint_url.as_deref(),
        Some("http://127.0.0.1:19000/v1/projects/nsac-e2e-test/messages:send")
    );
    assert_eq!(
        cfg.fcm.oauth_token_url.as_deref(),
        Some("http://127.0.0.1:19000/oauth2/token")
    );
    assert_eq!(cfg.cache.boletim_ttl_secs, 120);
    assert_eq!(cfg.cache.snapshot_ttl_secs, 60);
    assert_eq!(cfg.cache.auth_ttl_secs, 30);

    let debug_str = format!("{cfg:?}");
    assert!(!debug_str.contains("secret-pem"));
}

#[test]
fn config_rejects_invalid_keys_and_never_leaks_secrets_in_errors() {
    let mut env = base_env_map();
    env.insert("ENCRYPTION_KEY", "not-valid-hex-secret-material");
    let err = Config::from_lookup(|k| env.get(k).map(|v| (*v).to_string())).unwrap_err();
    assert_eq!(err, ConfigError::InvalidEncryptionKey);
    assert!(!err.to_string().contains("not-valid-hex-secret-material"));

    let mut env2 = base_env_map();
    env2.insert("SERVICE_API_KEYS", "bot:");
    let err2 = Config::from_lookup(|k| env2.get(k).map(|v| (*v).to_string())).unwrap_err();
    assert!(matches!(
        err2,
        ConfigError::InvalidVar {
            name: "SERVICE_API_KEYS",
            ..
        }
    ));
}

#[tokio::test]
async fn server_error_sanitizes_internal_and_sql_details_in_http_response() {
    let app_err = AppError::nsac_unavailable("raw cookie XSRF-TOKEN=leaked_secret failed");
    let srv_from_app: ServerError = app_err.into();
    assert_eq!(srv_from_app.http_status(), 503);
    assert_eq!(srv_from_app.code_str(), "NSAC_UNAVAILABLE");
    assert!(!srv_from_app.user_message().contains("leaked_secret"));
    assert!(!srv_from_app.to_string().contains("leaked_secret"));

    let sql_err: ServerError = sqlx::Error::RowNotFound.into();
    assert_eq!(sql_err.http_status(), 404);
    assert_eq!(sql_err.code_str(), "NOT_FOUND");

    let timeout_err: ServerError = sqlx::Error::PoolTimedOut.into();
    assert_eq!(timeout_err.http_status(), 500);
    assert_eq!(timeout_err.code_str(), "INTERNAL_ERROR");

    let conflict_err = ServerError::conflict("E-mail já vinculado.", "unique violation");
    assert_eq!(conflict_err.http_status(), 409);
    assert_eq!(conflict_err.code_str(), "CONFLICT");

    let resp = srv_from_app.into_response();
    assert_eq!(resp.status().as_u16(), 503);
    let body_bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
    assert_eq!(json["success"], false);
    assert_eq!(json["errors"][0]["code"], "NSAC_UNAVAILABLE");
    assert!(
        !json["errors"][0]["message"]
            .as_str()
            .unwrap()
            .contains("leaked_secret")
    );

    let cfg_err: ServerError = ConfigError::MissingVar {
        name: "ENCRYPTION_KEY",
    }
    .into();
    assert_eq!(cfg_err.error_code(), ErrorCode::InternalError);
    assert_eq!(cfg_err.http_status(), 500);
}
