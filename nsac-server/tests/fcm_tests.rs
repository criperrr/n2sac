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
    // Grade pushes use high-priority data-only FCM payloads so Android natively accumulates them.
    assert!(body["message"].get("notification").is_none());
    assert_eq!(
        body["message"]["data"]["title"],
        "Novas notas no NSAC (31A-2026)"
    );
    assert_eq!(
        body["message"]["data"]["body"],
        "Notas atualizadas: Matemática, Física (1, 2º bimestre)"
    );
    assert_eq!(body["message"]["data"]["channel_id"], "n2sac-grades-v2");
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

#[tokio::test]
async fn send_social_notifications_route_to_high_priority_android_channels() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/oauth2/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "social-oauth2-token",
            "expires_in": 3600
        })))
        .expect(1)
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/v1/projects/nsac-test-proj/messages:send"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "name": "projects/nsac-test-proj/messages/social-ok"
        })))
        .expect(3)
        .mount(&server)
        .await;

    let client = FcmClient::new(build_fcm_config(&server, true)).expect("create FcmClient");

    let cases = [
        ("chat_message", "n2sac-social-messages-v2", "chat-42", "Ana Beatriz", "Oi, tudo bem?"),
        ("follow", "n2sac-social-follows-v2", "follow-42", "Novo seguidor no n²sac", "Ana Beatriz começou a seguir você!"),
        ("friendship", "n2sac-social-friends-v2", "friend-42", "Amizade conquistada!", "Ana Beatriz agora é seu amigo!"),
    ];

    for (kind, _ch, _tag, title, body) in cases {
        let outcome = client
            .send_social_notification("device-tok", title, body, kind, 42, "Ana Beatriz")
            .await
            .expect("send social push");
        assert!(matches!(outcome, FcmDeliveryOutcome::Delivered { .. }));
    }

    let received = server.received_requests().await.expect("received requests");
    let pushes: Vec<serde_json::Value> = received
        .iter()
        .filter(|r| r.url.path() == "/v1/projects/nsac-test-proj/messages:send")
        .map(|r| serde_json::from_slice(&r.body).expect("json"))
        .collect();
    assert_eq!(pushes.len(), 3);

    for (push, (kind, expected_ch, expected_tag, _, _)) in pushes.iter().zip(cases.iter()) {
        assert_eq!(push["message"]["data"]["kind"], *kind);
        assert_eq!(push["message"]["data"]["actor_id"], "42");
        assert_eq!(push["message"]["data"]["actor_label"], "Ana Beatriz");
        assert_eq!(push["message"]["android"]["priority"], "high");
        assert_eq!(push["message"]["android"]["notification"]["channel_id"], *expected_ch);
        assert_eq!(push["message"]["android"]["notification"]["tag"], *expected_tag);
        assert_eq!(push["message"]["android"]["notification"]["default_sound"], true);
        assert_eq!(push["message"]["android"]["notification"]["default_vibrate_timings"], true);
    }
}

#[tokio::test]
async fn send_announcement_and_grade_notifications_use_priority_max_v2_channels() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/oauth2/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "ann-oauth2-token",
            "expires_in": 3600
        })))
        .expect(1)
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/v1/projects/nsac-test-proj/messages:send"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "name": "projects/nsac-test-proj/messages/ann-ok"
        })))
        .expect(2)
        .mount(&server)
        .await;

    let client = FcmClient::new(build_fcm_config(&server, true)).expect("create FcmClient");

    let g = client
        .send_grade_notification("tok-g", "INI3", &["Física".to_string()], &[4])
        .await
        .expect("send grade push");
    assert!(matches!(g, FcmDeliveryOutcome::Delivered { .. }));

    let a = client
        .send_announcement_notification(
            "tok-a",
            "INI3",
            77,
            "Lucas",
            "exam",
            "Prova adiada",
            "A prova de Física passou para sexta.",
        )
        .await
        .expect("send announcement push");
    assert!(matches!(a, FcmDeliveryOutcome::Delivered { .. }));

    let received = server.received_requests().await.expect("received requests");
    let pushes: Vec<serde_json::Value> = received
        .iter()
        .filter(|r| r.url.path() == "/v1/projects/nsac-test-proj/messages:send")
        .map(|r| serde_json::from_slice(&r.body).expect("json"))
        .collect();
    assert_eq!(pushes.len(), 2);

    assert_eq!(pushes[0]["message"]["data"]["kind"], "grade_update");
    assert_eq!(pushes[0]["message"]["data"]["channel_id"], "n2sac-grades-v2");
    assert_eq!(pushes[0]["message"]["android"]["priority"], "high");
    assert!(pushes[0]["message"].get("notification").is_none());

    assert_eq!(pushes[1]["message"]["data"]["kind"], "class_announcement");
    assert_eq!(pushes[1]["message"]["android"]["priority"], "high");
    assert_eq!(pushes[1]["message"]["android"]["notification"]["channel_id"], "n2sac-announcements-v1");
    assert_eq!(pushes[1]["message"]["android"]["notification"]["notification_priority"], "PRIORITY_MAX");
    assert_eq!(pushes[1]["message"]["android"]["notification"]["default_sound"], true);
}

