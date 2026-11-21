mod common;

use chrono::{Duration, TimeZone, Utc};
use nsac_core::crypto::CryptoService;
use nsac_core::models::{
    AllYearsResponse, BimesterData, ClassBiInformation, FullGrades, NsacSessionCookies,
    PersonalBiInformation, RecoveryMessage, RecoveryStatusCode, ResultData, UnifiedBimesterData,
    WarningValue, YearInfo,
};
use nsac_core::sentinel::compute_class_grade_hash;
use nsac_server::config::ServiceKeySeed;
use nsac_server::db::{
    AccountRepo, AuthRepo, BoletimRepo, CTI_CLASSES, NewGradePostingEvent, SentinelRepo,
    SnapshotRepo, compute_strong_etag, sha256_hex,
};
use nsac_server::ports::{Clock, DbHeuristicHintStub, HeuristicObserver};
use serde_json::json;
use zeroize::Zeroizing;

struct FixedClock(chrono::DateTime<Utc>);

impl Clock for FixedClock {
    fn now(&self) -> chrono::DateTime<Utc> {
        self.0
    }
}

fn test_crypto() -> CryptoService {
    CryptoService::from_hex_key("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef")
        .unwrap()
}

fn sample_boletim(class_code: &str) -> AllYearsResponse {
    let bim = |b: u8, g: Option<f64>, avg: Option<f64>, code: RecoveryStatusCode, rec: Option<bool>| {
        let recovery = code != RecoveryStatusCode::Nac;
        UnifiedBimesterData {
            bimester: b,
            personal: PersonalBiInformation {
                grade: g,
                absences: u32::from(b),
                recovery,
                recovered: rec,
                recovery_code: Some(code),
                recovery_message: Some(RecoveryMessage::from_title_or_code(None, code)),
            },
            class: ClassBiInformation { average_grade: avg },
        }
    };

    AllYearsResponse {
        warning: WarningValue::Bool(false),
        user_current_year: 2,
        data: vec![
            YearInfo {
                title: "INF1".to_string(),
                year: 2025,
                status: "Aprovado".to_string(),
                grades: vec![FullGrades {
                    subject_name: "Algoritmos".to_string(),
                    results: ResultData {
                        grade: Some(8.5),
                        total_absences: 4,
                    },
                    bimesters: vec![
                        bim(1, Some(8.0), Some(7.0), RecoveryStatusCode::Nac, None),
                        bim(2, Some(9.0), Some(7.5), RecoveryStatusCode::Nac, None),
                        bim(3, Some(0.0), Some(6.0), RecoveryStatusCode::Sat, Some(true)),
                        bim(4, Some(8.5), Some(7.2), RecoveryStatusCode::Nac, None),
                    ],
                }],
                bimesters_metrics: vec![
                    BimesterData {
                        user_average: 8.0,
                        class_average: 7.0,
                        total_absences: 1,
                    },
                    BimesterData {
                        user_average: 9.0,
                        class_average: 7.5,
                        total_absences: 2,
                    },
                    BimesterData {
                        user_average: 0.0,
                        class_average: 6.0,
                        total_absences: 3,
                    },
                    BimesterData {
                        user_average: 8.5,
                        class_average: 7.2,
                        total_absences: 4,
                    },
                ],
            },
            YearInfo {
                title: class_code.to_string(),
                year: 2026,
                status: "Cursando".to_string(),
                grades: vec![
                    FullGrades {
                        subject_name: "Banco de Dados".to_string(),
                        results: ResultData {
                            grade: None,
                            total_absences: 2,
                        },
                        bimesters: vec![
                            bim(1, Some(7.5), Some(6.8), RecoveryStatusCode::Nac, None),
                            bim(2, None, None, RecoveryStatusCode::Nac, None),
                            bim(3, Some(4.5), Some(5.5), RecoveryStatusCode::Ins, Some(false)),
                            bim(4, None, Some(0.0), RecoveryStatusCode::Nc, Some(false)),
                        ],
                    },
                    FullGrades {
                        subject_name: "Matemática".to_string(),
                        results: ResultData {
                            grade: None,
                            total_absences: 0,
                        },
                        bimesters: vec![
                            bim(1, Some(10.0), Some(8.0), RecoveryStatusCode::Nac, None),
                            bim(2, None, None, RecoveryStatusCode::Nac, None),
                            bim(3, None, None, RecoveryStatusCode::Nac, None),
                            bim(4, None, None, RecoveryStatusCode::Nac, None),
                        ],
                    },
                ],
                bimesters_metrics: vec![
                    BimesterData {
                        user_average: 8.75,
                        class_average: 7.4,
                        total_absences: 1,
                    },
                    BimesterData {
                        user_average: 0.0,
                        class_average: 0.0,
                        total_absences: 0,
                    },
                    BimesterData {
                        user_average: 4.5,
                        class_average: 5.5,
                        total_absences: 3,
                    },
                    BimesterData {
                        user_average: 0.0,
                        class_average: 0.0,
                        total_absences: 0,
                    },
                ],
            },
        ],
    }
}

