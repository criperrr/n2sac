use nsac_server::config::FcmConfig;
use nsac_server::fcm::{FcmClient, FcmDeliveryOutcome};
use serde_json::json;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn build_fcm_config(server: &MockServer, enabled: bool) -> FcmConfig {
    let base = server.uri();
    FcmConfig {
        enabled,
        project_id: Some("nsac-test-proj".to_string()),
        credentials_json: None,
        base_url: Some(base.clone()),
        endpoint_url: Some(format!(
            "{base}/v1/projects/nsac-test-proj/messages:send"
        )),
        oauth_token_url: Some(format!("{base}/oauth2/token")),
    }
}

#[tokio::test]
async fn send_grade_notification_200_ok_verifies_headers_and_payload() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/oauth2/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "test-oauth2-token-123",
            "expires_in": 3600,
            "token_type": "Bearer"
        })))
        .expect(1)
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/v1/projects/nsac-test-proj/messages:send"))
        .and(header("authorization", "Bearer test-oauth2-token-123"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "name": "projects/nsac-test-proj/messages/msg-001"
        })))
        .expect(1)
        .mount(&server)
        .await;

    let client = FcmClient::new(build_fcm_config(&server, true)).expect("create FcmClient");
    assert!(client.is_enabled());

    let subjects = vec!["Matemática".to_string(), "Física".to_string()];
    let bimesters = vec![1, 2];

    let outcome = client
        .send_grade_notification("device-token-abc", "31A-2026", &subjects, &bimesters)
        .await
        .expect("send notification");

    assert_eq!(
        outcome,
        FcmDeliveryOutcome::Delivered {
            message_id: "projects/nsac-test-proj/messages/msg-001".to_string()
        }
    );

    let received = server.received_requests().await.expect("received requests");
    let send_req = received
        .iter()
        .find(|r| r.url.path() == "/v1/projects/nsac-test-proj/messages:send")
        .expect("FCM send request captured");

    let body: serde_json::Value =
        serde_json::from_slice(&send_req.body).expect("parse request JSON");
    assert_eq!(body["message"]["token"], "device-token-abc");
    assert_eq!(
        body["message"]["notification"]["title"],
        "Novas notas no NSAC (31A-2026)"
    );
    assert_eq!(
        body["message"]["notification"]["body"],
        "Notas atualizadas: Matemática, Física (1, 2º bimestre)"
    );
    assert_eq!(body["message"]["data"]["class_code"], "31A-2026");
    assert_eq!(body["message"]["data"]["subjects"], "Matemática, Física");
    assert_eq!(body["message"]["data"]["bimesters"], "1, 2");
}

#[tokio::test]
async fn oauth2_token_is_cached_across_multiple_sends() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/oauth2/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "cached-bearer-token",
            "expires_in": 3600
        })))
        .expect(1)
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/v1/projects/nsac-test-proj/messages:send"))
        .and(header("authorization", "Bearer cached-bearer-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "name": "projects/nsac-test-proj/messages/msg-cached"
        })))
        .expect(3)
        .mount(&server)
        .await;

    let client = FcmClient::new(build_fcm_config(&server, true)).expect("create FcmClient");
    let subjects = vec!["Química".to_string()];
    let bimesters = vec![3];

    for idx in 0..3 {
        let token = format!("fcm-token-{idx}");
        let outcome = client
            .send_grade_notification(&token, "22B-2026", &subjects, &bimesters)
            .await
            .expect("send notification");
        assert_eq!(
            outcome,
            FcmDeliveryOutcome::Delivered {
                message_id: "projects/nsac-test-proj/messages/msg-cached".to_string()
            }
        );
    }
}

#[tokio::test]
async fn send_grade_notification_404_unregistered_returns_unregistered() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/oauth2/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "token-unreg",
            "expires_in": 3600
        })))
        .expect(1)
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/v1/projects/nsac-test-proj/messages:send"))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({
            "error": {
                "code": 404,
                "message": "Requested entity was not found.",
                "status": "NOT_FOUND",
                "details": [
                    {
                        "@type": "type.googleapis.com/google.firebase.fcm.v1.FcmError",
                        "errorCode": "UNREGISTERED"
                    }
                ]
            }
        })))
        .expect(1)
        .mount(&server)
        .await;

    let client = FcmClient::new(build_fcm_config(&server, true)).expect("create FcmClient");
    let outcome = client
        .send_grade_notification("stale-token", "11A-2026", &[], &[])
        .await
        .expect("send notification");

    assert_eq!(outcome, FcmDeliveryOutcome::Unregistered);
}

#[tokio::test]
async fn send_grade_notification_503_unavailable_returns_transient_failure() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/oauth2/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "token-503",
            "expires_in": 3600
        })))
        .expect(1)
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/v1/projects/nsac-test-proj/messages:send"))
        .respond_with(ResponseTemplate::new(503).set_body_json(json!({
            "error": {
                "code": 503,
                "message": "The service is currently unavailable.",
                "status": "UNAVAILABLE",
                "details": [
                    {
                        "@type": "type.googleapis.com/google.firebase.fcm.v1.FcmError",
                        "errorCode": "UNAVAILABLE"
                    }
                ]
            }
        })))
        .expect(1)
        .mount(&server)
        .await;

    let client = FcmClient::new(build_fcm_config(&server, true)).expect("create FcmClient");
    let outcome = client
        .send_grade_notification(
            "valid-token",
            "31A-2026",
            &["História".to_string()],
            &[4],
        )
        .await
        .expect("send notification");

    match outcome {
        FcmDeliveryOutcome::TransientFailure { status, detail } => {
            assert_eq!(status, 503);
            assert!(
                detail.contains("UNAVAILABLE"),
                "expected detail to mention UNAVAILABLE, got: {detail}"
            );
        }
        other => panic!("expected TransientFailure, got: {other:?}"),
    }
}

#[tokio::test]
async fn send_grade_notification_disabled_mode_returns_disabled_without_http_calls() {
    let server = MockServer::start().await;

    let client = FcmClient::new(build_fcm_config(&server, false)).expect("create FcmClient");
    assert!(!client.is_enabled());

    let outcome = client
        .send_grade_notification("any-token", "31A-2026", &[], &[])
        .await
        .expect("send notification");

    assert_eq!(outcome, FcmDeliveryOutcome::Disabled);

    let received = server.received_requests().await.expect("received requests");
    assert!(received.is_empty(), "expected zero HTTP requests when disabled");
}
