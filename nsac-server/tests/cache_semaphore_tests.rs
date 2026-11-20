use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use nsac_core::models::{AllYearsResponse, WarningValue};
use nsac_server::config::CacheConfig;
use nsac_server::state::{
    AppCaches, CachedBoletim, CachedServiceClient, CachedSnapshot, CachedUserSession,
    NsacSemaphore, RouteKey,
};
use serde_json::json;

#[tokio::test]
async fn moka_caches_support_hits_and_targeted_invalidations() {
    let caches = AppCaches::new(CacheConfig {
        boletim_ttl_secs: 300,
        snapshot_ttl_secs: 300,
        auth_ttl_secs: 60,
    });
    assert!(caches.auth_cache_enabled());

    let now = Utc::now();
    let payload = Arc::new(AllYearsResponse {
        warning: WarningValue::Bool(false),
        user_current_year: 0,
        data: Vec::new(),
    });

    caches
        .boletim
        .insert(
            10,
            CachedBoletim {
                data: Arc::clone(&payload),
                etag: "\"etag-10\"".to_string(),
                cached_at: now,
            },
        )
        .await;
    caches
        .boletim
        .insert(
            20,
            CachedBoletim {
                data: Arc::clone(&payload),
                etag: "\"etag-20\"".to_string(),
                cached_at: now,
            },
        )
        .await;

    let hit10 = caches.boletim.get(&10).await.unwrap();
    assert!(Arc::ptr_eq(&hit10.data, &payload));
    assert_eq!(hit10.etag, "\"etag-10\"");

    caches.invalidate_boletim(10).await;
    assert!(caches.boletim.get(&10).await.is_none());
    assert!(caches.boletim.get(&20).await.is_some());

    caches
        .snapshots
        .insert(
            (10, RouteKey::Home.as_str()),
            CachedSnapshot {
                payload_json: json!({"home": true}),
                etag: "\"h10\"".to_string(),
                updated_at: now,
            },
        )
        .await;
    caches
        .snapshots
        .insert(
            (10, RouteKey::Atrasos.as_str()),
            CachedSnapshot {
                payload_json: json!([]),
                etag: "\"a10\"".to_string(),
                updated_at: now,
            },
        )
        .await;
    caches
        .snapshots
        .insert(
            (20, RouteKey::Home.as_str()),
            CachedSnapshot {
                payload_json: json!({"home": true}),
                etag: "\"h20\"".to_string(),
                updated_at: now,
            },
        )
        .await;

    caches.invalidate_snapshots_for_account(10).await;
    assert!(
        caches
            .snapshots
            .get(&(10, RouteKey::Home.as_str()))
            .await
            .is_none()
    );
    assert!(
        caches
            .snapshots
            .get(&(10, RouteKey::Atrasos.as_str()))
            .await
            .is_none()
    );
    assert!(
        caches
            .snapshots
            .get(&(20, RouteKey::Home.as_str()))
            .await
            .is_some()
    );

    caches
        .put_user_session(
            "tok-10-a".to_string(),
            CachedUserSession {
                session_id: 1,
                account_id: 10,
                expires_at: now,
                account_active: true,
            },
        )
        .await;
    caches
        .put_user_session(
            "tok-20-a".to_string(),
            CachedUserSession {
                session_id: 2,
                account_id: 20,
                expires_at: now,
                account_active: true,
            },
        )
        .await;
    caches
        .put_service_client(
            "svc-hash-1".to_string(),
            CachedServiceClient {
                id: 1,
                name: "bot".to_string(),
                scopes: vec!["nsac:read".to_string()],
                active: true,
            },
        )
        .await;

    assert!(caches.get_user_session("tok-10-a").await.is_some());
    assert!(caches.get_service_client("svc-hash-1").await.is_some());

    caches.invalidate_user_sessions_for_account(10);
    caches.user_sessions.run_pending_tasks().await;
    assert!(caches.get_user_session("tok-10-a").await.is_none());
    assert!(caches.get_user_session("tok-20-a").await.is_some());

    caches.invalidate_service_client_by_hash("svc-hash-1").await;
    assert!(caches.get_service_client("svc-hash-1").await.is_none());

    // Zero auth TTL bypasses auth caches for E2E out-of-band psql consistency
    let bypass = AppCaches::new(CacheConfig {
        boletim_ttl_secs: 300,
        snapshot_ttl_secs: 300,
        auth_ttl_secs: 0,
    });
    assert!(!bypass.auth_cache_enabled());
    bypass
        .put_user_session(
            "tok-bypass".to_string(),
            CachedUserSession {
                session_id: 3,
                account_id: 30,
                expires_at: now,
                account_active: true,
            },
        )
        .await;
    assert!(bypass.get_user_session("tok-bypass").await.is_none());
}

#[tokio::test]
async fn nsac_semaphore_bounds_concurrency_and_releases_on_cancel() {
    let sem = NsacSemaphore::new(4);
    assert_eq!(sem.max_permits(), 4);

    let mut handles = Vec::with_capacity(32);
    for _ in 0..32 {
        let sem_cloned = sem.clone();
        handles.push(tokio::spawn(async move {
            let guard = sem_cloned.acquire().await.unwrap();
            assert!(sem_cloned.active_count() <= 4);
            tokio::time::sleep(Duration::from_millis(15)).await;
            drop(guard);
        }));
    }

    for h in handles {
        h.await.unwrap();
    }

    assert_eq!(sem.active_count(), 0);
    assert!(sem.peak_count() <= 4);
    assert!(sem.peak_count() >= 2);
    assert_eq!(sem.total_acquired(), 32);

    // Cancelled/dropped future holding a permit must release it cleanly
    sem.reset_metrics();
    let sem_for_cancel = sem.clone();
    let held_task = tokio::spawn(async move {
        let _guard = sem_for_cancel.acquire().await.unwrap();
        tokio::time::sleep(Duration::from_secs(30)).await;
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!(sem.active_count(), 1);
    held_task.abort();
    let _ = held_task.await;
    assert_eq!(sem.active_count(), 0);

    // All 4 permits are available again
    let g1 = sem.acquire().await.unwrap();
    let g2 = sem.acquire().await.unwrap();
    let g3 = sem.acquire().await.unwrap();
    let g4 = sem.acquire().await.unwrap();
    assert_eq!(sem.active_count(), 4);
    drop((g1, g2, g3, g4));
    assert_eq!(sem.active_count(), 0);
}