#[tokio::test]
async fn migrations_create_all_12_tables_and_indexes_idempotently() {
    let pool = common::test_pool().await;
    nsac_server::run_migrations(&pool).await.unwrap();

    let expected_tables = [
        "accounts",
        "school_years",
        "accounts_school_years",
        "subjects",
        "bimesters",
        "grades",
        "subject_final_results",
        "service_clients",
        "user_sessions",
        "auxiliary_snapshots",
        "sentinel_class_state",
        "grade_posting_events",
        "account_ranking_preferences",
        "account_profiles",
        "telemetry_events",
        "discord_channel_subscriptions",
        "discord_dm_preferences",
        "social_follows",
        "social_blocks",
        "social_messages",
        "social_conversation_state",
    ];

    for table in expected_tables {
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM information_schema.tables WHERE table_schema = 'public' AND table_name = $1)",
        )
        .bind(table)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(exists, "Expected table {table} to exist");
    }

    for idx in [
        "idx_grade_posting_events_class_doy",
        "idx_grade_posting_events_class_year_bim",
        "idx_ranking_prefs_opted_in",
        "idx_telemetry_events_type_created",
        "idx_school_years_year",
        "idx_grades_account_bimester_posted",
        "idx_accounts_upper_class_code_active",
    ] {
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM pg_indexes WHERE schemaname = 'public' AND indexname = $1)",
        )
        .bind(idx)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(exists, "Expected index {idx} to exist");
    }
}

