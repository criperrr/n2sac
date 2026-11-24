mod common;

use std::sync::{Arc, RwLock};

use chrono::{DateTime, Duration as ChronoDuration, TimeZone, Utc};
use nsac_core::models::NsacSessionCookies;
use nsac_core::parsers::parse_boletim;
use nsac_core::sentinel::compute_class_grade_hash;
use nsac_server::config::{CacheConfig, Config, FcmConfig, SentinelConfig};
use nsac_server::fcm::FcmClient;
use nsac_server::http::nsac_routes::canonicalize_boletim;
use nsac_server::ports::{Clock, HeuristicObserver};
use nsac_server::sentinel::{
    SentinelHeuristic, handle_manual_refresh_class_change, poll_due_classes_once,
};
use nsac_server::state::AppState;
use sqlx::PgPool;
use tokio::sync::Mutex;
use wiremock::matchers::{body_string_contains, header_regex, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};
use zeroize::Zeroizing;

const TEST_ENC_KEY: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

static LAB_TEST_MUTEX: Mutex<()> = Mutex::const_new(());

#[derive(Clone)]
struct ManualClock {
    now: Arc<RwLock<DateTime<Utc>>>,
}

impl ManualClock {
    fn new(initial: DateTime<Utc>) -> Self {
        Self {
            now: Arc::new(RwLock::new(initial)),
        }
    }

    fn set(&self, next: DateTime<Utc>) {
        if let Ok(mut guard) = self.now.write() {
            *guard = next;
        }
    }
}

impl Clock for ManualClock {
    fn now(&self) -> DateTime<Utc> {
        self.now
            .read()
            .map(|g| *g)
            .unwrap_or_else(|_| Utc.with_ymd_and_hms(2026, 11, 24, 14, 0, 0).unwrap())
    }
}

fn production_sentinel_config() -> SentinelConfig {
    SentinelConfig {
        enabled: true,
        tick_ms: 10_000,
        cold_baseline_secs: 604_800, // 7 days
        historical_radius_days: 4,
        historical_interval_secs: 7_200, // 2h
        hot_min_interval_secs: 600,      // 10 min
        hot_max_interval_secs: 600,      // 10 min
        hot_duration_secs: 7_200,        // 2h
        api_volume_weight: 0.15,
        api_cooldown_secs: 1_800,
        midnight_backoff_secs: 1_800,
        jitter_max_ms: 0,
    }
}

fn make_lab_config(
    nsac_base_url: String,
    fcm_server_uri: String,
    max_concurrent: usize,
    sentinel: SentinelConfig,
) -> Config {
    Config {
        http_bind: "127.0.0.1:0".parse().expect("valid SocketAddr"),
        rust_log: "warn".to_string(),
        database_url: Zeroizing::new("postgres://postgres@127.0.0.1:5432/nsac_m1_test".to_string()),
        encryption_key: Zeroizing::new(TEST_ENC_KEY.to_string()),
        service_api_keys: Vec::new(),
        user_session_ttl_secs: 3600,
        nsac_base_url: format!("{nsac_base_url}/nsac"),
        nsac_timeout_secs: 5,
        nsac_max_concurrent_requests: max_concurrent,
        sentinel,
        fcm: FcmConfig {
            enabled: true,
            project_id: Some("nsac-lab-proj".to_string()),
            credentials_json: None,
            base_url: Some(fcm_server_uri.clone()),
            endpoint_url: Some(format!(
                "{fcm_server_uri}/v1/projects/nsac-lab-proj/messages:send"
            )),
            oauth_token_url: Some(format!("{fcm_server_uri}/oauth2/token")),
        },
        cache: CacheConfig {
            boletim_ttl_secs: 300,
            snapshot_ttl_secs: 300,
            auth_ttl_secs: 0,
        },
    }
}

