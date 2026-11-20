mod common;

use std::sync::{Arc, RwLock};

use chrono::{DateTime, Duration as ChronoDuration, NaiveDate, TimeZone, Utc};
use nsac_core::models::NsacSessionCookies;
use nsac_core::parsers::parse_boletim;
use nsac_core::sentinel::compute_class_grade_hash;
use nsac_server::config::{CacheConfig, Config, FcmConfig, SentinelConfig};
use nsac_server::db::{NewGradePostingEvent, SentinelRepo};
use nsac_server::fcm::FcmClient;
use nsac_server::ports::{Clock, HeuristicObserver};
use nsac_server::sentinel::{
    DAYS_IN_YEAR, SentinelHeuristic, circular_day_distance, compute_historical_certainty,
    compute_interval_from_certainty, poll_due_classes_once,
};
use nsac_server::state::AppState;
use sqlx::PgPool;
use tokio::sync::Mutex;
use wiremock::matchers::{body_string_contains, header_regex, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};
use zeroize::Zeroizing;

const TEST_ENC_KEY: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

static POLL_TEST_MUTEX: Mutex<()> = Mutex::const_new(());

async fn freeze_other_sentinel_classes(pool: &PgPool, active_classes: &[String]) {
    let far_future = Utc.with_ymd_and_hms(2099, 1, 1, 0, 0, 0).unwrap();
    let _ = sqlx::query(
        "UPDATE sentinel_class_state SET next_check_at = $1 WHERE NOT (class_code = ANY($2))",
    )
    .bind(far_future)
    .bind(active_classes)
    .execute(pool)
    .await;
}

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
            .unwrap_or_else(|_| Utc.with_ymd_and_hms(2026, 4, 30, 12, 0, 0).unwrap())
    }
}

fn default_sentinel_config() -> SentinelConfig {
    SentinelConfig {
        enabled: true,
        tick_ms: 10_000,
        cold_baseline_secs: 86_400,
        historical_radius_days: 4,
        historical_interval_secs: 7_200,
        hot_min_interval_secs: 120,
        hot_max_interval_secs: 180,
        hot_duration_secs: 7_200,
        api_volume_weight: 0.15,
        api_cooldown_secs: 1_800,
        midnight_backoff_secs: 1_800,
        jitter_max_ms: 0,
    }
}

fn make_sim_config(
    nsac_base_url: String,
    fcm_server_uri: Option<String>,
    max_concurrent: usize,
    timeout_secs: u64,
    sentinel: SentinelConfig,
) -> Config {
    let fcm = match fcm_server_uri {
        Some(uri) => FcmConfig {
            enabled: true,
            project_id: Some("nsac-sim-proj".to_string()),
            credentials_json: None,
            base_url: Some(uri.clone()),
            endpoint_url: Some(format!("{uri}/v1/projects/nsac-sim-proj/messages:send")),
            oauth_token_url: Some(format!("{uri}/oauth2/token")),
        },
        None => FcmConfig {
            enabled: false,
            project_id: None,
            credentials_json: None,
            base_url: None,
            endpoint_url: None,
            oauth_token_url: None,
        },
    };

    Config {
        http_bind: "127.0.0.1:0".parse().expect("valid SocketAddr"),
        rust_log: "warn".to_string(),
        database_url: Zeroizing::new("postgres://postgres@127.0.0.1:5432/nsac_m1_test".to_string()),
        encryption_key: Zeroizing::new(TEST_ENC_KEY.to_string()),
        service_api_keys: Vec::new(),
        user_session_ttl_secs: 3600,
        nsac_base_url,
        nsac_timeout_secs: timeout_secs,
        nsac_max_concurrent_requests: max_concurrent,
        sentinel,
        fcm,
        cache: CacheConfig {
            boletim_ttl_secs: 300,
            snapshot_ttl_secs: 300,
            auth_ttl_secs: 0,
        },
    }
}

#[allow(clippy::too_many_arguments)]
fn render_boletim_html(
    class_code: &str,
    year: u16,
    subject_title: &str,
    subject_abbrev: &str,
    mb_b1: f64,
    mt_b1: f64,
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
              <td class="text-center bd"><span data-toggle="tooltip" title="Não Aconteceu">-</span></td>
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

async fn mount_fcm_mock(fcm_server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/oauth2/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "sim-fcm-oauth-token",
            "expires_in": 3600
        })))
        .mount(fcm_server)
        .await;

    Mock::given(method("POST"))
        .and(path("/v1/projects/nsac-sim-proj/messages:send"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "name": "projects/nsac-sim-proj/messages/msg-sim-001"
        })))
        .mount(fcm_server)
        .await;
}

#[allow(clippy::too_many_arguments)]
async fn seed_student_with_baseline_boletim(
    state: &AppState,
    email: &str,
    password: &str,
    session_token: &str,
    class_code: &str,
    fcm_token: Option<&str>,
    baseline_html: &str,
    now: DateTime<Utc>,
) -> (i64, String) {
    let cookies =
        NsacSessionCookies::new(format!("xsrf_{session_token}"), session_token, None).unwrap();
    let acct = state
        .accounts()
        .upsert_on_login(
            &state.crypto,
            email,
            password,
            &cookies,
            Some(class_code),
            None,
            fcm_token,
            now,
        )
        .await
        .expect("upsert student account");

    let mut parsed = parse_boletim(baseline_html).expect("parse baseline boletim");
    nsac_server::http::nsac_routes::canonicalize_boletim(&mut parsed);
    let hash = compute_class_grade_hash(&parsed);

    state
        .boletim()
        .upsert_boletim(acct.id, &parsed)
        .await
        .expect("upsert baseline boletim");

    (acct.id, hash)
}

async fn cleanup_classes(pool: &PgPool, class_codes: &[String]) {
    for code in class_codes {
        let _ = sqlx::query("DELETE FROM grade_posting_events WHERE class_code = $1")
            .bind(code)
            .execute(pool)
            .await;
        let _ = sqlx::query("DELETE FROM sentinel_class_state WHERE class_code = $1")
            .bind(code)
            .execute(pool)
            .await;
        let _ = sqlx::query("DELETE FROM accounts WHERE class_code = $1")
            .bind(code)
            .execute(pool)
            .await;
    }
}

fn utc_date_noon(year: i32, month: u32, day: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(year, month, day, 12, 0, 0).unwrap()
}

fn utc_from_doy(year: i32, doy: u32) -> DateTime<Utc> {
    let date = NaiveDate::from_yo_opt(year, doy).expect("valid year and day-of-year");
    Utc.from_utc_datetime(&date.and_hms_opt(12, 0, 0).unwrap())
}