#[tokio::test]
async fn accounts_and_auth_store_zero_plaintext_secrets_in_db_or_debug() {
    let pool = common::test_pool().await;
    let crypto = test_crypto();
    let suffix = common::unique_suffix();
    let accounts = AccountRepo::new(pool.clone());
    let auth = AuthRepo::new(pool.clone());
    let now = Utc::now();

    let email = format!("aluno_{suffix}@unesp.br");
    let raw_pass = format!("SuperSecretPass_{suffix}!");
    let raw_xsrf = format!("xsrf_secret_{suffix}");
    let raw_sess = format!("sess_secret_{suffix}");
    let cookies = NsacSessionCookies::new(raw_xsrf.clone(), raw_sess.clone(), None).unwrap();

    let summary = accounts
        .upsert_on_login(
            &crypto,
            &email,
            &raw_pass,
            &cookies,
            Some("ini2a"),
            Some(&format!("discord_{suffix}")),
            Some(&format!("fcm_tok_{suffix}")),
            now,
        )
        .await
        .unwrap();
    assert_eq!(summary.class_code.as_deref(), Some("INI2A"));
    assert!(summary.has_fcm_token);

    let (pass_enc, cookies_enc): (String, Option<String>) = sqlx::query_as(
        "SELECT nsac_pass_enc, nsac_cookies_enc FROM accounts WHERE id = $1",
    )
    .bind(summary.id)
    .fetch_one(&pool)
    .await
    .unwrap();

    assert!(pass_enc.starts_with("v1:"));
    assert!(!pass_enc.contains(&raw_pass));
    let cookies_enc_str = cookies_enc.unwrap();
    assert!(cookies_enc_str.starts_with("v1:"));
    assert!(!cookies_enc_str.contains(&raw_xsrf));
    assert!(!cookies_enc_str.contains(&raw_sess));

    let decrypted = accounts
        .get_decrypted_credentials(&crypto, summary.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(decrypted.password.as_str(), raw_pass);
    assert_eq!(decrypted.cookies.as_ref().unwrap().xsrf_token(), raw_xsrf);
    assert_eq!(decrypted.cookies.as_ref().unwrap().session(), raw_sess);

    let debug_secrets = format!("{decrypted:?}");
    assert!(debug_secrets.contains("[REDACTED]"));
    assert!(!debug_secrets.contains(&raw_pass));
    assert!(!debug_secrets.contains(&raw_xsrf));
    assert!(!debug_secrets.contains(&raw_sess));

    // Service client & user session hashing
    let client_name = format!("bot_{suffix}");
    let raw_service_key = format!("svc_key_secret_{suffix}");
    auth.seed_service_clients(&[ServiceKeySeed {
        name: client_name.clone(),
        raw_key: Zeroizing::new(raw_service_key.clone()),
        scopes: vec!["nsac:read".to_string()],
    }])
    .await
    .unwrap();

    let stored_key_hash: String =
        sqlx::query_scalar("SELECT key_hash FROM service_clients WHERE name = $1")
            .bind(&client_name)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(stored_key_hash.len(), 64);
    assert_eq!(stored_key_hash, sha256_hex(&raw_service_key));
    assert!(!stored_key_hash.contains(&raw_service_key));

    let svc = auth
        .find_service_client_by_raw_key(&raw_service_key)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(svc.name, client_name);
    assert!(svc.active);

    let session = auth
        .create_user_session(summary.id, 3600, now)
        .await
        .unwrap();
    let session_debug = format!("{session:?}");
    assert!(session_debug.contains("[REDACTED]"));
    assert!(!session_debug.contains(session.token.as_str()));

    let stored_tok_hash: String =
        sqlx::query_scalar("SELECT token_hash FROM user_sessions WHERE id = $1")
            .bind(session.session_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(stored_tok_hash.len(), 64);
    assert_eq!(stored_tok_hash, sha256_hex(session.token.as_str()));
    assert!(!stored_tok_hash.contains(session.token.as_str()));

    let lookup = auth
        .find_user_session_by_raw_token(session.token.as_str())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(lookup.account_id, summary.id);
    assert!(lookup.revoked_at.is_none());
    assert!(lookup.account_active);

    assert!(
        auth.revoke_session_by_raw_token(session.token.as_str(), now)
            .await
            .unwrap()
    );
    let after_revoke = auth
        .find_user_session_by_raw_token(session.token.as_str())
        .await
        .unwrap()
        .unwrap();
    assert!(after_revoke.revoked_at.is_some());
}

#[tokio::test]
async fn boletim_roundtrip_null_grades_reupsert_and_transaction_rollback() {
    let pool = common::test_pool().await;
    let crypto = test_crypto();
    let suffix = common::unique_suffix();
    let accounts = AccountRepo::new(pool.clone());
    let boletim_repo = BoletimRepo::new(pool.clone());
    let now = Utc::now();

    let cookies = NsacSessionCookies::new("x", "s", None).unwrap();
    let acct = accounts
        .upsert_on_login(
            &crypto,
            &format!("boletim_{suffix}@unesp.br"),
            "pass",
            &cookies,
            Some("INF2"),
            None,
            None,
            now,
        )
        .await
        .unwrap();

    let expected = sample_boletim("INF2");
    boletim_repo.upsert_boletim(acct.id, &expected).await.unwrap();

    let loaded = boletim_repo.get_boletim(acct.id).await.unwrap().unwrap();
    assert_eq!(loaded, expected);
    assert_eq!(
        compute_class_grade_hash(&loaded),
        compute_class_grade_hash(&expected)
    );
    // Verify NULL vs Some(0.0) distinction survived PostgreSQL round-trip
    assert_eq!(loaded.data[0].grades[0].bimesters[2].personal.grade, Some(0.0));
    assert_eq!(loaded.data[1].grades[0].bimesters[1].personal.grade, None);
    assert_eq!(loaded.data[1].grades[0].results.grade, None);

    // Re-upsert mutating a grade updates cleanly with no duplicate rows
    let mut updated = expected.clone();
    updated.data[1].grades[0].bimesters[1].personal.grade = Some(9.25);
    updated.data[1].grades[0].bimesters[1].class.average_grade = Some(7.75);
    boletim_repo.upsert_boletim(acct.id, &updated).await.unwrap();

    let loaded2 = boletim_repo.get_boletim(acct.id).await.unwrap().unwrap();
    assert_eq!(loaded2, updated);

    let grade_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM grades WHERE account_id = $1")
            .bind(acct.id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(grade_count, 12);

    // Transactional rollback test on a fresh account: year 1 succeeds in tx, year 2 fails on bimester=5
    let acct2 = accounts
        .upsert_on_login(
            &crypto,
            &format!("rollback_{suffix}@unesp.br"),
            "pass",
            &cookies,
            Some("MEC1"),
            None,
            None,
            now,
        )
        .await
        .unwrap();

    let mut invalid = sample_boletim("MEC1");
    invalid.data[1].grades[0].bimesters[0].bimester = 5;
    let err = boletim_repo.upsert_boletim(acct2.id, &invalid).await.unwrap_err();
    assert_eq!(err.http_status(), 400);

    // Zero partial rows persisted for acct2 across all 4 account-scoped boletim tables
    assert!(boletim_repo.get_boletim(acct2.id).await.unwrap().is_none());
    for tbl in [
        "accounts_school_years",
        "bimesters",
        "grades",
        "subject_final_results",
    ] {
        let query = format!("SELECT COUNT(*) FROM {tbl} WHERE account_id = $1");
        let cnt: i64 = sqlx::query_scalar(&query)
            .bind(acct2.id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(cnt, 0, "Expected 0 rolled-back rows in {tbl}");
    }
}

#[tokio::test]
async fn supports_all_21_cti_classes_snapshots_sentinel_and_hint_stub() {
    let pool = common::test_pool().await;
    let crypto = test_crypto();
    let suffix = common::unique_suffix();
    let accounts = AccountRepo::new(pool.clone());
    let snapshots = SnapshotRepo::new(pool.clone());
    let sentinel = SentinelRepo::new(pool.clone());
    let now = Utc.with_ymd_and_hms(2026, 11, 20, 12, 0, 0).unwrap();
    let cookies = NsacSessionCookies::new("x", "s", None).unwrap();

    assert_eq!(CTI_CLASSES.len(), 21);
    let mut first_account_id = 0i64;
    for (idx, class_code) in CTI_CLASSES.iter().enumerate() {
        let acct = accounts
            .upsert_on_login(
                &crypto,
                &format!("cti_{idx}_{suffix}@unesp.br"),
                "pass",
                &cookies,
                Some(class_code),
                None,
                None,
                now,
            )
            .await
            .unwrap();
        if idx == 0 {
            first_account_id = acct.id;
        }
        sentinel
            .ensure_class_initialized(class_code, 86_400, now)
            .await
            .unwrap();
    }

    let active_classes = accounts.list_active_classes().await.unwrap();
    for class_code in CTI_CLASSES {
        assert!(
            active_classes.iter().any(|c| c == class_code),
            "Missing class {class_code}"
        );
    }

    // SnapshotRepo upsert/get/overwrite
    let home_json = json!({"portalDate": "20/11/2026"});
    let etag = compute_strong_etag(&home_json).unwrap();
    let saved = snapshots
        .upsert(first_account_id, "home", &home_json, &etag, now)
        .await
        .unwrap();
    assert_eq!(saved.etag, etag);
    let fetched = snapshots
        .get(first_account_id, "home")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(fetched.payload_json, home_json);

    // SentinelRepo atomic cooldown + multi-event batch (no duplicate constraint errors)
    let custom_class = format!("TST_{suffix}");
    accounts
        .upsert_on_login(
            &crypto,
            &format!("custom_{suffix}@unesp.br"),
            "pass",
            &cookies,
            Some(&custom_class),
            None,
            None,
            now,
        )
        .await
        .unwrap();

    let mut state = sentinel
        .ensure_class_initialized(&custom_class, 86_400, now - Duration::seconds(60))
        .await
        .unwrap();
    state.next_check_at = now + Duration::seconds(120);
    sentinel.upsert_class_state(&state).await.unwrap();

    let stub = DbHeuristicHintStub::new(sentinel.clone(), 86_400);
    assert_eq!(stub.get_poll_hint_secs(Some(&custom_class), now).await, 120);
    assert_eq!(stub.get_poll_hint_secs(Some("NON_EXISTENT"), now).await, 86_400);
    let fixed_clock = FixedClock(now + Duration::seconds(130));
    assert_eq!(
        stub.get_poll_hint_secs(Some(&custom_class), fixed_clock.now())
            .await,
        0
    );

    let bumped1 = sentinel
        .try_record_api_bump(&custom_class, 0.15, 1800, now)
        .await
        .unwrap()
        .unwrap();
    assert!((bumped1.current_certainty - 0.15).abs() < 1e-9);

    // Second bump within cooldown returns None
    let bumped2 = sentinel
        .try_record_api_bump(&custom_class, 0.15, 1800, now + Duration::seconds(600))
        .await
        .unwrap();
    assert!(bumped2.is_none());

    // After cooldown expires, bump succeeds
    let bumped3 = sentinel
        .try_record_api_bump(&custom_class, 0.15, 1800, now + Duration::seconds(1800))
        .await
        .unwrap()
        .unwrap();
    assert!((bumped3.current_certainty - 0.30).abs() < 1e-9);

    // Batch insert duplicate (class_code, school_year, bimester, subject_name) allowed
    let ev = NewGradePostingEvent {
        class_code: custom_class.clone(),
        school_year: 2025,
        bimester: 3,
        subject_name: "Matemática".to_string(),
        day_of_year: 250,
        month: 9,
        day: 7,
        posted_at: now,
    };
    let inserted = sentinel
        .insert_posting_events_batch(&[ev.clone(), ev])
        .await
        .unwrap();
    assert_eq!(inserted.len(), 2);

    let events = sentinel
        .list_events_for_class_year_bimester(&custom_class, 2025, 3)
        .await
        .unwrap();
    assert_eq!(events.len(), 2);

    let due = sentinel.list_due_classes(now + Duration::seconds(130)).await.unwrap();
    assert!(due.iter().any(|s| s.class_code == custom_class));
}

#[tokio::test]
async fn leaderboard_cte_avoids_toast_groupby_and_preserves_avatar_version() {
    let pool = common::test_pool().await;
    let crypto = test_crypto();
    let suffix = common::unique_suffix();
    let accounts = AccountRepo::new(pool.clone());
    let boletim_repo = BoletimRepo::new(pool.clone());
    let ranking_repo = nsac_server::db::RankingRepo::new(pool.clone());
    let now = Utc::now();

    let class_code = format!("TST{}", &suffix[suffix.len().saturating_sub(3)..]);
    let cookies = NsacSessionCookies::new("x", "s", None).unwrap();
    let acct = accounts
        .upsert_on_login(
            &crypto,
            &format!("rank_{suffix}@unesp.br"),
            "pass",
            &cookies,
            Some(&class_code),
            None,
            None,
            now,
        )
        .await
        .unwrap();
    boletim_repo
        .upsert_boletim(acct.id, &sample_boletim(&class_code))
        .await
        .unwrap();

    ranking_repo
        .upsert_preferences(acct.id, true, false, Some("Aluno Teste"), Some(true), Some(true))
        .await
        .unwrap();

    // Store a large TOASTed base64 avatar (>30 KB) and verify leaderboard returns avatar_url accurately.
    let big_avatar = format!("data:image/jpeg;base64,{}", "A".repeat(40_000));
    sqlx::query(
        r#"
        INSERT INTO account_profiles (account_id, display_name, avatar_data_url, updated_at)
        VALUES ($1, 'Aluno Teste', $2, NOW())
        ON CONFLICT (account_id) DO UPDATE SET avatar_data_url = EXCLUDED.avatar_data_url, updated_at = NOW()
        "#,
    )
    .bind(acct.id)
    .bind(&big_avatar)
    .execute(&pool)
    .await
    .unwrap();

    let summary = ranking_repo
        .fetch_leaderboard(acct.id, Some(&class_code), "class", 2026, None, None, None)
        .await
        .unwrap();
    assert_eq!(summary.total_participants, 1);
    let entry = summary.my_entry.expect("requester must appear in leaderboard");
    assert_eq!(entry.display_label, "Aluno Teste");
    assert!(
        entry
            .avatar_url
            .as_deref()
            .is_some_and(|u| u.starts_with(&format!("/api/v1/profiles/{}/avatar?v=", acct.id))),
        "expected versioned avatar_url, got {:?}",
        entry.avatar_url
    );
}