#[allow(clippy::too_many_arguments)]
fn render_boletim_with_recovery(
    class_code: &str,
    year: u16,
    subject_title: &str,
    subject_abbrev: &str,
    mb_b1: f64,
    mt_b1: f64,
    rec_b1_code: &str,
    rec_b1_title: &str,
    mb_b2: Option<f64>,
    mt_b2: Option<f64>,
) -> String {
    let mb2_str = mb_b2.map_or_else(|| "-".to_string(), |v| format!("{v:.1}"));
    let mt2_str = mt_b2.map_or_else(|| "-".to_string(), |v| format!("{v:.1}"));
    format!(
        r#"<!DOCTYPE html>
<html>
<head><meta charset="utf-8"><title>NSac Online - Boletim</title></head>
<body>
<div class="content-wrapper">
  <section class="content-header"><h1>Boletim</h1></section>
  <section class="content">
    <div class="box box-primary">
      <div class="box-header with-border">
        <h4 class="col-md-8 col-xs-12">{class_code}/{year}</h4>
      </div>
      <div class="box-body">
        <table id="notas" class="table table-responsive table-notas">
          <tbody>
            <tr class="linha">
              <td><span title="{subject_title}">{subject_abbrev}</span></td>
              <td class="text-center"><span>{mb_b1:.1}</span></td>
              <td class="text-center">{mt_b1:.1}</td>
              <td class="text-center bd">0</td>
              <td class="text-center bd"><span data-toggle="tooltip" title="{rec_b1_title}">{rec_b1_code}</span></td>
              <td class="text-center"><span>{mb2_str}</span></td>
              <td class="text-center">{mt2_str}</td>
              <td class="text-center bd">0</td>
              <td class="text-center bd"><span data-toggle="tooltip" title="Não Aconteceu">-</span></td>
              <td class="text-center"><span>-</span></td>
              <td class="text-center">-</td>
              <td class="text-center bd">0</td>
              <td class="text-center bd"><span data-toggle="tooltip" title="Não Aconteceu">-</span></td>
              <td class="text-center"><span>-</span></td>
              <td class="text-center">-</td>
              <td class="text-center bd">0</td>
              <td class="text-center bd"><span data-toggle="tooltip" title="Não Aconteceu">-</span></td>
              <td class="text-center">-</td>
              <td class="text-center">0</td>
            </tr>
          </tbody>
        </table>
      </div>
    </div>
  </section>
</div>
</body>
</html>"#
    )
}

async fn freeze_other_classes(pool: &PgPool, active_classes: &[String]) {
    let far_future = Utc.with_ymd_and_hms(2099, 1, 1, 0, 0, 0).unwrap();
    let _ = sqlx::query(
        "UPDATE sentinel_class_state SET next_check_at = $1 WHERE NOT (class_code = ANY($2))",
    )
    .bind(far_future)
    .bind(active_classes)
    .execute(pool)
    .await;
}

async fn cleanup_classes(pool: &PgPool, classes: &[String]) {
    for cls in classes {
        let _ = sqlx::query("DELETE FROM grade_posting_events WHERE class_code = $1")
            .bind(cls)
            .execute(pool)
            .await;
        let _ = sqlx::query("DELETE FROM sentinel_class_state WHERE class_code = $1")
            .bind(cls)
            .execute(pool)
            .await;
        let _ = sqlx::query("DELETE FROM accounts WHERE class_code = $1")
            .bind(cls)
            .execute(pool)
            .await;
    }
}

#[allow(clippy::too_many_arguments)]
async fn seed_student(
    state: &AppState,
    email: &str,
    session_token: &str,
    class_code: &str,
    fcm_token: &str,
    html: &str,
    now: DateTime<Utc>,
) -> (i64, String) {
    let cookies =
        NsacSessionCookies::new(format!("xsrf_{session_token}"), session_token, None).unwrap();
    let acct = state
        .accounts()
        .upsert_on_login(
            &state.crypto,
            email,
            "LabPass!123",
            &cookies,
            Some(class_code),
            None,
            Some(fcm_token),
            now,
        )
        .await
        .unwrap();

    let mut boletim = parse_boletim(html).unwrap();
    canonicalize_boletim(&mut boletim);
    let hash = compute_class_grade_hash(&boletim);
    state.boletim().upsert_boletim(acct.id, &boletim).await.unwrap();
    (acct.id, hash)
}

async fn mount_fcm_ok(fcm_server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/oauth2/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "lab-fcm-oauth-token",
            "expires_in": 3600
        })))
        .mount(fcm_server)
        .await;

    Mock::given(method("POST"))
        .and(path("/v1/projects/nsac-lab-proj/messages:send"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "name": "projects/nsac-lab-proj/messages/lab-ok"
        })))
        .mount(fcm_server)
        .await;
}