// ============================================================================
// CENÁRIO 1: Simulação Multi-Ano e Multi-Bimestre com Dados Forjados
// (Cold Start -> Aprendizado 3 Anos -> Raio ±4 Dias -> Virada de Ano/Bissexto -> Modo Quente + Decaimento)
// ============================================================================
#[tokio::test]
async fn scenario_1_multi_year_bimester_learning_radius_new_year_and_hot_decay() {
    let pool = common::test_pool().await;
    let suffix = common::unique_suffix();
    let class_code = format!("SIM1_{suffix}");
    let sentinel_repo = SentinelRepo::new(pool.clone());
    let sentinel_cfg = default_sentinel_config();

    assert_eq!(DAYS_IN_YEAR, 365);

    // 1.1 Cold Start (0 histórico): certainty == 0.0, interval == 86400s (24h)
    let start_2026 = utc_from_doy(2026, 110);
    let empty_events = sentinel_repo
        .list_events_for_class(&class_code)
        .await
        .unwrap();
    assert!(empty_events.is_empty());

    let cold_certainty = compute_historical_certainty(
        &empty_events,
        start_2026,
        sentinel_cfg.historical_radius_days,
    );
    assert_eq!(cold_certainty, 0.0);
    let cold_interval = compute_interval_from_certainty(cold_certainty, false, &sentinel_cfg);
    assert_eq!(
        cold_interval,
        i64::try_from(sentinel_cfg.cold_baseline_secs).unwrap()
    );

    // 1.2 Forjar histórico de 3 anos letivos (2024 bissexto, 2025, 2026) nos 4 bimestres:
    // - 1º Bimestre: cluster em torno de day_of_year = 120 (~30 de Abril)
    // - 2º Bimestre: cluster em torno de day_of_year = 185 (~4 de Julho)
    // - 3º Bimestre: cluster em torno de day_of_year = 275 (~2 de Outubro)
    // - 4º Bimestre: cluster na virada de ano (day_of_year = 365 e day_of_year = 1)
    let forged_history = vec![
        // 2024 (ano bissexto) e 2025 para 1º Bimestre (DOY 120)
        NewGradePostingEvent {
            class_code: class_code.clone(),
            school_year: 2024,
            bimester: 1,
            subject_name: "Algoritmos".to_string(),
            day_of_year: 120,
            month: 4,
            day: 29,
            posted_at: utc_from_doy(2024, 120),
        },
        NewGradePostingEvent {
            class_code: class_code.clone(),
            school_year: 2024,
            bimester: 1,
            subject_name: "Física".to_string(),
            day_of_year: 120,
            month: 4,
            day: 29,
            posted_at: utc_from_doy(2024, 120),
        },
        NewGradePostingEvent {
            class_code: class_code.clone(),
            school_year: 2025,
            bimester: 1,
            subject_name: "Algoritmos".to_string(),
            day_of_year: 120,
            month: 4,
            day: 30,
            posted_at: utc_from_doy(2025, 120),
        },
        // 2º Bimestre (DOY 185)
        NewGradePostingEvent {
            class_code: class_code.clone(),
            school_year: 2024,
            bimester: 2,
            subject_name: "Banco de Dados".to_string(),
            day_of_year: 185,
            month: 7,
            day: 3,
            posted_at: utc_from_doy(2024, 185),
        },
        NewGradePostingEvent {
            class_code: class_code.clone(),
            school_year: 2025,
            bimester: 2,
            subject_name: "Banco de Dados".to_string(),
            day_of_year: 185,
            month: 7,
            day: 4,
            posted_at: utc_from_doy(2025, 185),
        },
        // 3º Bimestre (DOY 275)
        NewGradePostingEvent {
            class_code: class_code.clone(),
            school_year: 2025,
            bimester: 3,
            subject_name: "Redes".to_string(),
            day_of_year: 275,
            month: 10,
            day: 2,
            posted_at: utc_from_doy(2025, 275),
        },
    ];
    sentinel_repo
        .insert_posting_events_batch(&forged_history)
        .await
        .unwrap();

    let events = sentinel_repo
        .list_events_for_class(&class_code)
        .await
        .unwrap();
    assert_eq!(events.len(), 6);

    // 1.3 Varrer linha do tempo em torno de DOY = 120 (1º Bimestre de 2026)
    for far_offset in [-10_i32, -5, 5, 10] {
        let doy = u32::try_from(120 + far_offset).unwrap();
        let t = utc_from_doy(2026, doy);
        let cert = compute_historical_certainty(&events, t, sentinel_cfg.historical_radius_days);
        let interval = compute_interval_from_certainty(cert, false, &sentinel_cfg);
        assert_eq!(
            cert, 0.0,
            "offset {far_offset} outside ±4 days must yield 0.0 certainty"
        );
        assert_eq!(
            interval, 86_400,
            "offset {far_offset} outside ±4 days must stay at 24h baseline"
        );
    }

    // Entrando no raio ±4 dias: d = 4, 3, 2, 1, 0, -1, -2, -3, -4
    let mut certainties_by_offset = Vec::new();
    for offset in [-4_i32, -3, -2, -1, 0, 1, 2, 3, 4] {
        let doy = u32::try_from(120 + offset).unwrap();
        let t = utc_from_doy(2026, doy);
        let cert = compute_historical_certainty(&events, t, sentinel_cfg.historical_radius_days);
        let interval = compute_interval_from_certainty(cert, false, &sentinel_cfg);
        assert!(
            cert > 0.30,
            "within ±4 days (offset={offset}) certainty ({cert}) must be > 0.30"
        );
        assert!(
            interval <= i64::try_from(sentinel_cfg.historical_interval_secs).unwrap(),
            "within ±4 days (offset={offset}) interval ({interval}s) must drop to <= historical_interval_secs (7200s)"
        );
        certainties_by_offset.push((offset, cert, interval));
    }

    // Comprovar crescimento monotônico à medida que se aproxima do pico (d = 4 -> 3 -> 2 -> 1 -> 0)
    let cert_d4 = certainties_by_offset.iter().find(|x| x.0 == -4).unwrap().1;
    let cert_d3 = certainties_by_offset.iter().find(|x| x.0 == -3).unwrap().1;
    let cert_d2 = certainties_by_offset.iter().find(|x| x.0 == -2).unwrap().1;
    let cert_d1 = certainties_by_offset.iter().find(|x| x.0 == -1).unwrap().1;
    let cert_d0 = certainties_by_offset.iter().find(|x| x.0 == 0).unwrap().1;
    assert!(cert_d4 < cert_d3 && cert_d3 < cert_d2 && cert_d2 < cert_d1 && cert_d1 < cert_d0);
    assert!((cert_d0 - 0.85).abs() < 1e-6, "3 events at d=0 should hit peak historical certainty 0.85");

    // 1.4 Fronteira de Virada de Ano e Ano Bissexto (day_of_year 364/365/366 vs day_of_year 1/2)
    assert_eq!(circular_day_distance(365, 1), 1);
    assert_eq!(circular_day_distance(364, 2), 3);
    assert_eq!(circular_day_distance(1, 365), 1);
    assert_eq!(circular_day_distance(366, 365), 1); // Bissexto (366 normaliza sem pânico)

    // Inserir postagens de recuperação final na virada do ano (31/Dez = DOY 365 e 02/Jan = DOY 2)
    sentinel_repo
        .insert_posting_events_batch(&[
            NewGradePostingEvent {
                class_code: class_code.clone(),
                school_year: 2024,
                bimester: 4,
                subject_name: "Matemática".to_string(),
                day_of_year: 365,
                month: 12,
                day: 30,
                posted_at: utc_from_doy(2024, 365),
            },
            NewGradePostingEvent {
                class_code: class_code.clone(),
                school_year: 2025,
                bimester: 4,
                subject_name: "Matemática".to_string(),
                day_of_year: 2,
                month: 1,
                day: 2,
                posted_at: utc_from_doy(2025, 2),
            },
        ])
        .await
        .unwrap();

    let events_with_ny = sentinel_repo
        .list_events_for_class(&class_code)
        .await
        .unwrap();

    for ny_doy in [364_u32, 365, 1, 2] {
        let t_ny = utc_from_doy(2026, ny_doy);
        let cert_ny = compute_historical_certainty(
            &events_with_ny,
            t_ny,
            sentinel_cfg.historical_radius_days,
        );
        let int_ny = compute_interval_from_certainty(cert_ny, false, &sentinel_cfg);
        assert!(
            cert_ny > 0.30,
            "New Year boundary DOY={ny_doy} must activate predictive window (got cert={cert_ny})"
        );
        assert!(
            int_ny <= 7_200,
            "New Year boundary DOY={ny_doy} must use historical interval <= 7200s (got {int_ny}s)"
        );
    }

    // 1.5 Modo Quente (Burst Mode) ao detectar mudança real de nota + Decaimento após hot_duration_secs
    let _poll_guard = POLL_TEST_MUTEX.lock().await;
    freeze_other_sentinel_classes(&pool, std::slice::from_ref(&class_code)).await;

    let nsac_server = MockServer::start().await;
    let fcm_server = MockServer::start().await;
    mount_fcm_mock(&fcm_server).await;

    let clock = ManualClock::new(utc_from_doy(2026, 120)); // Dia de pico histórico (DOY 120)
    let cfg = make_sim_config(
        format!("{}/nsac", nsac_server.uri()),
        Some(fcm_server.uri()),
        4,
        5,
        sentinel_cfg.clone(),
    );
    let heuristic = SentinelHeuristic::arc(sentinel_repo.clone(), sentinel_cfg.clone());
    let state = AppState::new(cfg, pool.clone(), heuristic.clone(), Arc::new(clock.clone()))
        .expect("build AppState");
    let fcm = FcmClient::new(state.config.fcm.clone()).expect("build FcmClient");

    let initial_html = render_boletim_html(
        &class_code,
        2026,
        "Programação Orientada a Objetos",
        "POO",
        8.0,
        7.0,
        None,
        None,
    );
    let updated_html = render_boletim_html(
        &class_code,
        2026,
        "Programação Orientada a Objetos",
        "POO",
        8.0,
        7.0,
        Some(9.2),
        Some(8.1),
    );

    let (student_id, initial_hash) = seed_student_with_baseline_boletim(
        &state,
        &format!("sim1_aluno_{suffix}@unesp.br"),
        "PassSim1!",
        &format!("sess_sim1_{suffix}"),
        &class_code,
        Some(&format!("fcm_sim1_{suffix}")),
        &initial_html,
        clock.now(),
    )
    .await;

    heuristic
        .initialize_class_on_login(&class_code, &initial_hash, student_id, clock.now())
        .await
        .unwrap();

    // Forçar next_check_at = now para executar o ciclo de polling
    let mut st = sentinel_repo
        .get_class_state(&class_code)
        .await
        .unwrap()
        .unwrap();
    st.next_check_at = clock.now();
    sentinel_repo.upsert_class_state(&st).await.unwrap();

    // NSAC publica nota do 2º bimestre -> Modo Quente (certainty = 1.0, interval = 120s)
    Mock::given(method("GET"))
        .and(path("/nsac/aluno/boletim"))
        .respond_with(ResponseTemplate::new(200).set_body_string(updated_html.clone()))
        .mount(&nsac_server)
        .await;

    let report_hot = poll_due_classes_once(&state, &fcm).await.unwrap();
    assert_eq!(report_hot.classes_checked, 1);
    assert_eq!(report_hot.classes_changed, 1);
    assert_eq!(report_hot.fcm_delivered, 1);

    let hot_state = sentinel_repo
        .get_class_state(&class_code)
        .await
        .unwrap()
        .unwrap();
    assert!((hot_state.current_certainty - 1.0).abs() < 1e-9);
    assert_eq!(
        hot_state.current_interval_secs,
        i64::try_from(sentinel_cfg.hot_min_interval_secs).unwrap()
    );
    assert_eq!(
        hot_state.hot_until,
        Some(clock.now() + ChronoDuration::seconds(7_200))
    );

    // Avançar relógio além de hot_duration_secs (ex: +3 horas, ainda em DOY 120) sem novas notas:
    // Deve decair do Modo Quente (120s) de volta para o intervalo histórico (<= 7200s, > 120s)
    let after_hot_in_window = clock.now() + ChronoDuration::seconds(7_200 + 60);
    clock.set(after_hot_in_window);

    let mut st_due = sentinel_repo
        .get_class_state(&class_code)
        .await
        .unwrap()
        .unwrap();
    st_due.next_check_at = after_hot_in_window;
    sentinel_repo.upsert_class_state(&st_due).await.unwrap();

    let report_decay_hist = poll_due_classes_once(&state, &fcm).await.unwrap();
    assert_eq!(report_decay_hist.classes_checked, 1);
    assert_eq!(report_decay_hist.classes_changed, 0);

    let decayed_hist_state = sentinel_repo
        .get_class_state(&class_code)
        .await
        .unwrap()
        .unwrap();
    assert!(decayed_hist_state.hot_until.is_none());
    assert!(decayed_hist_state.current_certainty >= 0.30 && decayed_hist_state.current_certainty <= 0.85);
    assert!(
        decayed_hist_state.current_interval_secs > 120
            && decayed_hist_state.current_interval_secs <= 7_200
    );

    // Avançar relógio para fora de qualquer janela histórica (DOY 150, +30 dias sem nota):
    // Deve decair completamente para baseline frio (certainty == 0.0, interval == 86400s)
    let out_of_window_time = utc_from_doy(2026, 150);
    clock.set(out_of_window_time);
    let mut st_cold_due = sentinel_repo
        .get_class_state(&class_code)
        .await
        .unwrap()
        .unwrap();
    st_cold_due.next_check_at = out_of_window_time;
    sentinel_repo
        .upsert_class_state(&st_cold_due)
        .await
        .unwrap();

    let report_decay_cold = poll_due_classes_once(&state, &fcm).await.unwrap();
    assert_eq!(report_decay_cold.classes_checked, 1);
    assert_eq!(report_decay_cold.classes_changed, 0);

    let decayed_cold_state = sentinel_repo
        .get_class_state(&class_code)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(decayed_cold_state.current_certainty, 0.0);
    assert_eq!(decayed_cold_state.current_interval_secs, 86_400);

    cleanup_classes(&pool, &[class_code]).await;
}

