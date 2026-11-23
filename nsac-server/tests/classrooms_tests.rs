//! Integration tests for Class Rooms, Representative Elections (public non-secret approval voting),
//! Zipped Channel Media, Official Announcements, Dual Group Chats (`chat` & `off_topic`),
//! and Notification Preferences.

use std::io::Cursor;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use base64::Engine as _;
use chrono::Utc;
use http_body_util::BodyExt;
use image::{DynamicImage, ImageFormat, Rgba, RgbaImage};
use nsac_core::models::NsacSessionCookies;
use nsac_server::config::{CacheConfig, Config, FcmConfig, SentinelConfig};
use nsac_server::http::build_router;
use nsac_server::run_migrations;
use nsac_server::state::AppState;
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt;
use zeroize::Zeroizing;

fn test_database_url() -> String {
    std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://postgres:postgres@127.0.0.1:5432/nsac_server".to_string())
}

fn build_test_config(db_url: String) -> Config {
    Config {
        http_bind: "127.0.0.1:0".parse().expect("bind addr"),
        rust_log: "warn".to_string(),
        database_url: Zeroizing::new(db_url),
        encryption_key: Zeroizing::new(
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".to_string(),
        ),
        service_api_keys: Vec::new(),
        user_session_ttl_secs: 3600,
        nsac_base_url: "http://127.0.0.1:1".to_string(),
        nsac_timeout_secs: 5,
        nsac_max_concurrent_requests: 2,
        sentinel: SentinelConfig {
            enabled: false,
            tick_ms: 1000,
            cold_baseline_secs: 86_400,
            historical_radius_days: 4,
            historical_interval_secs: 7200,
            hot_min_interval_secs: 120,
            hot_max_interval_secs: 180,
            hot_duration_secs: 7200,
            api_volume_weight: 0.15,
            api_cooldown_secs: 1800,
            midnight_backoff_secs: 1800,
            jitter_max_ms: 0,
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
            boletim_ttl_secs: 60,
            snapshot_ttl_secs: 60,
            auth_ttl_secs: 0,
        },
    }
}

async fn setup_test_app() -> Option<(AppState, axum::Router)> {
    let db_url = test_database_url();
    let pool = PgPool::connect(&db_url).await.ok()?;
    run_migrations(&pool).await.expect("run migrations");
    let state = AppState::with_default_ports(build_test_config(db_url), pool)
        .expect("build test AppState");
    let router = build_router(state.clone());
    Some((state, router))
}

async fn create_student(state: &AppState, email: &str, class_code: &str) -> (i64, String) {
    let now = Utc::now();
    let cookies = NsacSessionCookies::new("xsrf", "sess", None).expect("cookies");
    let acc = state
        .accounts()
        .upsert_on_login(
            &state.crypto,
            email,
            "secret-pass",
            &cookies,
            Some(class_code),
            None,
            Some("fake-fcm-tok"),
            now,
        )
        .await
        .expect("upsert student");
    let sess = state
        .auth()
        .create_user_session(acc.id, 3600, now)
        .await
        .expect("create session");
    (acc.id, sess.token.as_str().to_string())
}

fn tiny_png_data_url() -> String {
    let img = DynamicImage::ImageRgba8(RgbaImage::from_pixel(64, 64, Rgba([40, 120, 220, 255])));
    let mut buf = Vec::new();
    img.write_to(&mut Cursor::new(&mut buf), ImageFormat::Png)
        .expect("encode png");
    format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(buf)
    )
}

async fn json_req(
    router: &axum::Router,
    method: &str,
    uri: &str,
    token: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("Authorization", format!("Bearer {token}"));
    let req = if let Some(b) = body {
        builder = builder.header("Content-Type", "application/json");
        builder.body(Body::from(b.to_string())).expect("req")
    } else {
        builder.body(Body::empty()).expect("req")
    };
    let resp = router.clone().oneshot(req).await.expect("oneshot");
    let status = resp.status();
    let bytes = resp.into_body().collect().await.expect("body").to_bytes();
    let val = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, val)
}

