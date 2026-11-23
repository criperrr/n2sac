mod common;

use chrono::Utc;
use nsac_core::crypto::CryptoService;
use nsac_core::models::NsacSessionCookies;
use nsac_server::db::moderation::{ReportKind, ReportOutcome};
use nsac_server::db::{AccountRepo, ModerationRepo};
use sqlx::PgPool;

fn crypto() -> CryptoService {
    CryptoService::from_hex_key("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef").unwrap()
}

async fn account(pool: &PgPool, name: &str) -> i64 {
    let cookies = NsacSessionCookies::new("x", "s", None).unwrap();
    let email = format!("{name}.{}@mod.test", common::unique_suffix());
    AccountRepo::new(pool.clone())
        .upsert_on_login(&crypto(), &email, "pw", &cookies, Some("31A"), None, None, Utc::now())
        .await
        .unwrap()
        .id
}

async fn set_media(pool: &PgPool, id: i64) {
    sqlx::query(
        "INSERT INTO account_profiles (account_id, avatar_data_url, banner_data_url, bio) VALUES ($1, 'data:image/jpeg;base64,AAAA', 'data:image/jpeg;base64,BBBB', 'oi')
         ON CONFLICT (account_id) DO UPDATE SET avatar_data_url = EXCLUDED.avatar_data_url, banner_data_url = EXCLUDED.banner_data_url",
    )
    .bind(id)
    .execute(pool)
    .await
    .unwrap();
}

async fn media_of(pool: &PgPool, id: i64) -> (Option<String>, Option<String>) {
    sqlx::query_as("SELECT avatar_data_url, banner_data_url FROM account_profiles WHERE account_id = $1")
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn three_distinct_reporters_strike_then_second_strike_is_permanent() {
    let pool = common::test_pool().await;
    let repo = ModerationRepo::new(pool.clone());
    let target = account(&pool, "alvo").await;
    let r: Vec<i64> = futures_reporters(&pool).await;
    set_media(&pool, target).await;

    assert!(repo.report(target, target, ReportKind::Media).await.is_err(), "no self reports");
    assert_eq!(repo.report(r[0], target, ReportKind::Media).await.unwrap(), ReportOutcome::Received);
    assert_eq!(repo.report(r[0], target, ReportKind::Media).await.unwrap(), ReportOutcome::AlreadyReported, "one vote per person");
    assert_eq!(repo.report(r[1], target, ReportKind::Media).await.unwrap(), ReportOutcome::Received);
    assert!(media_of(&pool, target).await.0.is_some(), "two votes are not enough");
    assert_eq!(repo.report(r[2], target, ReportKind::Media).await.unwrap(), ReportOutcome::Actioned);

    assert_eq!(media_of(&pool, target).await, (None, None), "photo and banner deleted from the database");
    let restriction = repo.restriction(target, ReportKind::Media).await.unwrap().expect("restricted");
    assert!(!restriction.permanent && restriction.until.is_some(), "first strike is temporary");
    assert!(repo.restriction(target, ReportKind::Text).await.unwrap().is_none(), "text untouched");
    assert!(repo.report(r[3], target, ReportKind::Media).await.is_err(), "nothing left to report");

    // Restriction lapses, they upload again and get reported again by 3 people → permanent.
    sqlx::query("UPDATE moderation_restrictions SET restricted_until = NOW() - INTERVAL '1 minute' WHERE account_id = $1")
        .bind(target)
        .execute(&pool)
        .await
        .unwrap();
    assert!(repo.restriction(target, ReportKind::Media).await.unwrap().is_none());
    set_media(&pool, target).await;
    for reporter in &r[0..2] {
        assert_eq!(repo.report(*reporter, target, ReportKind::Media).await.unwrap(), ReportOutcome::Received);
    }
    assert_eq!(repo.report(r[3], target, ReportKind::Media).await.unwrap(), ReportOutcome::Actioned);
    let restriction = repo.restriction(target, ReportKind::Media).await.unwrap().expect("restricted");
    assert!(restriction.permanent);

    let events: i64 = sqlx::query_scalar("SELECT COUNT(*)::bigint FROM moderation_events WHERE account_id = $1")
        .bind(target)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(events, 2, "audit log");
}

#[tokio::test]
async fn text_reports_clear_name_and_bio_and_daily_limit_applies() {
    let pool = common::test_pool().await;
    let repo = ModerationRepo::new(pool.clone());
    let target = account(&pool, "texto").await;
    let r = futures_reporters(&pool).await;
    set_media(&pool, target).await;
    for reporter in &r[0..3] {
        repo.report(*reporter, target, ReportKind::Text).await.unwrap();
    }
    let bio: Option<String> = sqlx::query_scalar("SELECT bio FROM account_profiles WHERE account_id = $1")
        .bind(target)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(bio, None);
    assert!(media_of(&pool, target).await.0.is_some(), "media untouched by a text strike");

    // Daily limit for one reporter.
    let spammer = r[4];
    let mut hit_limit = false;
    for i in 0..12 {
        let victim = account(&pool, &format!("v{i}")).await;
        set_media(&pool, victim).await;
        if repo.report(spammer, victim, ReportKind::Media).await.is_err() {
            hit_limit = true;
            break;
        }
    }
    assert!(hit_limit, "reporters are rate limited");
}

async fn futures_reporters(pool: &PgPool) -> Vec<i64> {
    let mut out = Vec::new();
    for i in 0..5 {
        out.push(account(pool, &format!("rep{i}")).await);
    }
    out
}