// ============================================================================
// CENÁRIO 2: Ataque de Flood / DDoS de Chamadas na API
// (Sinal Secundário Blindado por Cooldown de 30 min)
// ============================================================================
#[tokio::test]
async fn scenario_2_api_flood_ddos_shielded_by_cooldown_never_triggers_hot_mode() {
    let pool = common::test_pool().await;
    let suffix = common::unique_suffix();
    let class_code = format!("INI3A_DDOS_{suffix}");
    let sentinel_repo = SentinelRepo::new(pool.clone());
    let sentinel_cfg = default_sentinel_config();
    let heuristic = SentinelHeuristic::new(sentinel_repo.clone(), sentinel_cfg.clone());

    let t0 = utc_date_noon(2026, 5, 20);
    heuristic
        .initialize_class_on_login(&class_code, "a".repeat(64).as_str(), 1, t0)
        .await
        .unwrap();

    let initial_state = sentinel_repo
        .get_class_state(&class_code)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(initial_state.current_certainty, 0.0);
    assert_eq!(initial_state.current_interval_secs, 86_400);
    assert!(initial_state.last_api_bump_at.is_none());

    // Simular ataque de 500 chamadas consecutivas `record_api_activity` dentro da mesma janela de 30 min (1800s)
    for i in 0..500_i64 {
        // Distribuir chamadas entre t0 e t0 + 1799s (estritamente dentro do cooldown de 1800s)
        let call_time = t0 + ChronoDuration::seconds((i * 3) % 1_799);
        let hint = heuristic
            .record_api_activity(&class_code, call_time)
            .await
            .expect("record_api_activity under flood");
        assert!(
            hint > sentinel_cfg.historical_interval_secs,
            "hint ({hint}s) at iteration {i} must stay strictly greater than historical_interval_secs (7200s)"
        );
    }

    let after_flood = sentinel_repo
        .get_class_state(&class_code)
        .await
        .unwrap()
        .unwrap();

    // Comprovar que apenas 1 único bump (+0.15) foi aceito
    assert!(
        (after_flood.current_certainty - 0.15).abs() < 1e-9,
        "Expected exactly 1 bump of +0.15, got {}",
        after_flood.current_certainty
    );
    assert!(after_flood.current_certainty <= 0.16);
    assert_eq!(
        after_flood.last_api_bump_at,
        Some(t0),
        "last_api_bump_at must remain anchored at the first accepted bump timestamp t0"
    );
    assert!(
        after_flood.current_interval_secs
            > i64::try_from(sentinel_cfg.historical_interval_secs).unwrap(),
        "current_interval_secs ({}) must remain strictly > historical_interval_secs (7200s)",
        after_flood.current_interval_secs
    );
    assert!(
        after_flood.hot_until.is_none(),
        "API flood must NEVER activate Hot Mode"
    );

    cleanup_classes(&pool, &[class_code]).await;
}