/// LAB SCENARIO 1:
/// Individual recovery (`NAC -> SAT` and `NAC -> INS`) shifts class `M.T.` (`7.0 -> 7.2`).
/// - Classmate A (personal grade 8.0, NAC unchanged) MUST receive 0 notifications.
/// - Student B (4.5 -> 6.0, NAC -> SAT) and Student C (4.0 -> 4.0, NAC -> INS) MUST receive their individual push.
/// - `grade_posting_events` MUST NOT record duplicate class events and class MUST NOT enter Hot Mode.
#[tokio::test]
async fn lab_scenario_1_individual_recovery_sat_ins_never_notifies_classmates() {
    let _guard = LAB_TEST_MUTEX.lock().await;
    let pool = common::test_pool().await;
    let suffix = common::unique_suffix();
    let class_code = format!("LAB1_{suffix}");
    freeze_other_classes(&pool, std::slice::from_ref(&class_code)).await;

    let nsac_server = MockServer::start().await;
    let fcm_server = MockServer::start().await;
    mount_fcm_ok(&fcm_server).await;

    let t0 = Utc.with_ymd_and_hms(2026, 11, 24, 14, 0, 0).unwrap();
    let clock = Arc::new(ManualClock::new(t0));
    let cfg = make_lab_config(
        nsac_server.uri(),
        fcm_server.uri(),
        3,
        production_sentinel_config(),
    );
    let sentinel_repo = nsac_server::db::SentinelRepo::new(pool.clone());
    let heuristic = SentinelHeuristic::arc(sentinel_repo.clone(), cfg.sentinel.clone());
    let fcm = FcmClient::new(cfg.fcm.clone()).unwrap();
    let state = AppState::new(cfg, pool.clone(), heuristic.clone(), clock.clone()).unwrap();

    // Baseline: 3 students with bimester 1 already posted (M.T. = 7.0)
    let html_a_base = render_boletim_with_recovery(
        &class_code, 2026, "Língua Portuguesa", "PORT", 8.0, 7.0, "-", "Não Aconteceu", None, None,
    );
    let html_b_base = render_boletim_with_recovery(
        &class_code, 2026, "Língua Portuguesa", "PORT", 4.5, 7.0, "-", "Não Aconteceu", None, None,
    );
    let html_c_base = render_boletim_with_recovery(
        &class_code, 2026, "Língua Portuguesa", "PORT", 4.0, 7.0, "-", "Não Aconteceu", None, None,
    );

    let tok_a = format!("fcm_lab1_a_{suffix}");
    let tok_b = format!("fcm_lab1_b_{suffix}");
    let tok_c = format!("fcm_lab1_c_{suffix}");
    let sess_a = format!("sess_lab1_a_{suffix}");
    let sess_b = format!("sess_lab1_b_{suffix}");
    let sess_c = format!("sess_lab1_c_{suffix}");

    let (id_a, base_hash) = seed_student(&state, &format!("a_{suffix}@unesp.br"), &sess_a, &class_code, &tok_a, &html_a_base, t0).await;
    let (_id_b, _) = seed_student(&state, &format!("b_{suffix}@unesp.br"), &sess_b, &class_code, &tok_b, &html_b_base, t0).await;
    let (_id_c, _) = seed_student(&state, &format!("c_{suffix}@unesp.br"), &sess_c, &class_code, &tok_c, &html_c_base, t0).await;

    heuristic.initialize_class_on_login(&class_code, &base_hash, id_a, t0).await.unwrap();
    let mut st = sentinel_repo.get_class_state(&class_code).await.unwrap().unwrap();
    st.next_check_at = t0;
    sentinel_repo.upsert_class_state(&st).await.unwrap();

    // Recovery happens: Student B -> SAT (6.0), Student C -> INS (4.0), M.T. shifts 7.0 -> 7.2
    // Classmate A stays 8.0 / NAC (only M.T. shifted 7.0 -> 7.2)
    let html_a_after = render_boletim_with_recovery(
        &class_code, 2026, "Língua Portuguesa", "PORT", 8.0, 7.2, "-", "Não Aconteceu", None, None,
    );
    let html_b_after = render_boletim_with_recovery(
        &class_code, 2026, "Língua Portuguesa", "PORT", 6.0, 7.2, "SAT", "Satisfatório", None, None,
    );
    let html_c_after = render_boletim_with_recovery(
        &class_code, 2026, "Língua Portuguesa", "PORT", 4.0, 7.2, "INS", "Insatisfatório", None, None,
    );

    for (sess, body) in [(&sess_a, html_a_after), (&sess_b, html_b_after), (&sess_c, html_c_after)] {
        Mock::given(method("GET"))
            .and(path("/nsac/aluno/boletim"))
            .and(header_regex("cookie", &format!("nsaconline_session={sess}")))
            .respond_with(ResponseTemplate::new(200).set_body_string(body))
            .mount(&nsac_server)
            .await;
    }

    let report = poll_due_classes_once(&state, &fcm).await.unwrap();
    assert_eq!(report.classes_checked, 1);
    assert_eq!(report.classes_changed, 1);
    assert_eq!(report.fanout_students_updated, 3);
    assert_eq!(report.fcm_delivered, 2, "Only Student B (SAT) and Student C (INS) must receive push");

    // Verify zero duplicate grade_posting_events and NO Hot Mode triggered
    let events = sentinel_repo.list_events_for_class(&class_code).await.unwrap();
    assert!(events.is_empty(), "M.T. shift from recovery must not create new class posting event");

    let st_after = sentinel_repo.get_class_state(&class_code).await.unwrap().unwrap();
    assert!(st_after.hot_until.is_none(), "Recovery-only M.T. shift must not trigger Hot Mode");

    // Verify exact FCM recipients: B and C yes, A never!
    let fcm_reqs: Vec<String> = fcm_server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|r| r.url.path().ends_with("/messages:send"))
        .map(|r| String::from_utf8_lossy(&r.body).to_string())
        .collect();
    assert_eq!(fcm_reqs.len(), 2);
    assert!(fcm_reqs.iter().any(|b| b.contains(&tok_b)), "Student B (SAT) must be notified");
    assert!(fcm_reqs.iter().any(|b| b.contains(&tok_c)), "Student C (INS) must be notified");
    assert!(!fcm_reqs.iter().any(|b| b.contains(&tok_a)), "Classmate A must NOT be notified");

    cleanup_classes(&pool, &[class_code]).await;
}

