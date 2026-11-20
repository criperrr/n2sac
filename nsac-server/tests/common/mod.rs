use std::sync::atomic::{AtomicU64, Ordering};

use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use tokio::sync::OnceCell;
use url::Url;

static DB_INIT: OnceCell<String> = OnceCell::const_new();
static UNIQUE_SEQ: AtomicU64 = AtomicU64::new(1);

pub fn unique_suffix() -> String {
    let seq = UNIQUE_SEQ.fetch_add(1, Ordering::Relaxed);
    format!("{}_{}", std::process::id(), seq)
}

fn resolved_test_db_url() -> String {
    if let Ok(url) = std::env::var("TEST_DATABASE_URL") {
        let trimmed = url.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    "postgres://postgres@127.0.0.1:5432/nsac_m1_test".to_string()
}

fn validate_and_extract_db_name(db_url: &str) -> (String, String) {
    let parsed = Url::parse(db_url).expect("TEST_DATABASE_URL must be a valid URL");
    let db_name = parsed.path().trim_start_matches('/').to_string();
    assert!(
        !db_name.is_empty(),
        "Test database URL must include a database name"
    );
    assert_ne!(
        db_name, "nsac_e2e",
        "STRICT ISOLATION VIOLATION: M1 tests must NEVER connect to nsac_e2e"
    );
    assert_ne!(
        db_name, "postgres",
        "M1 tests must not run migrations against the postgres admin database"
    );

    let mut admin_url = parsed;
    admin_url.set_path("/postgres");
    (db_name, admin_url.to_string())
}

pub async fn test_pool() -> PgPool {
    let db_url = DB_INIT
        .get_or_init(|| async {
            let target_url = resolved_test_db_url();
            let (db_name, admin_url) = validate_and_extract_db_name(&target_url);

            let admin_pool = PgPoolOptions::new()
                .max_connections(2)
                .connect(&admin_url)
                .await
                .expect("Failed to connect to PostgreSQL admin database");

            let exists: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM pg_database WHERE datname = $1)",
            )
            .bind(&db_name)
            .fetch_one(&admin_pool)
            .await
            .expect("Failed to query pg_database");

            if !exists {
                let escaped_ident = db_name.replace('"', "\"\"");
                let create_sql = format!("CREATE DATABASE \"{escaped_ident}\"");
                // Tolerate concurrent test process creating the same database.
                if let Err(err) = sqlx::query(&create_sql).execute(&admin_pool).await {
                    let still_missing = !sqlx::query_scalar::<_, bool>(
                        "SELECT EXISTS(SELECT 1 FROM pg_database WHERE datname = $1)",
                    )
                    .bind(&db_name)
                    .fetch_one(&admin_pool)
                    .await
                    .unwrap_or(false);
                    if still_missing {
                        panic!("Failed to create test database {db_name}: {err}");
                    }
                }
            }
            admin_pool.close().await;

            let init_pool = PgPoolOptions::new()
                .max_connections(4)
                .connect(&target_url)
                .await
                .expect("Failed to connect to M1 test database");

            nsac_server::run_migrations(&init_pool)
                .await
                .expect("Failed to apply M1 migrations");
            init_pool.close().await;

            target_url
        })
        .await;

    PgPoolOptions::new()
        .max_connections(5)
        .connect(db_url)
        .await
        .expect("Failed to open test PgPool")
}