#[tokio::test]
async fn send_grade_notification_detailed_includes_personal_grade_and_highlights_json() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/oauth2/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "detailed-token",
            "expires_in": 3600
        })))
        .expect(1)
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/v1/projects/nsac-test-proj/messages:send"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "name": "projects/nsac-test-proj/messages/grade-detailed-1"
        })))
        .expect(1)
        .mount(&server)
        .await;

    let client = FcmClient::new(build_fcm_config(&server, true)).expect("create FcmClient");
    let highlights = vec![nsac_server::fcm::StudentGradeHighlight {
        subject: "Física".to_string(),
        bimester: 2,
        year: 2026,
        personal_grade: Some(8.5),
        class_average: Some(7.1),
        previous_grade: Some(7.0),
        recovery: false,
    }];

    let outcome = client
        .send_grade_notification_detailed(
            "tok-detailed",
            "INI3",
            &["Física".to_string()],
            &[2],
            &highlights,
        )
        .await
        .expect("send detailed grade push");
    assert!(matches!(outcome, FcmDeliveryOutcome::Delivered { .. }));

    let received = server.received_requests().await.expect("received requests");
    let push: serde_json::Value = received
        .iter()
        .find(|r| r.url.path() == "/v1/projects/nsac-test-proj/messages:send")
        .map(|r| serde_json::from_slice(&r.body).expect("json"))
        .expect("grade push request");

    assert_eq!(push["message"]["data"]["title"], "Nova nota em Física: 8,5");
    let body = push["message"]["data"]["body"].as_str().unwrap_or("");
    assert!(body.contains("2º Bim"), "body={body}");
    assert!(body.contains("7,1"), "body={body}");
    assert!(body.contains("+1,5"), "body={body}");
    assert_eq!(push["message"]["data"]["primary_subject"], "Física");
    assert_eq!(push["message"]["data"]["primary_bimester"], "2");
    assert_eq!(push["message"]["data"]["primary_year"], "2026");
    assert_eq!(push["message"]["data"]["primary_grade"], "8.50");
    assert_eq!(push["message"]["data"]["primary_class_avg"], "7.10");
    let parsed_highlights: Vec<serde_json::Value> = serde_json::from_str(
        push["message"]["data"]["highlights_json"]
            .as_str()
            .expect("highlights_json"),
    )
    .expect("valid highlights_json");
    assert_eq!(parsed_highlights.len(), 1);
    assert_eq!(parsed_highlights[0]["subject"], "Física");
}

#[tokio::test]
async fn send_grade_notification_four_plus_hides_individual_preview_and_keeps_all_highlights() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/oauth2/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "multi-token",
            "expires_in": 3600
        })))
        .expect(1)
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/v1/projects/nsac-test-proj/messages:send"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "name": "projects/nsac-test-proj/messages/grade-multi-4"
        })))
        .expect(1)
        .mount(&server)
        .await;

    let client = FcmClient::new(build_fcm_config(&server, true)).expect("create FcmClient");
    let highlights = vec![
        nsac_server::fcm::StudentGradeHighlight {
            subject: "Física".to_string(),
            bimester: 3,
            year: 2026,
            personal_grade: Some(8.8),
            class_average: Some(7.1),
            previous_grade: Some(7.8),
            recovery: false,
        },
        nsac_server::fcm::StudentGradeHighlight {
            subject: "Matemática".to_string(),
            bimester: 3,
            year: 2026,
            personal_grade: Some(9.2),
            class_average: Some(7.4),
            previous_grade: Some(8.5),
            recovery: false,
        },
        nsac_server::fcm::StudentGradeHighlight {
            subject: "Programação Web".to_string(),
            bimester: 3,
            year: 2026,
            personal_grade: Some(9.8),
            class_average: Some(8.1),
            previous_grade: Some(9.4),
            recovery: false,
        },
        nsac_server::fcm::StudentGradeHighlight {
            subject: "Química".to_string(),
            bimester: 3,
            year: 2026,
            personal_grade: Some(7.6),
            class_average: Some(6.9),
            previous_grade: Some(6.8),
            recovery: false,
        },
    ];

    let subjects: Vec<String> = highlights.iter().map(|h| h.subject.clone()).collect();
    let outcome = client
        .send_grade_notification_detailed("tok-multi", "INI3", &subjects, &[3], &highlights)
        .await
        .expect("send multi grade push");
    assert!(matches!(outcome, FcmDeliveryOutcome::Delivered { .. }));

    let received = server.received_requests().await.expect("received requests");
    let push: serde_json::Value = received
        .iter()
        .find(|r| r.url.path() == "/v1/projects/nsac-test-proj/messages:send")
        .map(|r| serde_json::from_slice(&r.body).expect("json"))
        .expect("grade push request");

    assert_eq!(
        push["message"]["data"]["title"],
        "Múltiplas novas notas saíram (4)"
    );
    let body = push["message"]["data"]["body"].as_str().unwrap_or("");
    assert!(body.contains("Múltiplas notas novas saíram"), "body={body}");
    assert!(!body.contains("8,8"), "4+ grade notification should not inline individual scores: {body}");
    let parsed_highlights: Vec<serde_json::Value> = serde_json::from_str(
        push["message"]["data"]["highlights_json"]
            .as_str()
            .expect("highlights_json"),
    )
    .expect("valid highlights_json");
    assert_eq!(parsed_highlights.len(), 4);
}