/// LAB SCENARIO 2:
/// Gradual teacher grade posting + Manual Boletim refresh waking 7-day Cold Mode -> 10-min Hot Mode.
/// - Wave 1 (manual refresh by Student A): A gets 8.5 (M.T. 7.8), B still unposted (`-`).
///   -> Records 1 event, enters 10-min Hot Mode (`600s`), notifies ONLY Student A.
/// - Wave 2 (+10 min, Sentinel polls): B gets 7.5 (M.T. shifts 7.8 -> 7.7), A unchanged (8.5).
///   -> Still ONLY 1 event in `grade_posting_events` (deduped!), notifies ONLY Student B, 0 spam for A!
#[tokio::test]
async fn lab_scenario_2_gradual_posting_manual_refresh_wakes_7d_cold_to_10m_hot_without_duplicates() {
    let _guard = LAB_TEST_MUTEX.lock().await;
    let pool = common::test_pool().await;
    let suffix = common::unique_suffix();
    let class_code = format!("LAB2_{suffix}");
    freeze_other_classes(&pool, std::slice::from_ref(&class_code)).await;

    let nsac_server = MockServer::start().await;
    let fcm_server = MockServer::start().await;
    mount_fcm_ok(&fcm_server).await;

    let t0 = Utc.with_ymd_and_hms(2026, 6, 25, 14, 0, 0).unwrap();
    let clock = Arc::new(ManualClock::new(t0));
    let cfg = make_lab_config(
        nsac_server.uri(),
        fcm_server.uri(),
        3,
        production_sentinel_config(),
    );
    let sentinel_repo = nsac_server::db::SentinelRepo::new(pool.clone());
    let heuristic = SentinelHeuristic::arc(sentinel_repo.clone(), cfg.sentinel.clone());
    let fcm = FcmClient::new(cfg.fcm.clone()).unwrap();
    let state = AppState::new(cfg, pool.clone(), heuristic.clone(), clock.clone()).unwrap();

    let html_base = render_boletim_with_recovery(
        &class_code, 2026, "Química", "QUIM", 7.0, 7.0, "-", "Não Aconteceu", None, None,
    );
    let tok_a = format!("fcm_lab2_a_{suffix}");
    let tok_b = format!("fcm_lab2_b_{suffix}");
    let sess_a = format!("sess_lab2_a_{suffix}");
    let sess_b = format!("sess_lab2_b_{suffix}");

    let (id_a, base_hash) = seed_student(&state, &format!("a2_{suffix}@unesp.br"), &sess_a, &class_code, &tok_a, &html_base, t0).await;
    let (_id_b, _) = seed_student(&state, &format!("b2_{suffix}@unesp.br"), &sess_b, &class_code, &tok_b, &html_base, t0).await;

    // Initialize class -> starts in 7-day Cold Mode (604_800s)
    heuristic.initialize_class_on_login(&class_code, &base_hash, id_a, t0).await.unwrap();
    let st_cold = sentinel_repo.get_class_state(&class_code).await.unwrap().unwrap();
    assert_eq!(st_cold.current_interval_secs, 604_800, "Fresh class must start in 7-day Cold Mode");

    // Wave 1: Student A gets bimester 2 grade (8.5, M.T. = 7.8), Student B still None
    let html_a_w1 = render_boletim_with_recovery(
        &class_code, 2026, "Química", "QUIM", 7.0, 7.0, "-", "Não Aconteceu", Some(8.5), Some(7.8),
    );
    let html_b_w1 = render_boletim_with_recovery(
        &class_code, 2026, "Química", "QUIM", 7.0, 7.0, "-", "Não Aconteceu", None, Some(7.8),
    );
    Mock::given(method("GET"))
        .and(path("/nsac/aluno/boletim"))
        .and(header_regex("cookie", &format!("nsaconline_session={sess_b}")))
        .respond_with(ResponseTemplate::new(200).set_body_string(html_b_w1))
        .mount(&nsac_server)
        .await;

    let prev_a = state.boletim().get_boletim(id_a).await.unwrap();
    let mut fresh_a = parse_boletim(&html_a_w1).unwrap();
    canonicalize_boletim(&mut fresh_a);
    let fresh_hash_w1 = compute_class_grade_hash(&fresh_a);
    state.boletim().upsert_boletim(id_a, &fresh_a).await.unwrap();

    // Student A manually refreshes Boletim -> wakes Sentinel from 7d Cold to 10m Hot!
    handle_manual_refresh_class_change(
        state.clone(),
        class_code.clone(),
        id_a,
        prev_a,
        fresh_a,
        fresh_hash_w1,
    )
    .await;

    let st_hot = sentinel_repo.get_class_state(&class_code).await.unwrap().unwrap();
    assert_eq!(st_hot.current_interval_secs, 600, "Manual refresh with new grade must activate 10-min Hot Mode");
    assert_eq!(st_hot.next_check_at, t0 + ChronoDuration::seconds(600));
    assert_eq!(sentinel_repo.list_events_for_class(&class_code).await.unwrap().len(), 1);

    let fcm_w1: Vec<String> = fcm_server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|r| r.url.path().ends_with("/messages:send"))
        .map(|r| String::from_utf8_lossy(&r.body).to_string())
        .collect();
    assert_eq!(fcm_w1.len(), 1, "Only Student A (who actually got a personal grade) must be notified");
    assert!(fcm_w1[0].contains(&tok_a));

    // Wave 2 (+10 minutes): Teacher finishes grading Student B (7.5), shifting M.T. 7.8 -> 7.7
    let t1 = t0 + ChronoDuration::seconds(600);
    clock.set(t1);
    nsac_server.reset().await;
    fcm_server.reset().await;
    mount_fcm_ok(&fcm_server).await;

    let html_a_w2 = render_boletim_with_recovery(
        &class_code, 2026, "Química", "QUIM", 7.0, 7.0, "-", "Não Aconteceu", Some(8.5), Some(7.7),
    );
    let html_b_w2 = render_boletim_with_recovery(
        &class_code, 2026, "Química", "QUIM", 7.0, 7.0, "-", "Não Aconteceu", Some(7.5), Some(7.7),
    );
    for (sess, body) in [(&sess_a, html_a_w2), (&sess_b, html_b_w2)] {
        Mock::given(method("GET"))
            .and(path("/nsac/aluno/boletim"))
            .and(header_regex("cookie", &format!("nsaconline_session={sess}")))
            .respond_with(ResponseTemplate::new(200).set_body_string(body))
            .mount(&nsac_server)
            .await;
    }

    let rep_w2 = poll_due_classes_once(&state, &fcm).await.unwrap();
    assert_eq!(rep_w2.classes_checked, 1);
    assert_eq!(rep_w2.classes_changed, 1);
    assert_eq!(rep_w2.fcm_delivered, 1, "Only Student B must receive push in Wave 2; Student A must NOT be re-announced");
    assert_eq!(
        sentinel_repo.list_events_for_class(&class_code).await.unwrap().len(),
        1,
        "grade_posting_events must stay at 1 (no duplicate)"
    );

    let fcm_w2: Vec<String> = fcm_server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|r| r.url.path().ends_with("/messages:send"))
        .map(|r| String::from_utf8_lossy(&r.body).to_string())
        .collect();
    assert_eq!(fcm_w2.len(), 1);
    assert!(fcm_w2[0].contains(&tok_b), "Student B must receive their newly posted 7.5 grade");
    assert!(!fcm_w2[0].contains(&tok_a), "Student A must NOT be re-announced");

    cleanup_classes(&pool, &[class_code]).await;
}