// ============================================================================
// CENÁRIO 3: Ataque / Falhas e Flapping do NSAC
// (Madrugada, 503, Timeouts, HTML Corrompido/Truncado + Backoff Exponencial + Recuperação)
// ============================================================================
#[tokio::test]
async fn scenario_3_nsac_flapping_503_timeout_corrupted_html_backoff_and_recovery() {
    let _poll_guard = POLL_TEST_MUTEX.lock().await;
    let pool = common::test_pool().await;
    let suffix = common::unique_suffix();
    let class_code = format!("INF1_FLAP_{suffix}");
    freeze_other_sentinel_classes(&pool, std::slice::from_ref(&class_code)).await;
    let sentinel_repo = SentinelRepo::new(pool.clone());
    let sentinel_cfg = default_sentinel_config();

    let nsac_server = MockServer::start().await;
    let fcm_server = MockServer::start().await;
    mount_fcm_mock(&fcm_server).await;

    let t0 = utc_date_noon(2026, 6, 15);
    let clock = ManualClock::new(t0);
    // Timeout curto (1s) para testar falha de timeout rapidamente sem atrasar a suíte
    let cfg = make_sim_config(
        format!("{}/nsac", nsac_server.uri()),
        Some(fcm_server.uri()),
        4,
        1,
        sentinel_cfg.clone(),
    );
    let heuristic = SentinelHeuristic::arc(sentinel_repo.clone(), sentinel_cfg.clone());
    let state = AppState::new(cfg, pool.clone(), heuristic.clone(), Arc::new(clock.clone()))
        .expect("build AppState");
    let fcm = FcmClient::new(state.config.fcm.clone()).expect("build FcmClient");

    let baseline_html = render_boletim_html(
        &class_code,
        2026,
        "Sistemas Operacionais",
        "SO",
        7.5,
        6.8,
        None,
        None,
    );
    let (student_id, baseline_hash) = seed_student_with_baseline_boletim(
        &state,
        &format!("flap_{suffix}@unesp.br"),
        "FlapPass1!",
        &format!("sess_flap_{suffix}"),
        &class_code,
        Some(&format!("fcm_flap_{suffix}")),
        &baseline_html,
        t0,
    )
    .await;

    heuristic
        .initialize_class_on_login(&class_code, &baseline_hash, student_id, t0)
        .await
        .unwrap();

    // Ciclo 1: NSAC retorna HTTP 503 Service Unavailable (manutenção da madrugada)
    let mut st = sentinel_repo
        .get_class_state(&class_code)
        .await
        .unwrap()
        .unwrap();
    st.next_check_at = t0;
    sentinel_repo.upsert_class_state(&st).await.unwrap();

    nsac_server.reset().await;
    Mock::given(method("GET"))
        .and(path("/nsac/aluno/boletim"))
        .respond_with(ResponseTemplate::new(503).set_body_string("Service Unavailable"))
        .mount(&nsac_server)
        .await;

    let rep1 = poll_due_classes_once(&state, &fcm).await.unwrap();
    assert_eq!(rep1.classes_errored, 1);
    assert_eq!(rep1.classes_changed, 0);
    assert_eq!(rep1.fcm_delivered, 0);

    let st1 = sentinel_repo
        .get_class_state(&class_code)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(st1.consecutive_errors, 1);
    // Backoff n=1: midnight_backoff_secs * 2^0 = 1800s
    assert_eq!(st1.next_check_at, t0 + ChronoDuration::seconds(1_800));
    assert_eq!(st1.last_class_hash.as_deref(), Some(baseline_hash.as_str()));

    // Ciclo 2: Avançar para t1 = t0 + 1800s; NSAC sofre Timeout (> 1s)
    let t1 = t0 + ChronoDuration::seconds(1_800);
    clock.set(t1);
    nsac_server.reset().await;
    Mock::given(method("GET"))
        .and(path("/nsac/aluno/boletim"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(std::time::Duration::from_millis(1_500))
                .set_body_string(&baseline_html),
        )
        .mount(&nsac_server)
        .await;

    let rep2 = poll_due_classes_once(&state, &fcm).await.unwrap();
    assert_eq!(rep2.classes_errored, 1);
    assert_eq!(rep2.classes_changed, 0);
    assert_eq!(rep2.fcm_delivered, 0);

    let st2 = sentinel_repo
        .get_class_state(&class_code)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(st2.consecutive_errors, 2);
    // Backoff n=2: midnight_backoff_secs * 2^1 = 3600s
    assert_eq!(st2.next_check_at, t1 + ChronoDuration::seconds(3_600));
    assert_eq!(st2.last_class_hash.as_deref(), Some(baseline_hash.as_str()));

    // Ciclo 3: Avançar para t2 = t1 + 3600s; NSAC retorna HTTP 200 com HTML truncado/corrompido (< 18 células na linha)
    let t2 = t1 + ChronoDuration::seconds(3_600);
    clock.set(t2);
    nsac_server.reset().await;
    let truncated_html = r#"<!DOCTYPE html><html><body>
      <div class="box box-primary">
        <div class="box-header"><h4>INF1/2026</h4></div>
        <div class="box-body"><table id="notas"><tbody>
          <tr class="linha"><td><span>SO</span></td><td class="text-center">8.0</td><td class="text-center">7.5</td></tr>
        </tbody></table></div>
      </div></body></html>"#;
    Mock::given(method("GET"))
        .and(path("/nsac/aluno/boletim"))
        .respond_with(ResponseTemplate::new(200).set_body_string(truncated_html))
        .mount(&nsac_server)
        .await;

    let rep3 = poll_due_classes_once(&state, &fcm).await.unwrap();
    assert_eq!(rep3.classes_errored, 1);
    assert_eq!(rep3.classes_changed, 0);
    assert_eq!(rep3.fcm_delivered, 0);

    let st3 = sentinel_repo
        .get_class_state(&class_code)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(st3.consecutive_errors, 3);
    // Backoff n=3: midnight_backoff_secs * 2^2 = 7200s
    assert_eq!(st3.next_check_at, t2 + ChronoDuration::seconds(7_200));
    assert_eq!(
        st3.last_class_hash.as_deref(),
        Some(baseline_hash.as_str()),
        "Corrupted HTML must never overwrite last_class_hash"
    );

    // Comprovar 0 eventos falsos no banco e 0 chamadas ao FCM durante os 3 ciclos de falha
    let events_during_outage = sentinel_repo
        .list_events_for_class(&class_code)
        .await
        .unwrap();
    assert!(events_during_outage.is_empty());

    let fcm_requests_during_outage = fcm_server.received_requests().await.unwrap();
    assert!(
        fcm_requests_during_outage.is_empty(),
        "FCM must receive 0 requests during NSAC outage/corruption"
    );

    // Ciclo 4: Restauração do NSAC em t3 = t2 + 7200s com nota nova real!
    let t3 = t2 + ChronoDuration::seconds(7_200);
    clock.set(t3);
    nsac_server.reset().await;
    let recovered_new_grade_html = render_boletim_html(
        &class_code,
        2026,
        "Sistemas Operacionais",
        "SO",
        7.5,
        6.8,
        Some(8.5),
        Some(7.9),
    );
    Mock::given(method("GET"))
        .and(path("/nsac/aluno/boletim"))
        .respond_with(ResponseTemplate::new(200).set_body_string(recovered_new_grade_html))
        .mount(&nsac_server)
        .await;

    let rep4 = poll_due_classes_once(&state, &fcm).await.unwrap();
    assert_eq!(rep4.classes_errored, 0);
    assert_eq!(rep4.classes_checked, 1);
    assert_eq!(rep4.classes_changed, 1);
    assert_eq!(rep4.fcm_delivered, 1);

    let st4 = sentinel_repo
        .get_class_state(&class_code)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(st4.consecutive_errors, 0);
    assert_ne!(st4.last_class_hash.as_deref(), Some(baseline_hash.as_str()));
    assert!((st4.current_certainty - 1.0).abs() < 1e-9);

    let events_after_recovery = sentinel_repo
        .list_events_for_class(&class_code)
        .await
        .unwrap();
    assert_eq!(events_after_recovery.len(), 1);
    assert_eq!(events_after_recovery[0].subject_name, "Sistemas Operacionais");
    assert_eq!(events_after_recovery[0].bimester, 2);

    cleanup_classes(&pool, &[class_code]).await;
}

// ============================================================================
// CENÁRIO 4: Thundering Herd
// (Onda Simultânea em Múltiplas Turmas do CTI + Isolamento de Fan-Out + Limite de Semáforo <= 3)
// ============================================================================
#[tokio::test]
async fn scenario_4_thundering_herd_multi_class_fanout_isolation_and_semaphore_cap() {
    let _poll_guard = POLL_TEST_MUTEX.lock().await;
    let pool = common::test_pool().await;
    let suffix = common::unique_suffix();
    let sentinel_repo = SentinelRepo::new(pool.clone());
    let sentinel_cfg = default_sentinel_config();

    let nsac_server = MockServer::start().await;
    let fcm_server = MockServer::start().await;
    mount_fcm_mock(&fcm_server).await;

    let t0 = utc_date_noon(2026, 7, 10);
    let clock = ManualClock::new(t0);
    let max_concurrent = 3_usize;
    let cfg = make_sim_config(
        format!("{}/nsac", nsac_server.uri()),
        Some(fcm_server.uri()),
        max_concurrent,
        5,
        sentinel_cfg.clone(),
    );
    let heuristic = SentinelHeuristic::arc(sentinel_repo.clone(), sentinel_cfg.clone());
    let state = AppState::new(cfg, pool.clone(), heuristic.clone(), Arc::new(clock.clone()))
        .expect("build AppState");
    let fcm = FcmClient::new(state.config.fcm.clone()).expect("build FcmClient");

    let changed_classes = [
        format!("INI3A_TH_{suffix}"),
        format!("INF1_TH_{suffix}"),
        format!("MEC3_TH_{suffix}"),
    ];
    let unchanged_classes = [format!("ELI2_TH_{suffix}"), format!("MEI1_TH_{suffix}")];
    let all_classes: Vec<String> = changed_classes
        .iter()
        .chain(unchanged_classes.iter())
        .cloned()
        .collect();
    freeze_other_sentinel_classes(&pool, &all_classes).await;

    // Cadastrar 3 alunos por turma (total 15 alunos em 5 turmas)
    for (c_idx, class_code) in all_classes.iter().enumerate() {
        let is_changed = c_idx < 3;
        let baseline_html = render_boletim_html(
            class_code,
            2026,
            "Matemática Aplicada",
            "Mat",
            7.0,
            6.5,
            None,
            None,
        );
        let live_html = if is_changed {
            render_boletim_html(
                class_code,
                2026,
                "Matemática Aplicada",
                "Mat",
                7.0,
                6.5,
                Some(8.8),
                Some(7.9),
            )
        } else {
            baseline_html.clone()
        };

        let mut first_student_id = 0_i64;
        let mut class_baseline_hash = String::new();

        for s_idx in 1..=3 {
            let sess_tok = format!("sess_th_{c_idx}_{s_idx}_{suffix}");
            let email = format!("th_{c_idx}_{s_idx}_{suffix}@unesp.br");
            let fcm_tok = format!("fcm_th_{class_code}_{s_idx}");

            let (acct_id, hash) = seed_student_with_baseline_boletim(
                &state,
                &email,
                "ThPass123!",
                &sess_tok,
                class_code,
                Some(&fcm_tok),
                &baseline_html,
                t0,
            )
            .await;

            if s_idx == 1 {
                first_student_id = acct_id;
                class_baseline_hash = hash;
            }

            // Montar rota no NSAC mock identificada pelo cookie do aluno com pequeno delay (25ms)
            // para exercitar contenção real no semáforo
            Mock::given(method("GET"))
                .and(path("/nsac/aluno/boletim"))
                .and(header_regex("cookie", &format!("nsaconline_session={sess_tok}")))
                .respond_with(
                    ResponseTemplate::new(200)
                        .set_delay(std::time::Duration::from_millis(25))
                        .set_body_string(live_html.clone()),
                )
                .mount(&nsac_server)
                .await;
        }

        heuristic
            .initialize_class_on_login(class_code, &class_baseline_hash, first_student_id, t0)
            .await
            .unwrap();
        let mut st = sentinel_repo
            .get_class_state(class_code)
            .await
            .unwrap()
            .unwrap();
        st.next_check_at = t0;
        sentinel_repo.upsert_class_state(&st).await.unwrap();
    }

    state.semaphore.reset_metrics();

    // Disparar poll_due_classes_once junto com requisições concorrentes sob o mesmo semáforo
    // para comprovar que `peak_count() <= 3` jamais é violado sob carga concorrente (Thundering Herd)
    let state_bg1 = state.clone();
    let state_bg2 = state.clone();
    let bg1 = tokio::spawn(async move {
        let _p = state_bg1.semaphore.acquire().await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    });
    let bg2 = tokio::spawn(async move {
        let _p = state_bg2.semaphore.acquire().await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    });

    let report = poll_due_classes_once(&state, &fcm).await.unwrap();
    bg1.await.unwrap();
    bg2.await.unwrap();

    // 4.1 Limite estrito do semáforo: peak_count() <= 3
    assert!(
        state.semaphore.peak_count() <= max_concurrent,
        "Semaphore peak_count ({}) exceeded max_concurrent ({max_concurrent})!",
        state.semaphore.peak_count()
    );
    assert!(state.semaphore.peak_count() >= 1);

    // 4.2 Verificação do relatório de ciclo:
    // 5 turmas checadas, 3 alteradas (3 alunos cada = 9 alunos atualizados e 9 pushes FCM), 2 inalteradas
    assert_eq!(report.classes_checked, 5);
    assert_eq!(report.classes_changed, 3);
    assert_eq!(report.classes_errored, 0);
    assert_eq!(report.fanout_students_updated, 9);
    assert_eq!(report.fcm_delivered, 9);

    // 4.3 Verificar requisições NSAC recebidas por turma:
    // - Cada turma alterada (INI3A, INF1, MEC3): 1 sentinela + 2 fan-out = 3 reqs (total 9)
    // - Cada turma inalterada (ELI2, MEI1): apenas 1 checagem sentinela + 0 fan-out = 1 req (total 2)
    let nsac_reqs = nsac_server.received_requests().await.unwrap();
    let boletim_reqs: Vec<_> = nsac_reqs
        .iter()
        .filter(|r| r.url.path() == "/nsac/aluno/boletim")
        .collect();
    assert_eq!(
        boletim_reqs.len(),
        11,
        "Expected 3*3 (changed) + 2*1 (unchanged) = 11 NSAC boletim requests"
    );

    for (c_idx, _class_code) in unchanged_classes.iter().enumerate() {
        let u_idx = c_idx + 3;
        let sentinel_cookie = format!("nsaconline_session=sess_th_{u_idx}_1_{suffix}");
        let fanout2_cookie = format!("nsaconline_session=sess_th_{u_idx}_2_{suffix}");
        let fanout3_cookie = format!("nsaconline_session=sess_th_{u_idx}_3_{suffix}");

        let count_sentinel = boletim_reqs
            .iter()
            .filter(|r| {
                r.headers
                    .get("cookie")
                    .and_then(|v| v.to_str().ok())
                    .is_some_and(|c| c.contains(&sentinel_cookie))
            })
            .count();
        let count_fanout = boletim_reqs
            .iter()
            .filter(|r| {
                r.headers
                    .get("cookie")
                    .and_then(|v| v.to_str().ok())
                    .is_some_and(|c| c.contains(&fanout2_cookie) || c.contains(&fanout3_cookie))
            })
            .count();
        assert_eq!(count_sentinel, 1, "Unchanged class must perform exactly 1 sentinel check");
        assert_eq!(count_fanout, 0, "Unchanged class must perform 0 fan-out requests");
    }

    // 4.4 Verificar que o FCM recebeu notificações EXCLUSIVAMENTE para as turmas alteradas
    let fcm_reqs = fcm_server.received_requests().await.unwrap();
    let fcm_send_reqs: Vec<_> = fcm_reqs
        .iter()
        .filter(|r| r.url.path().ends_with("/messages:send"))
        .collect();
    assert_eq!(fcm_send_reqs.len(), 9);

    for req in &fcm_send_reqs {
        let body_str = String::from_utf8_lossy(&req.body);
        for unch in &unchanged_classes {
            assert!(
                !body_str.contains(unch),
                "Unchanged class {unch} must receive 0 FCM push notifications"
            );
        }
        assert!(
            changed_classes.iter().any(|ch| body_str.contains(ch)),
            "FCM push must belong to one of the changed classes"
        );
    }

    cleanup_classes(&pool, &all_classes).await;
}

// ============================================================================
// CENÁRIO 5: Envenenamento Estatístico / Outliers Fora de Época
// (Dezenas de eventos forjados 60-120 dias longe produzem 0.0 na data atual)
// ============================================================================
#[tokio::test]
async fn scenario_5_statistical_poisoning_out_of_season_outliers_produce_zero_certainty() {
    let pool = common::test_pool().await;
    let suffix = common::unique_suffix();
    let poisoned_class = format!("POISON_{suffix}");
    let legitimate_class = format!("LEGIT_{suffix}");
    let sentinel_repo = SentinelRepo::new(pool.clone());
    let sentinel_cfg = default_sentinel_config();

    // Data atual de avaliação: DOY = 200
    let now = utc_from_doy(2026, 200);

    // 5.1 Injetar 45 eventos forjados fora de época (60 a 120 dias longe de DOY 200: DOY 80..=140 e DOY 260..=320)
    let mut outlier_events = Vec::new();
    for i in 0..45_i16 {
        let outlier_doy = if i % 2 == 0 {
            80 + (i % 61) // distância de 60 a 120 dias antes de 200
        } else {
            260 + (i % 61) // distância de 60 a 120 dias depois de 200
        };
        outlier_events.push(NewGradePostingEvent {
            class_code: poisoned_class.clone(),
            school_year: 2024 + i32::from(i % 2),
            bimester: (i % 4) + 1,
            subject_name: format!("OutlierSubj_{i}"),
            day_of_year: outlier_doy,
            month: 3,
            day: 15,
            posted_at: utc_from_doy(2025, u32::try_from(outlier_doy).unwrap()),
        });
    }
    sentinel_repo
        .insert_posting_events_batch(&outlier_events)
        .await
        .unwrap();

    let stored_outliers = sentinel_repo
        .list_events_for_class(&poisoned_class)
        .await
        .unwrap();
    assert_eq!(stored_outliers.len(), 45);

    let poisoned_certainty = compute_historical_certainty(
        &stored_outliers,
        now,
        sentinel_cfg.historical_radius_days,
    );
    let poisoned_interval =
        compute_interval_from_certainty(poisoned_certainty, false, &sentinel_cfg);
    assert_eq!(
        poisoned_certainty, 0.0,
        "45 out-of-season outlier events (60-120 days away) must produce strictly 0.0 certainty"
    );
    assert_eq!(
        poisoned_interval, 86_400,
        "Poisoned out-of-season class must stay asleep at 24h cold baseline"
    );

    // 5.2 Turma com cluster legítimo dentro de ±4 dias (DOY 198, 200, 201) + alguns outliers fora de época
    let mut legit_events = outlier_events
        .iter()
        .take(10)
        .cloned()
        .map(|mut e| {
            e.class_code = legitimate_class.clone();
            e
        })
        .collect::<Vec<_>>();
    for (yr, doy) in [(2024, 198_i16), (2024, 200), (2025, 201)] {
        legit_events.push(NewGradePostingEvent {
            class_code: legitimate_class.clone(),
            school_year: yr,
            bimester: 2,
            subject_name: "Eletrônica Digital".to_string(),
            day_of_year: doy,
            month: 7,
            day: 19,
            posted_at: utc_from_doy(yr, u32::try_from(doy).unwrap()),
        });
    }
    sentinel_repo
        .insert_posting_events_batch(&legit_events)
        .await
        .unwrap();

    let stored_legit = sentinel_repo
        .list_events_for_class(&legitimate_class)
        .await
        .unwrap();
    let legit_certainty =
        compute_historical_certainty(&stored_legit, now, sentinel_cfg.historical_radius_days);
    let legit_interval = compute_interval_from_certainty(legit_certainty, false, &sentinel_cfg);

    assert!(
        legit_certainty > 0.70,
        "Legitimate cluster within ±4 days must activate normally with high certainty (got {legit_certainty})"
    );
    assert!(
        legit_interval <= 7_200,
        "Legitimate cluster must drop polling interval to historical range <= 7200s (got {legit_interval}s)"
    );

    cleanup_classes(&pool, &[poisoned_class, legitimate_class]).await;
}

// ============================================================================
// CENÁRIO 6: Failover Automático de Sentinela sob Credencial/Cookie Quebrado
// Durante Janela Quente
// ============================================================================
#[tokio::test]
async fn scenario_6_automatic_sentinel_failover_on_broken_credentials_during_hot_window() {
    let _poll_guard = POLL_TEST_MUTEX.lock().await;
    let pool = common::test_pool().await;
    let suffix = common::unique_suffix();
    let class_code = format!("INI3A_FAILOVER_{suffix}");
    freeze_other_sentinel_classes(&pool, std::slice::from_ref(&class_code)).await;
    let sentinel_repo = SentinelRepo::new(pool.clone());
    let sentinel_cfg = default_sentinel_config();

    let nsac_server = MockServer::start().await;
    let fcm_server = MockServer::start().await;
    mount_fcm_mock(&fcm_server).await;

    let t0 = utc_date_noon(2026, 9, 28);
    let clock = ManualClock::new(t0);
    let cfg = make_sim_config(
        format!("{}/nsac", nsac_server.uri()),
        Some(fcm_server.uri()),
        4,
        5,
        sentinel_cfg.clone(),
    );
    let heuristic = SentinelHeuristic::arc(sentinel_repo.clone(), sentinel_cfg.clone());
    let state = AppState::new(cfg, pool.clone(), heuristic.clone(), Arc::new(clock.clone()))
        .expect("build AppState");
    let fcm = FcmClient::new(state.config.fcm.clone()).expect("build FcmClient");

    let baseline_html = render_boletim_html(
        &class_code,
        2026,
        "Desenvolvimento Web",
        "DevWeb",
        8.2,
        7.4,
        None,
        None,
    );
    let new_grade_html = render_boletim_html(
        &class_code,
        2026,
        "Desenvolvimento Web",
        "DevWeb",
        8.2,
        7.4,
        Some(9.5),
        Some(8.6),
    );

    // Cadastrar 2 alunos na turma INI3A: sentinel_primaria e sentinel_backup
    let prim_email = format!("sentinel_primaria_{suffix}@unesp.br");
    let prim_old_pass = "OldPasswordBroken!";
    let prim_sess = format!("sess_prim_broken_{suffix}");

    let backup_email = format!("sentinel_backup_{suffix}@unesp.br");
    let backup_pass = "BackupValidPass!";
    let backup_sess = format!("sess_backup_valid_{suffix}");

    let (primaria_id, baseline_hash) = seed_student_with_baseline_boletim(
        &state,
        &prim_email,
        prim_old_pass,
        &prim_sess,
        &class_code,
        Some(&format!("fcm_prim_{suffix}")),
        &baseline_html,
        t0,
    )
    .await;

    let (backup_id, _) = seed_student_with_baseline_boletim(
        &state,
        &backup_email,
        backup_pass,
        &backup_sess,
        &class_code,
        Some(&format!("fcm_backup_{suffix}")),
        &baseline_html,
        t0,
    )
    .await;

    // Configurar a turma em Janela Quente com active_sentinel_account_id = Some(primaria_id)
    heuristic
        .initialize_class_on_login(&class_code, &baseline_hash, primaria_id, t0)
        .await
        .unwrap();
    let mut hot_class_state = sentinel_repo
        .get_class_state(&class_code)
        .await
        .unwrap()
        .unwrap();
    hot_class_state.active_sentinel_account_id = Some(primaria_id);
    hot_class_state.hot_until = Some(t0 + ChronoDuration::seconds(3_600));
    hot_class_state.current_certainty = 1.0;
    hot_class_state.current_interval_secs = 120;
    hot_class_state.next_check_at = t0;
    sentinel_repo
        .upsert_class_state(&hot_class_state)
        .await
        .unwrap();

    // Configurar Mock NSAC:
    // 1) Cookie da sentinel_primaria expirou (redireciona para /nsac/login -> NsacAuthExpired)
    Mock::given(method("GET"))
        .and(path("/nsac/aluno/boletim"))
        .and(header_regex("cookie", &format!("nsaconline_session={prim_sess}")))
        .respond_with(
            ResponseTemplate::new(302)
                .insert_header("Location", format!("{}/nsac/login", nsac_server.uri())),
        )
        .mount(&nsac_server)
        .await;

    // 2) Tentativa de auto-refresh (re-login) da sentinel_primaria falha porque o aluno trocou a senha no portal NSAC
    Mock::given(method("GET"))
        .and(path("/nsac"))
        .respond_with(
            ResponseTemplate::new(200)
                .append_header("Set-Cookie", "XSRF-TOKEN=init_xsrf; Path=/")
                .append_header("Set-Cookie", "nsaconline_session=init_sess; Path=/")
                .set_body_string(include_str!("../../nsac-core/tests/fixtures/login_page.html")),
        )
        .mount(&nsac_server)
        .await;

    let prim_pass_encoded = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("password", prim_old_pass)
        .finish();
    Mock::given(method("POST"))
        .and(path("/nsac/login"))
        .and(body_string_contains(prim_pass_encoded.as_str()))
        .respond_with(
            ResponseTemplate::new(302)
                .insert_header("Location", format!("{}/nsac/login", nsac_server.uri())),
        )
        .mount(&nsac_server)
        .await;

    // 3) Cookie da sentinel_backup é válido e retorna o boletim com a nota nova!
    Mock::given(method("GET"))
        .and(path("/nsac/aluno/boletim"))
        .and(header_regex("cookie", &format!("nsaconline_session={backup_sess}")))
        .respond_with(ResponseTemplate::new(200).set_body_string(new_grade_html))
        .mount(&nsac_server)
        .await;

    // Executar ciclo de polling
    let report = poll_due_classes_once(&state, &fcm).await.unwrap();
    assert_eq!(report.classes_errored, 0, "Failover to backup sentinel must prevent class error");
    assert_eq!(report.classes_checked, 1);
    assert_eq!(report.classes_changed, 1);
    assert_eq!(report.fcm_delivered, 2, "Both students in class with FCM tokens receive push notification");

    // Comprovar rotação de active_sentinel_account_id para sentinel_backup
    let updated_state = sentinel_repo
        .get_class_state(&class_code)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        updated_state.active_sentinel_account_id,
        Some(backup_id),
        "Engine must rotate active_sentinel_account_id from broken primary ({primaria_id}) to backup ({backup_id})"
    );
    assert_eq!(updated_state.consecutive_errors, 0);
    assert_ne!(
        updated_state.last_class_hash.as_deref(),
        Some(baseline_hash.as_str())
    );

    // Comprovar gravação do evento em grade_posting_events
    let events = sentinel_repo
        .list_events_for_class(&class_code)
        .await
        .unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].subject_name, "Desenvolvimento Web");
    assert_eq!(events[0].bimester, 2);

    // Comprovar entrega de push FCM
    let fcm_reqs = fcm_server.received_requests().await.unwrap();
    let fcm_pushes: Vec<_> = fcm_reqs
        .iter()
        .filter(|r| r.url.path().ends_with("/messages:send"))
        .collect();
    assert_eq!(fcm_pushes.len(), 2);

    cleanup_classes(&pool, &[class_code]).await;
}
