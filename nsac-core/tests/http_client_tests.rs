use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use nsac_core::client::NsacClient;
use nsac_core::error::ErrorCode;
use nsac_core::models::NsacSessionCookies;
use wiremock::matchers::{body_string_contains, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn login_follows_301_trailing_slash_parses_reordered_cookies_and_verifies_home() {
    let server = MockServer::start().await;
    let base_url = format!("{}/nsac", server.uri());

    // 1. GET /nsac -> 301 /nsac/ (no Set-Cookie)
    Mock::given(method("GET"))
        .and(path("/nsac"))
        .respond_with(
            ResponseTemplate::new(301)
                .insert_header("Location", format!("{}/nsac/", server.uri())),
        )
        .mount(&server)
        .await;

    // 2. GET /nsac/ -> 200 login HTML + XSRF-TOKEN & nsaconline_session
    Mock::given(method("GET"))
        .and(path("/nsac/"))
        .respond_with(
            ResponseTemplate::new(200)
                .append_header(
                    "Set-Cookie",
                    "nsaconline_session=init_sess_111%3D; Path=/; HttpOnly",
                )
                .append_header("Set-Cookie", "XSRF-TOKEN=init_xsrf_222%3D; Path=/")
                .set_body_string(include_str!("fixtures/login_page.html")),
        )
        .mount(&server)
        .await;

    // 3. POST /nsac/login -> 302 /nsac/home + reordered cookies including remember_web_* (PF-06)
    Mock::given(method("POST"))
        .and(path("/nsac/login"))
        .and(body_string_contains(
            "_token=SYNTHETIC_CSRF_TOKEN_40_CHARACTERS_LONG1",
        ))
        .and(body_string_contains("remember=on"))
        .respond_with(
            ResponseTemplate::new(302)
                .insert_header("Location", format!("{}/nsac/home", server.uri()))
                .append_header(
                    "Set-Cookie",
                    "remember_web_59ba36addc=rem_token_999; Path=/; HttpOnly",
                )
                .append_header("Set-Cookie", "XSRF-TOKEN=auth_xsrf_333; Path=/")
                .append_header(
                    "Set-Cookie",
                    "nsaconline_session=auth_sess_444; Path=/; HttpOnly",
                ),
        )
        .mount(&server)
        .await;

    // 4. GET /nsac/home -> 200 home HTML
    Mock::given(method("GET"))
        .and(path("/nsac/home"))
        .and(header(
            "Cookie",
            "XSRF-TOKEN=auth_xsrf_333; nsaconline_session=auth_sess_444; remember_web_59ba36addc=rem_token_999",
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(include_str!("fixtures/home_authenticated.html")),
        )
        .mount(&server)
        .await;

    let client = NsacClient::new(base_url).unwrap();
    let cookies = client.login("aluno@unesp.br", "senha-teste").await.unwrap();

    assert_eq!(cookies.xsrf_token(), "auth_xsrf_333");
    assert_eq!(cookies.session(), "auth_sess_444");
    assert_eq!(
        cookies.remember_web(),
        Some(("remember_web_59ba36addc", "rem_token_999"))
    );
}

#[tokio::test]
async fn login_rejects_bad_credentials_redirecting_to_login() {
    let server = MockServer::start().await;
    let base_url = format!("{}/nsac", server.uri());

    Mock::given(method("GET"))
        .and(path("/nsac"))
        .respond_with(
            ResponseTemplate::new(200)
                .append_header("Set-Cookie", "XSRF-TOKEN=xsrf1; Path=/")
                .append_header("Set-Cookie", "nsaconline_session=sess1; Path=/")
                .set_body_string(include_str!("fixtures/login_page.html")),
        )
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/nsac/login"))
        .respond_with(
            ResponseTemplate::new(302)
                .insert_header("Location", format!("{}/nsac/login", server.uri())),
        )
        .mount(&server)
        .await;

    let client = NsacClient::new(base_url).unwrap();
    let err = client.login("wrong@unesp.br", "badpass").await.unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidNsacCredentials);
}

#[tokio::test]
async fn pf04_explicit_timeout_returns_nsac_unavailable() {
    let server = MockServer::start().await;
    let base_url = format!("{}/nsac", server.uri());

    Mock::given(method("GET"))
        .and(path("/nsac/home"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(Duration::from_millis(250))
                .set_body_string(include_str!("fixtures/home_authenticated.html")),
        )
        .mount(&server)
        .await;

    let client = NsacClient::with_timeout(base_url, Duration::from_millis(50)).unwrap();
    let cookies = NsacSessionCookies::new("xsrf", "sess", None).unwrap();

    let err = client.fetch_home(&cookies).await.unwrap_err();
    assert_eq!(err.code, ErrorCode::NsacUnavailable);
}

#[test]
fn pf06_cookie_parsing_handles_two_cookies_and_arbitrary_order() {
    let reordered = [
        "remember_web_abc=rem_1; Path=/; HttpOnly",
        "ignored_cookie=foo; Path=/",
        "nsaconline_session=sess_2%3D; Path=/",
        "XSRF-TOKEN=xsrf_3%3D; Path=/",
    ];
    let parsed = NsacSessionCookies::from_set_cookie_headers(reordered).unwrap();
    assert_eq!(parsed.xsrf_token(), "xsrf_3%3D");
    assert_eq!(parsed.session(), "sess_2%3D");
    assert_eq!(parsed.remember_web(), Some(("remember_web_abc", "rem_1")));

    // Omitting remember_web_* works cleanly without "undefined"
    let two_only = [
        "nsaconline_session=sess_only; Path=/",
        "XSRF-TOKEN=xsrf_only; Path=/",
    ];
    let parsed_two = NsacSessionCookies::from_set_cookie_headers(two_only).unwrap();
    assert!(!parsed_two.has_remember_cookie());
    assert_eq!(
        parsed_two.to_cookie_header().as_str(),
        "XSRF-TOKEN=xsrf_only; nsaconline_session=sess_only"
    );
}

#[tokio::test]
async fn verify_cookie_with_refresh_handles_valid_rotated_and_expired_sessions() {
    let server = MockServer::start().await;
    let base_url = format!("{}/nsac", server.uri());
    let cookies = NsacSessionCookies::new("old_xsrf", "old_sess", None).unwrap();

    // Case 1: 200 OK with rotated Set-Cookie
    Mock::given(method("GET"))
        .and(path("/nsac/home"))
        .and(header("Cookie", "XSRF-TOKEN=old_xsrf; nsaconline_session=old_sess"))
        .respond_with(
            ResponseTemplate::new(200)
                .append_header("Set-Cookie", "XSRF-TOKEN=rot_xsrf; Path=/")
                .append_header("Set-Cookie", "nsaconline_session=rot_sess; Path=/")
                .set_body_string(include_str!("fixtures/home_authenticated.html")),
        )
        .mount(&server)
        .await;

    let client = NsacClient::new(base_url.clone()).unwrap();
    let verified = client.verify_cookie(&cookies).await.unwrap();
    assert_eq!(verified.xsrf_token(), "rot_xsrf");
    assert_eq!(verified.session(), "rot_sess");

    // Case 2: 302 -> login without refresh callback -> NsacAuthExpired
    let expired_cookies = NsacSessionCookies::new("exp_xsrf", "exp_sess", None).unwrap();
    Mock::given(method("GET"))
        .and(path("/nsac/home"))
        .and(header("Cookie", "XSRF-TOKEN=exp_xsrf; nsaconline_session=exp_sess"))
        .respond_with(
            ResponseTemplate::new(302)
                .insert_header("Location", format!("{}/nsac/login", server.uri())),
        )
        .mount(&server)
        .await;

    let err = client.verify_cookie(&expired_cookies).await.unwrap_err();
    assert_eq!(err.code, ErrorCode::NsacAuthExpired);

    // Case 3: 302 -> login WITH refresh callback -> invokes callback and returns refreshed cookies
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_clone = Arc::clone(&calls);
    let refreshed = client
        .verify_cookie_with_refresh(
            &expired_cookies,
            Some(move || {
                calls_clone.fetch_add(1, Ordering::SeqCst);
                async move { NsacSessionCookies::new("new_xsrf", "new_sess", None) }
            }),
        )
        .await
        .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(refreshed.xsrf_token(), "new_xsrf");
}

#[tokio::test]
async fn fetch_boletim_propagates_rotated_cookies_and_ficha_detail_handles_302() {
    let server = MockServer::start().await;
    let base_url = format!("{}/nsac", server.uri());
    let cookies = NsacSessionCookies::new(
        "xsrf_1",
        "sess_1",
        Some(("remember_web_xyz".to_string(), "rem_1".to_string())),
    )
    .unwrap();

    Mock::given(method("GET"))
        .and(path("/nsac/aluno/boletim"))
        .respond_with(
            ResponseTemplate::new(200)
                .append_header("Set-Cookie", "XSRF-TOKEN=xsrf_2; Path=/")
                .append_header("Set-Cookie", "nsaconline_session=sess_2; Path=/")
                .set_body_string(include_str!("fixtures/boletim_noturno_inf1.html")),
        )
        .mount(&server)
        .await;

    // Ficha 1 unavailable: 302 -> /nsac/aluno/ficha-avaliacao => Ok((None, ...))
    Mock::given(method("GET"))
        .and(path("/nsac/aluno/ficha-avaliacao/1"))
        .respond_with(
            ResponseTemplate::new(302).insert_header(
                "Location",
                format!("{}/nsac/aluno/ficha-avaliacao", server.uri()),
            ),
        )
        .mount(&server)
        .await;

    // Ficha 2 available: 200 OK
    Mock::given(method("GET"))
        .and(path("/nsac/aluno/ficha-avaliacao/2"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(include_str!("fixtures/ficha_avaliacao_detail.html")),
        )
        .mount(&server)
        .await;

    // Ficha 3 expired session: 302 -> /nsac/login => Err(NsacAuthExpired)
    Mock::given(method("GET"))
        .and(path("/nsac/aluno/ficha-avaliacao/3"))
        .respond_with(
            ResponseTemplate::new(302)
                .insert_header("Location", format!("{}/nsac/login", server.uri())),
        )
        .mount(&server)
        .await;

    let client = NsacClient::new(base_url).unwrap();

    let (boletim, rotated) = client.fetch_boletim(&cookies).await.unwrap();
    assert_eq!(boletim.current_class_code(), Some("INF1"));
    let rotated = rotated.unwrap();
    assert_eq!(rotated.xsrf_token(), "xsrf_2");
    assert_eq!(rotated.session(), "sess_2");
    // Remember cookie preserved even though rotation omitted it
    assert_eq!(rotated.remember_web(), Some(("remember_web_xyz", "rem_1")));

    let (ficha1, _) = client.fetch_ficha_avaliacao_detail(&cookies, 1).await.unwrap();
    assert!(ficha1.is_none());

    let (ficha2, _) = client.fetch_ficha_avaliacao_detail(&cookies, 2).await.unwrap();
    assert_eq!(ficha2.unwrap().bimester, 2);

    let err3 = client
        .fetch_ficha_avaliacao_detail(&cookies, 3)
        .await
        .unwrap_err();
    assert_eq!(err3.code, ErrorCode::NsacAuthExpired);

    let err_bounds = client
        .fetch_ficha_avaliacao_detail(&cookies, 0)
        .await
        .unwrap_err();
    assert_eq!(err_bounds.code, ErrorCode::InvalidParams);
}

#[tokio::test]
async fn fetch_home_atrasos_saidas_and_ficha_list_over_http() {
    let server = MockServer::start().await;
    let base_url = format!("{}/nsac", server.uri());
    let cookies = NsacSessionCookies::new("xsrf_r", "sess_r", None).unwrap();

    Mock::given(method("GET"))
        .and(path("/nsac/home"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(include_str!("fixtures/home_authenticated.html")),
        )
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/nsac/aluno/atrasos"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(include_str!("fixtures/atrasos_populated.html")),
        )
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/nsac/aluno/saidas"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(include_str!("fixtures/saidas_empty.html")),
        )
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/nsac/aluno/ficha-avaliacao"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(include_str!("fixtures/ficha_avaliacao_list.html")),
        )
        .mount(&server)
        .await;

    let client = NsacClient::new(base_url).unwrap();

    let (home, _) = client.fetch_home(&cookies).await.unwrap();
    assert_eq!(home.portal_date.as_deref(), Some("19/11/2026"));

    let (atrasos, _) = client.fetch_atrasos(&cookies).await.unwrap();
    assert_eq!(atrasos.len(), 2);

    let (saidas, _) = client.fetch_saidas(&cookies).await.unwrap();
    assert!(saidas.is_empty());

    let (fichas, _) = client.fetch_ficha_avaliacao(&cookies).await.unwrap();
    assert_eq!(fichas.len(), 3);
    assert!(fichas[1].available);
}

#[tokio::test]
async fn login_ignores_max_age_zero_and_deleted_cookies_during_login_redirects() {
    // Part A: Redirect hop sets valid cookies, final login page tries to clear them with Max-Age=0 / deleted.
    // Valid cookies must survive and be sent on POST /nsac/login.
    let server = MockServer::start().await;
    let base_url = format!("{}/nsac", server.uri());

    Mock::given(method("GET"))
        .and(path("/nsac"))
        .respond_with(
            ResponseTemplate::new(301)
                .insert_header("Location", format!("{}/nsac/", server.uri()))
                .append_header("Set-Cookie", "XSRF-TOKEN=good_xsrf; Path=/")
                .append_header("Set-Cookie", "nsaconline_session=good_sess; Path=/"),
        )
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/nsac/"))
        .respond_with(
            ResponseTemplate::new(200)
                .append_header("Set-Cookie", "XSRF-TOKEN=bad_expired; Path=/; Max-Age=0")
                .append_header("Set-Cookie", "nsaconline_session=deleted; Path=/")
                .append_header("Set-Cookie", "remember_web_abc=stale; Path=/; Max-Age=-1")
                .set_body_string(include_str!("fixtures/login_page.html")),
        )
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/nsac/login"))
        .and(header(
            "Cookie",
            "XSRF-TOKEN=good_xsrf; nsaconline_session=good_sess",
        ))
        .respond_with(
            ResponseTemplate::new(302)
                .insert_header("Location", format!("{}/nsac/home", server.uri()))
                .append_header("Set-Cookie", "XSRF-TOKEN=auth_xsrf; Path=/")
                .append_header("Set-Cookie", "nsaconline_session=auth_sess; Path=/"),
        )
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/nsac/home"))
        .and(header(
            "Cookie",
            "XSRF-TOKEN=auth_xsrf; nsaconline_session=auth_sess",
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(include_str!("fixtures/home_authenticated.html")),
        )
        .mount(&server)
        .await;

    let client = NsacClient::new(base_url).unwrap();
    let cookies = client.login("aluno@unesp.br", "senha-teste").await.unwrap();
    assert_eq!(cookies.xsrf_token(), "auth_xsrf");
    assert_eq!(cookies.session(), "auth_sess");
    assert!(!cookies.has_remember_cookie());

    // Part B: If only Max-Age=0 XSRF-TOKEN is provided, login must fail instead of sending expired cookie.
    let server2 = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/nsac"))
        .respond_with(
            ResponseTemplate::new(200)
                .append_header("Set-Cookie", "XSRF-TOKEN=expired; Path=/; Max-Age=0")
                .append_header("Set-Cookie", "nsaconline_session=good_sess; Path=/")
                .set_body_string(include_str!("fixtures/login_page.html")),
        )
        .mount(&server2)
        .await;

    let client2 = NsacClient::new(format!("{}/nsac", server2.uri())).unwrap();
    let err = client2
        .login("aluno@unesp.br", "senha-teste")
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::NsacUnavailable);
}

#[tokio::test]
async fn location_points_to_login_handles_nsac_without_trailing_slash() {
    let server = MockServer::start().await;
    let base_url = format!("{}/nsac", server.uri());
    let cookies = NsacSessionCookies::new("xsrf_r", "sess_r", None).unwrap();

    // 1. Absolute Location: .../nsac (no trailing slash) on ficha-avaliacao/3 -> NsacAuthExpired
    Mock::given(method("GET"))
        .and(path("/nsac/aluno/ficha-avaliacao/3"))
        .respond_with(
            ResponseTemplate::new(302)
                .insert_header("Location", format!("{}/nsac", server.uri())),
        )
        .mount(&server)
        .await;

    // 2. Relative Location: /nsac?expired=1 on ficha-avaliacao/2 -> NsacAuthExpired
    Mock::given(method("GET"))
        .and(path("/nsac/aluno/ficha-avaliacao/2"))
        .respond_with(
            ResponseTemplate::new(302).insert_header("Location", "/nsac?expired=1"),
        )
        .mount(&server)
        .await;

    // 3. /nsac/home must NOT falsely match login -> NsacUnavailable from fetch_ficha_avaliacao_detail
    Mock::given(method("GET"))
        .and(path("/nsac/aluno/ficha-avaliacao/4"))
        .respond_with(
            ResponseTemplate::new(302)
                .insert_header("Location", format!("{}/nsac/home", server.uri())),
        )
        .mount(&server)
        .await;

    // 4. login() POST -> 302 Location: .../nsac (no trailing slash, no /home mounted) -> InvalidNsacCredentials
    Mock::given(method("GET"))
        .and(path("/nsac"))
        .respond_with(
            ResponseTemplate::new(200)
                .append_header("Set-Cookie", "XSRF-TOKEN=xsrf1; Path=/")
                .append_header("Set-Cookie", "nsaconline_session=sess1; Path=/")
                .set_body_string(include_str!("fixtures/login_page.html")),
        )
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/nsac/login"))
        .respond_with(
            ResponseTemplate::new(302)
                .insert_header("Location", format!("{}/nsac", server.uri())),
        )
        .mount(&server)
        .await;

    let client = NsacClient::new(base_url).unwrap();

    let err3 = client
        .fetch_ficha_avaliacao_detail(&cookies, 3)
        .await
        .unwrap_err();
    assert_eq!(err3.code, ErrorCode::NsacAuthExpired);

    let err2 = client
        .fetch_ficha_avaliacao_detail(&cookies, 2)
        .await
        .unwrap_err();
    assert_eq!(err2.code, ErrorCode::NsacAuthExpired);

    let err4 = client
        .fetch_ficha_avaliacao_detail(&cookies, 4)
        .await
        .unwrap_err();
    assert_eq!(err4.code, ErrorCode::NsacUnavailable);

    let login_err = client.login("wrong@unesp.br", "badpass").await.unwrap_err();
    assert_eq!(login_err.code, ErrorCode::InvalidNsacCredentials);
}

/// The NSAC serves the ficha as a PDF: it is passed through byte for byte (never as text),
/// and a not-yet-released bimester (redirect to another ficha) is `None`.
#[tokio::test]
async fn ficha_pdf_is_passed_through_and_unreleased_is_none() {
    let server = MockServer::start().await;
    let base_url = format!("{}/nsac", server.uri());
    let cookies = NsacSessionCookies::new("xsrf_p", "sess_p", None).unwrap();
    // Binary payload with bytes that are invalid UTF-8: reading it as text would corrupt it.
    let pdf: Vec<u8> = [b"%PDF-1.7\n".as_slice(), &[0xE2, 0xE3, 0xCF, 0xD3, 0xFF, 0x00, 0x9C], b"\n%%EOF\n"].concat();

    Mock::given(method("GET"))
        .and(path("/nsac/aluno/ficha-avaliacao/1"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Content-Type", "application/pdf")
                .set_body_bytes(pdf.clone()),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/nsac/aluno/ficha-avaliacao/2"))
        .respond_with(
            ResponseTemplate::new(302)
                .insert_header("Location", format!("{}/nsac/aluno/ficha-avaliacao/1", server.uri())),
        )
        .mount(&server)
        .await;

    let client = NsacClient::new(base_url).unwrap();
    let (bytes, _) = client.fetch_ficha_avaliacao_pdf(&cookies, 1).await.unwrap();
    assert_eq!(bytes.as_deref(), Some(pdf.as_slice()));
    let (unreleased, _) = client.fetch_ficha_avaliacao_pdf(&cookies, 2).await.unwrap();
    assert!(unreleased.is_none());
    assert!(client.fetch_ficha_avaliacao_pdf(&cookies, 5).await.is_err());
}