/// LAB SCENARIO 3:
/// Multi-class burst under `NsacSemaphore(3)` + `400 INVALID_ARGUMENT` FCM token auto-cleanup +
/// Full decay lifecycle: 10-min Hot Mode (600s) -> Historical (<= 7200s) -> 7-day Cold (604_800s).
#[tokio::test]
async fn lab_scenario_3_semaphore_c3_invalid_fcm_400_cleanup_and_10m_to_7d_decay() {
    let _guard = LAB_TEST_MUTEX.lock().await;
    let pool = common::test_pool().await;
    let suffix = common::unique_suffix();
    let cls_1 = format!("LAB3A_{suffix}");
    let cls_2 = format!("LAB3B_{suffix}");
    let all_classes = vec![cls_1.clone(), cls_2.clone()];
    freeze_other_classes(&pool, &all_classes).await;

    let nsac_server = MockServer::start().await;
    let fcm_server = MockServer::start().await;

    // OAuth OK; broken token returns 400 INVALID_ARGUMENT, valid token returns 200 OK
    Mock::given(method("POST"))
        .and(path("/oauth2/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "lab3-oauth",
            "expires_in": 3600
        })))
        .mount(&fcm_server)
        .await;

    let bad_tok = format!("fcm_broken_400_{suffix}");
    let ok_tok = format!("fcm_valid_200_{suffix}");

    Mock::given(method("POST"))
        .and(path("/v1/projects/nsac-lab-proj/messages:send"))
        .and(body_string_contains(bad_tok.as_str()))
        .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
            "error": {
                "code": 400,
                "message": "The registration token is not a valid FCM registration token",
                "status": "INVALID_ARGUMENT"
            }
        })))
        .mount(&fcm_server)
        .await;

    Mock::given(method("POST"))
        .and(path("/v1/projects/nsac-lab-proj/messages:send"))
        .and(body_string_contains(ok_tok.as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "name": "projects/nsac-lab-proj/messages/lab3-ok"
        })))
        .mount(&fcm_server)
        .await;

    let t0 = Utc.with_ymd_and_hms(2026, 5, 1, 14, 0, 0).unwrap();
    let clock = Arc::new(ManualClock::new(t0));
    let cfg = make_lab_config(
        nsac_server.uri(),
        fcm_server.uri(),
        3, // NsacSemaphore(3)
        production_sentinel_config(),
    );
    let sentinel_repo = nsac_server::db::SentinelRepo::new(pool.clone());
    let heuristic = SentinelHeuristic::arc(sentinel_repo.clone(), cfg.sentinel.clone());
    let fcm = FcmClient::new(cfg.fcm.clone()).unwrap();
    let state = AppState::new(cfg, pool.clone(), heuristic.clone(), clock.clone()).unwrap();

    // Class 1 has 2 students (one with valid token, one with broken 400 token); Class 2 is unchanged
    let html_c1_base = render_boletim_with_recovery(
        &cls_1, 2026, "Física", "FIS", 7.0, 7.0, "-", "Não Aconteceu", None, None,
    );
    let html_c1_new = render_boletim_with_recovery(
        &cls_1, 2026, "Física", "FIS", 7.0, 7.0, "-", "Não Aconteceu", Some(9.0), Some(8.2),
    );
    let html_c2_base = render_boletim_with_recovery(
        &cls_2, 2026, "História", "HIS", 7.5, 7.5, "-", "Não Aconteceu", None, None,
    );

    let sess_ok = format!("sess_lab3_ok_{suffix}");
    let sess_bad = format!("sess_lab3_bad_{suffix}");
    let sess_c2 = format!("sess_lab3_c2_{suffix}");

    let (id_ok, h1) = seed_student(&state, &format!("ok3_{suffix}@unesp.br"), &sess_ok, &cls_1, &ok_tok, &html_c1_base, t0).await;
    let (id_bad, _) = seed_student(&state, &format!("bad3_{suffix}@unesp.br"), &sess_bad, &cls_1, &bad_tok, &html_c1_base, t0).await;
    let (id_c2, h2) = seed_student(&state, &format!("c2_{suffix}@unesp.br"), &sess_c2, &cls_2, &ok_tok, &html_c2_base, t0).await;

    heuristic.initialize_class_on_login(&cls_1, &h1, id_ok, t0).await.unwrap();
    heuristic.initialize_class_on_login(&cls_2, &h2, id_c2, t0).await.unwrap();
    for cls in &all_classes {
        let mut st = sentinel_repo.get_class_state(cls).await.unwrap().unwrap();
        st.next_check_at = t0;
        sentinel_repo.upsert_class_state(&st).await.unwrap();
    }

    for (sess, body) in [(&sess_ok, html_c1_new.clone()), (&sess_bad, html_c1_new), (&sess_c2, html_c2_base)] {
        Mock::given(method("GET"))
            .and(path("/nsac/aluno/boletim"))
            .and(header_regex("cookie", &format!("nsaconline_session={sess}")))
            .respond_with(ResponseTemplate::new(200).set_body_string(body))
            .mount(&nsac_server)
            .await;
    }

    state.semaphore.reset_metrics();
    let rep = poll_due_classes_once(&state, &fcm).await.unwrap();
    assert!(state.semaphore.peak_count() <= 3, "NsacSemaphore(3) must never be exceeded");
    assert_eq!(rep.classes_checked, 2);
    assert_eq!(rep.classes_changed, 1);
    assert_eq!(rep.fcm_delivered, 1);
    assert_eq!(rep.fcm_unregistered_cleared, 1, "400 INVALID_ARGUMENT token must be auto-cleared");

    // Verify broken token was actually cleared in PostgreSQL
    let bad_tok_in_db: Option<String> =
        sqlx::query_scalar("SELECT fcm_token FROM accounts WHERE id = $1")
            .bind(id_bad)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(bad_tok_in_db.is_none(), "Broken FCM token must be set to NULL in DB");

    // Verify Hot Mode = 600s (10 min)
    let st_hot = sentinel_repo.get_class_state(&cls_1).await.unwrap().unwrap();
    assert_eq!(st_hot.current_interval_secs, 600);

    // Advance +2h10m (Hot Mode expired, still inside historical window) -> decays to <= 7200s (> 600s)
    let t_hist = t0 + ChronoDuration::seconds(7_800);
    clock.set(t_hist);
    let mut st_due = sentinel_repo.get_class_state(&cls_1).await.unwrap().unwrap();
    st_due.next_check_at = t_hist;
    sentinel_repo.upsert_class_state(&st_due).await.unwrap();
    let rep_hist = poll_due_classes_once(&state, &fcm).await.unwrap();
    assert_eq!(rep_hist.classes_changed, 0);
    let st_hist = sentinel_repo.get_class_state(&cls_1).await.unwrap().unwrap();
    assert!(st_hist.hot_until.is_none());
    assert!(st_hist.current_interval_secs > 600 && st_hist.current_interval_secs <= 7_200);

    // Advance +35 days (completely outside historical window) -> decays to 7-day Cold Mode (604_800s)
    let t_cold = t0 + ChronoDuration::days(35);
    clock.set(t_cold);
    let mut st_due2 = sentinel_repo.get_class_state(&cls_1).await.unwrap().unwrap();
    st_due2.next_check_at = t_cold;
    sentinel_repo.upsert_class_state(&st_due2).await.unwrap();
    let rep_cold = poll_due_classes_once(&state, &fcm).await.unwrap();
    assert_eq!(rep_cold.classes_changed, 0);
    let st_cold = sentinel_repo.get_class_state(&cls_1).await.unwrap().unwrap();
    assert_eq!(st_cold.current_certainty, 0.0);
    assert_eq!(st_cold.current_interval_secs, 604_800, "Must decay all the way to 7-day Cold Mode");

    cleanup_classes(&pool, &all_classes).await;
}