#[tokio::test]
async fn room_lifecycle_min_members_public_election_zipped_media_and_dual_chats() {
    let Some((state, router)) = setup_test_app().await else {
        return;
    };

    let suffix = Utc::now().timestamp_nanos_opt().unwrap_or(1);
    let class_code = format!("T{}", suffix % 100_000);

    // 1. With only 5 students (<= 5), room is NOT functional yet.
    let mut students = Vec::new();
    for i in 1..=5 {
        let email = format!("roomtest_{suffix}_{i}@unesp.br");
        students.push(create_student(&state, &email, &class_code).await);
    }
    let (status, body) = json_req(&router, "GET", "/api/v1/classrooms/me", &students[0].1, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["room"]["member_count"], 5);
    assert_eq!(body["data"]["room"]["is_functional"], false);

    // Voting must be rejected while member_count <= 5.
    let (vote_status, _) = json_req(
        &router,
        "POST",
        &format!("/api/v1/classrooms/me/representatives/votes/{}", students[1].0),
        &students[0].1,
        None,
    )
    .await;
    assert_eq!(vote_status, StatusCode::FORBIDDEN);

    // 2. Add 6th student (> 5): room becomes functional with adaptive quorum = ceil(0.6 * 6) = 4.
    let s6 = create_student(&state, &format!("roomtest_{suffix}_6@unesp.br"), &class_code).await;
    students.push(s6);

    let (status, body) = json_req(&router, "GET", "/api/v1/classrooms/me", &students[0].1, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["room"]["is_functional"], true);
    assert_eq!(body["data"]["room"]["quorum"], 4);

    // 3. Public (non-secret) approval voting: students 1..4 vote for student 0 -> student 0 elected!
    let rep_id = students[0].0;
    let rep_tok = students[0].1.clone();
    let regular_tok = students[5].1.clone();

    for s in &students[1..=4] {
        let (st, _) = json_req(
            &router,
            "POST",
            &format!("/api/v1/classrooms/me/representatives/votes/{rep_id}"),
            &s.1,
            None,
        )
        .await;
        assert_eq!(st, StatusCode::OK);
    }

    // Verify public ranking & non-secret voters list.
    let (st, election) = json_req(&router, "GET", "/api/v1/classrooms/me/election", &regular_tok, None).await;
    assert_eq!(st, StatusCode::OK);
    let top = &election["data"]["candidates"][0];
    assert_eq!(top["account_id"], rep_id);
    assert_eq!(top["is_representative"], true);
    assert_eq!(top["vote_count"], 4);
    assert_eq!(top["voters"].as_array().map(|v| v.len()), Some(4));

    // 4. Non-representative CANNOT post announcements or edit room appearance.
    let (st, _) = json_req(
        &router,
        "POST",
        "/api/v1/classrooms/me/announcements",
        &regular_tok,
        Some(json!({ "title": "Teste", "body": "Oi" })),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);

    let (st, _) = json_req(
        &router,
        "PUT",
        "/api/v1/classrooms/me/appearance",
        &regular_tok,
        Some(json!({ "avatar_data_url": tiny_png_data_url() })),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);

    // 5. Representative CAN customize room avatar & banner (stored zipped) and post announcement with zipped image.
    let (st, app_res) = json_req(
        &router,
        "PUT",
        "/api/v1/classrooms/me/appearance",
        &rep_tok,
        Some(json!({
            "avatar_data_url": tiny_png_data_url(),
            "banner_data_url": tiny_png_data_url()
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert!(app_res["data"]["avatar_url"].is_string());
    assert!(app_res["data"]["banner_url"].is_string());

    let (st, ann_res) = json_req(
        &router,
        "POST",
        "/api/v1/classrooms/me/announcements",
        &rep_tok,
        Some(json!({
            "category": "exam",
            "title": "Prova de Matemática",
            "body": "Trazer calculadora científica.",
            "notify_push": false,
            "image_data_url": tiny_png_data_url()
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let ann_id = ann_res["data"]["id"].as_i64().expect("announcement id");

    // Fetch unzipped JPEG attachment via authenticated endpoint.
    let att_req = Request::builder()
        .method("GET")
        .uri(format!("/api/v1/classrooms/me/announcements/{ann_id}/attachment"))
        .header("Authorization", format!("Bearer {regular_tok}"))
        .body(Body::empty())
        .expect("req");
    let att_resp = router.clone().oneshot(att_req).await.expect("oneshot");
    assert_eq!(att_resp.status(), StatusCode::OK);
    let jpeg_bytes = att_resp.into_body().collect().await.expect("bytes").to_bytes();
    assert!(jpeg_bytes.starts_with(&[0xFF, 0xD8])); // Valid JPEG SOI

    // 6. Dual chats (`chat` and `off_topic`): regular student can send text, CANNOT send image; rep CAN send image.
    let (st, _) = json_req(
        &router,
        "POST",
        "/api/v1/classrooms/me/channels/chat/messages",
        &regular_tok,
        Some(json!({ "text": "Alguém tem a lista?" })),
    )
    .await;
    assert_eq!(st, StatusCode::OK);

    let (st, _) = json_req(
        &router,
        "POST",
        "/api/v1/classrooms/me/channels/off_topic/messages",
        &regular_tok,
        Some(json!({ "text": "Bora jogar?", "image_data_url": tiny_png_data_url() })),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);

    let (st, rep_msg) = json_req(
        &router,
        "POST",
        "/api/v1/classrooms/me/channels/off_topic/messages",
        &rep_tok,
        Some(json!({ "text": "Foto da sala!", "image_data_url": tiny_png_data_url() })),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert!(rep_msg["data"]["message"]["attachment_url"].is_string());

    // 7. Notification preferences & DELETE /api/v1/account/fcm-token
    let (st, prefs) = json_req(
        &router,
        "PUT",
        "/api/v1/account/notification-preferences",
        &regular_tok,
        Some(json!({ "grades_enabled": true, "announcements_enabled": true, "room_chat_mode": "all" })),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(prefs["data"]["room_chat_mode"], "all");

    let (st, del_fcm) = json_req(&router, "DELETE", "/api/v1/account/fcm-token", &regular_tok, None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(del_fcm["data"]["has_fcm_token"], false);

    // 8. Instant revocation: withdrawing 1 vote drops rep_id from 4 -> 3 (< quorum 4).
    let (st, after_revoke) = json_req(
        &router,
        "DELETE",
        &format!("/api/v1/classrooms/me/representatives/votes/{rep_id}"),
        &students[1].1,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(after_revoke["data"]["candidates"][0]["is_representative"], false);
}
