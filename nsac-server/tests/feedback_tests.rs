mod common;

use std::io::Cursor;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use base64::Engine as _;
use chrono::Utc;
use http_body_util::BodyExt;
use image::{DynamicImage, ImageFormat, Rgba, RgbaImage};
use nsac_core::crypto::CryptoService;
use nsac_core::models::NsacSessionCookies;
use nsac_server::auth::seed_service_clients;
use nsac_server::config::Config;
use nsac_server::db::{AccountRepo, AuthRepo};
use nsac_server::http::build_router;
use nsac_server::state::AppState;
use serde_json::{Value, json};
use tower::ServiceExt;

fn test_config() -> Config {
    let db_url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://postgres:postgres@127.0.0.1:5432/nsac_test".to_string());
    let key = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    Config::from_lookup(|name| match name {
        "DATABASE_URL" => Some(db_url.clone()),
        "ENCRYPTION_KEY" => Some(key.to_string()),
        "SERVICE_API_KEYS" => Some("admin-test:nsac_srv_feedback_admin_key_123456".to_string()),
        "NSAC_BASE_URL" => Some("http://127.0.0.1:9/nsac".to_string()),
        "SENTINEL_ENABLED" => Some("false".to_string()),
        "FCM_ENABLED" => Some("false".to_string()),
        _ => None,
    })
    .unwrap()
}

fn png_data_url(w: u32, h: u32) -> String {
    let img = DynamicImage::ImageRgba8(RgbaImage::from_pixel(w, h, Rgba([40, 140, 220, 255])));
    let mut buf = Vec::new();
    img.write_to(&mut Cursor::new(&mut buf), ImageFormat::Png)
        .unwrap();
    format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(buf)
    )
}

async fn read_json(resp: axum::response::Response) -> Value {
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn feedback_end_to_end_validation_attachments_and_admin_triage() {
    let pool = common::test_pool().await;
    let config = test_config();
    let state = AppState::with_default_ports(config, pool.clone()).unwrap();
    seed_service_clients(&state).await.unwrap();

    let crypto = CryptoService::from_hex_key(
        "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
    )
    .unwrap();
    let cookies = NsacSessionCookies::new("x", "s", None).unwrap();
    let email = format!("aluno.{}@feedback.test", common::unique_suffix());
    let account = AccountRepo::new(pool.clone())
        .upsert_on_login(&crypto, &email, "pw", &cookies, Some("31A"), None, None, Utc::now())
        .await
        .unwrap();
    let session = AuthRepo::new(pool.clone())
        .create_user_session(account.id, 3600, Utc::now())
        .await
        .unwrap();

    let app = build_router(state);

    // 1) Reject gibberish / low-effort bug submission
    let bad_req = Request::builder()
        .method("POST")
        .uri("/api/v1/feedback")
        .header("authorization", format!("Bearer {}", session.token.as_str()))
        .header("content-type", "application/json")
        .body(Body::from(
            json!({
                "kind": "bug",
                "area": "social",
                "severity": "minor",
                "frequency": "always",
                "title": "aaaaaaaaaaaaaaaaaa",
                "steps_to_reproduce": "1. Abri o chat e tentei enviar uma mensagem longa",
                "actual_behavior": "O teclado fechou imediatamente ao clicar no enviar"
            })
            .to_string(),
        ))
        .unwrap();
    let resp = app.clone().oneshot(bad_req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    // 2) Reject screenshots on non-bug (suggestion)
    let bad_attach_req = Request::builder()
        .method("POST")
        .uri("/api/v1/feedback")
        .header("authorization", format!("Bearer {}", session.token.as_str()))
        .header("content-type", "application/json")
        .body(Body::from(
            json!({
                "kind": "suggestion",
                "area": "ranking",
                "title": "Filtro por bimestre no ranking",
                "problem": "Hoje o ranking mistura todos os bimestres juntos na mesma lista.",
                "proposal": "Adicionar chips de 1B, 2B, 3B e 4B no topo do ranking da turma.",
                "attachments": [png_data_url(64, 64)]
            })
            .to_string(),
        ))
        .unwrap();
    let resp = app.clone().oneshot(bad_attach_req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    // 3) Valid bug report with 2 screenshots succeeds (201 Created)
    let ok_req = Request::builder()
        .method("POST")
        .uri("/api/v1/feedback")
        .header("authorization", format!("Bearer {}", session.token.as_str()))
        .header("content-type", "application/json")
        .body(Body::from(
            json!({
                "kind": "bug",
                "area": "social",
                "severity": "major",
                "frequency": "always",
                "title": "Selo de horário desalinhado sobre GIF",
                "steps_to_reproduce": "1. Abri uma conversa com um amigo\n2. Enviei um GIF pelo explorador\n3. Olhei o canto inferior direito",
                "actual_behavior": "O horário e o visto ficaram colados na borda inferior do selo escuro.",
                "expected_behavior": "O texto e o ícone deveriam ficar centralizados verticalmente.",
                "attachments": [png_data_url(1600, 900), png_data_url(200, 200)],
                "context": { "app_version": "1.4.2", "platform": "android_apk" }
            })
            .to_string(),
        ))
        .unwrap();
    let resp = app.clone().oneshot(ok_req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let created = read_json(resp).await;
    let submission_id = created["data"]["id"].as_i64().unwrap();
    assert_eq!(created["data"]["attachment_count"].as_i64(), Some(2));
    assert_eq!(created["data"]["status"].as_str(), Some("open"));

    // 4) Immediate second submission hits 10-minute cooldown (429)
    let rate_req = Request::builder()
        .method("POST")
        .uri("/api/v1/feedback")
        .header("authorization", format!("Bearer {}", session.token.as_str()))
        .header("content-type", "application/json")
        .body(Body::from(
            json!({
                "kind": "feedback",
                "area": "geral",
                "rating": 5,
                "title": "App muito rápido e bonito",
                "message": "Gostei bastante das animações novas no chat e do desempenho geral no celular."
            })
            .to_string(),
        ))
        .unwrap();
    let resp = app.clone().oneshot(rate_req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);

    // 5) Regular user cannot access admin endpoints (403)
    let forbidden_admin = Request::builder()
        .method("GET")
        .uri("/api/v1/admin/feedback")
        .header("authorization", format!("Bearer {}", session.token.as_str()))
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(forbidden_admin).await.unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);

    // 6) Admin lists feedback, fetches JPEG attachment, and updates status + dev_note
    let admin_list_req = Request::builder()
        .method("GET")
        .uri("/api/v1/admin/feedback?kind=bug&status=open")
        .header("x-service-key", "nsac_srv_feedback_admin_key_123456")
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(admin_list_req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let list_json = read_json(resp).await;
    let items = list_json["data"]["items"].as_array().unwrap();
    let found = items
        .iter()
        .find(|i| i["id"].as_i64() == Some(submission_id))
        .expect("created bug must appear in admin list");
    assert_eq!(found["author"]["account_id"].as_i64(), Some(account.id));
    assert_eq!(found["author"]["class_code"].as_str(), Some("31A"));
    let first_att_id = found["attachments"][0]["id"].as_i64().unwrap();
    assert_eq!(found["attachments"][0]["width"].as_i64(), Some(1280));
    assert_eq!(found["attachments"][0]["height"].as_i64(), Some(720));

    let att_req = Request::builder()
        .method("GET")
        .uri(format!(
            "/api/v1/admin/feedback/{submission_id}/attachments/{first_att_id}"
        ))
        .header("x-service-key", "nsac_srv_feedback_admin_key_123456")
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(att_req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers().get("content-type").unwrap().to_str().unwrap(),
        "image/jpeg"
    );
    assert_eq!(
        resp.headers()
            .get("x-content-type-options")
            .unwrap()
            .to_str()
            .unwrap(),
        "nosniff"
    );
    let img_bytes = resp.into_body().collect().await.unwrap().to_bytes();
    assert!(image::load_from_memory(&img_bytes).is_ok());

    let patch_req = Request::builder()
        .method("PATCH")
        .uri(format!("/api/v1/admin/feedback/{submission_id}"))
        .header("x-service-key", "nsac_srv_feedback_admin_key_123456")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({
                "status": "resolved",
                "dev_note": "Alinhamento corrigido na versão 1.4.2!"
            })
            .to_string(),
        ))
        .unwrap();
    let resp = app.clone().oneshot(patch_req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let patched = read_json(resp).await;
    assert_eq!(patched["data"]["status"].as_str(), Some("resolved"));
    assert_eq!(
        patched["data"]["dev_note"].as_str(),
        Some("Alinhamento corrigido na versão 1.4.2!")
    );

    // 7) Student's /feedback/mine reflects the updated status and dev_note
    let mine_req = Request::builder()
        .method("GET")
        .uri("/api/v1/feedback/mine")
        .header("authorization", format!("Bearer {}", session.token.as_str()))
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(mine_req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let mine = read_json(resp).await;
    let first = &mine["data"][0];
    assert_eq!(first["id"].as_i64(), Some(submission_id));
    assert_eq!(first["status"].as_str(), Some("resolved"));
    assert_eq!(
        first["dev_note"].as_str(),
        Some("Alinhamento corrigido na versão 1.4.2!")
    );
}
