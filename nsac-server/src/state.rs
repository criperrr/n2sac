use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

use chrono::{DateTime, Utc};
use moka::future::Cache;
use nsac_core::client::NsacClient;
use nsac_core::crypto::CryptoService;
use nsac_core::models::AllYearsResponse;
use sqlx::PgPool;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::config::{CacheConfig, Config};
use crate::db::{AccountRepo, AuthRepo, BoletimRepo, RankingRepo, SentinelRepo, SnapshotRepo, SocialRepo};
use crate::error::ServerError;
use crate::fcm::FcmClient;
use crate::ports::{Clock, HeuristicObserver, SystemClock};
use crate::realtime::ClassroomRealtimeHub;
use crate::sentinel::SentinelHeuristic;

#[derive(Clone)]
pub struct NsacSemaphore {
    inner: Arc<Semaphore>,
    max_permits: usize,
    active_permits: Arc<AtomicUsize>,
    peak_permits: Arc<AtomicUsize>,
    total_acquired: Arc<AtomicU64>,
}

pub struct NsacPermitGuard {
    _permit: OwnedSemaphorePermit,
    active_permits: Arc<AtomicUsize>,
}

impl Drop for NsacPermitGuard {
    fn drop(&mut self) {
        self.active_permits.fetch_sub(1, Ordering::AcqRel);
    }
}

impl NsacSemaphore {
    pub fn new(max_permits: usize) -> Self {
        let bounded = max_permits.max(1);
        Self {
            inner: Arc::new(Semaphore::new(bounded)),
            max_permits: bounded,
            active_permits: Arc::new(AtomicUsize::new(0)),
            peak_permits: Arc::new(AtomicUsize::new(0)),
            total_acquired: Arc::new(AtomicU64::new(0)),
        }
    }

    pub async fn acquire(&self) -> Result<NsacPermitGuard, ServerError> {
        let permit = self.inner.clone().acquire_owned().await?;
        let current = self.active_permits.fetch_add(1, Ordering::AcqRel) + 1;
        self.peak_permits.fetch_max(current, Ordering::AcqRel);
        self.total_acquired.fetch_add(1, Ordering::Relaxed);

        Ok(NsacPermitGuard {
            _permit: permit,
            active_permits: Arc::clone(&self.active_permits),
        })
    }

    pub const fn max_permits(&self) -> usize {
        self.max_permits
    }

    pub fn active_count(&self) -> usize {
        self.active_permits.load(Ordering::Acquire)
    }

    pub fn peak_count(&self) -> usize {
        self.peak_permits.load(Ordering::Acquire)
    }

    pub fn total_acquired(&self) -> u64 {
        self.total_acquired.load(Ordering::Relaxed)
    }

    pub fn reset_metrics(&self) {
        self.peak_permits
            .store(self.active_count(), Ordering::Release);
        self.total_acquired.store(0, Ordering::Release);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RouteKey {
    Home,
    Atrasos,
    Saidas,
    FichaList,
    FichaDetail(u8),
}

impl RouteKey {
    pub fn as_str(&self) -> String {
        match self {
            Self::Home => "home".to_string(),
            Self::Atrasos => "atrasos".to_string(),
            Self::Saidas => "saidas".to_string(),
            Self::FichaList => "ficha_list".to_string(),
            Self::FichaDetail(id) => format!("ficha_detail_{id}"),
        }
    }

    pub fn all_known_keys() -> [&'static str; 8] {
        [
            "home",
            "atrasos",
            "saidas",
            "ficha_list",
            "ficha_detail_1",
            "ficha_detail_2",
            "ficha_detail_3",
            "ficha_detail_4",
        ]
    }
}

#[derive(Debug, Clone)]
pub struct CachedBoletim {
    pub data: Arc<AllYearsResponse>,
    pub etag: String,
    pub cached_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct CachedSnapshot {
    pub payload_json: serde_json::Value,
    pub etag: String,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct CachedUserSession {
    pub session_id: i64,
    pub account_id: i64,
    pub expires_at: DateTime<Utc>,
    pub account_active: bool,
}

#[derive(Debug, Clone)]
pub struct CachedServiceClient {
    pub id: i64,
    pub name: String,
    pub scopes: Vec<String>,
    pub active: bool,
}

/// How long a typing ping keeps the "digitando…" indicator on.
pub const CHAT_TYPING_TTL_SECS: u64 = 6;

#[derive(Clone)]
pub struct AppCaches {
    pub boletim: Cache<i64, CachedBoletim>,
    pub snapshots: Cache<(i64, String), CachedSnapshot>,
    pub user_sessions: Cache<String, CachedUserSession>,
    pub service_clients: Cache<String, CachedServiceClient>,
    /// `(typer, peer)` pairs that sent a chat typing ping in the last few seconds.
    pub chat_typing: Cache<(i64, i64), ()>,
    /// Cached per-year ranking datasets (`school_year -> Arc<YearRankingDataset>`).
    pub ranking_datasets: Cache<i32, Arc<crate::db::ranking::YearRankingDataset>>,
    /// Cached per-account ranking preferences (`account_id -> RankingPreferences`).
    pub ranking_prefs: Cache<i64, crate::db::ranking::RankingPreferences>,
    auth_cache_enabled: bool,
}

impl AppCaches {
    pub fn new(config: CacheConfig) -> Self {
        let boletim = Cache::builder()
            .max_capacity(2_000)
            .time_to_live(Duration::from_secs(config.boletim_ttl_secs))
            .build();

        let snapshots = Cache::builder()
            .max_capacity(10_000)
            .time_to_live(Duration::from_secs(config.snapshot_ttl_secs))
            .support_invalidation_closures()
            .build();

        let auth_ttl = config.auth_ttl_secs;
        let user_sessions = Cache::builder()
            .max_capacity(4_000)
            .time_to_live(Duration::from_secs(auth_ttl))
            .support_invalidation_closures()
            .build();

        let service_clients = Cache::builder()
            .max_capacity(512)
            .time_to_live(Duration::from_secs(auth_ttl))
            .build();

        let chat_typing = Cache::builder()
            .max_capacity(4_000)
            .time_to_live(Duration::from_secs(CHAT_TYPING_TTL_SECS))
            .build();

        let ranking_datasets = Cache::builder()
            .max_capacity(8)
            .time_to_live(Duration::from_secs(300))
            .build();

        let ranking_prefs = Cache::builder()
            .max_capacity(2_000)
            .time_to_live(Duration::from_secs(300))
            .build();

        Self {
            boletim,
            snapshots,
            user_sessions,
            service_clients,
            chat_typing,
            ranking_datasets,
            ranking_prefs,
            auth_cache_enabled: auth_ttl > 0,
        }
    }

    pub const fn auth_cache_enabled(&self) -> bool {
        self.auth_cache_enabled
    }

    pub async fn get_user_session(&self, token_hash: &str) -> Option<CachedUserSession> {
        if !self.auth_cache_enabled {
            return None;
        }
        self.user_sessions.get(token_hash).await
    }

    pub async fn put_user_session(&self, token_hash: String, session: CachedUserSession) {
        if self.auth_cache_enabled {
            self.user_sessions.insert(token_hash, session).await;
        }
    }

    pub async fn get_service_client(&self, key_hash: &str) -> Option<CachedServiceClient> {
        if !self.auth_cache_enabled {
            return None;
        }
        self.service_clients.get(key_hash).await
    }

    pub async fn put_service_client(&self, key_hash: String, client: CachedServiceClient) {
        if self.auth_cache_enabled {
            self.service_clients.insert(key_hash, client).await;
        }
    }

    pub async fn invalidate_boletim(&self, account_id: i64) {
        self.boletim.invalidate(&account_id).await;
    }

    pub async fn invalidate_snapshot(&self, account_id: i64, route_key: &str) {
        self.snapshots
            .invalidate(&(account_id, route_key.to_string()))
            .await;
    }

    pub async fn invalidate_snapshots_for_account(&self, account_id: i64) {
        for key in RouteKey::all_known_keys() {
            self.snapshots
                .invalidate(&(account_id, key.to_string()))
                .await;
        }
        let _ = self
            .snapshots
            .invalidate_entries_if(move |(aid, _), _| *aid == account_id);
    }

    pub async fn invalidate_user_session(&self, token_hash: &str) {
        self.user_sessions.invalidate(token_hash).await;
    }

    pub fn invalidate_user_sessions_for_account(&self, account_id: i64) {
        let _ = self
            .user_sessions
            .invalidate_entries_if(move |_, session| session.account_id == account_id);
    }

    pub async fn invalidate_service_client_by_hash(&self, key_hash: &str) {
        self.service_clients.invalidate(key_hash).await;
    }

    pub async fn invalidate_ranking_year(&self, school_year: i32) {
        self.ranking_datasets.invalidate(&school_year).await;
    }

    pub async fn invalidate_all_rankings(&self) {
        self.ranking_datasets.invalidate_all();
    }

    pub async fn invalidate_ranking_prefs(&self, account_id: i64) {
        self.ranking_prefs.invalidate(&account_id).await;
    }

    pub async fn invalidate_account_all(&self, account_id: i64) {
        self.invalidate_boletim(account_id).await;
        self.invalidate_snapshots_for_account(account_id).await;
        self.invalidate_user_sessions_for_account(account_id);
        self.invalidate_ranking_prefs(account_id).await;
    }
}

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub db: PgPool,
    pub crypto: CryptoService,
    pub nsac: NsacClient,
    pub fcm: FcmClient,
    pub clock: Arc<dyn Clock>,
    pub heuristic: Arc<dyn HeuristicObserver>,
    pub caches: AppCaches,
    pub semaphore: NsacSemaphore,
    pub realtime: ClassroomRealtimeHub,
}

impl AppState {
    pub fn new(
        config: Config,
        db: PgPool,
        heuristic: Arc<dyn HeuristicObserver>,
        clock: Arc<dyn Clock>,
    ) -> Result<Self, ServerError> {
        let crypto = CryptoService::from_hex_key(&config.encryption_key)?;
        let nsac = NsacClient::with_timeout(
            &config.nsac_base_url,
            Duration::from_secs(config.nsac_timeout_secs),
        )?;
        let fcm = FcmClient::new(config.fcm.clone())?;
        let caches = AppCaches::new(config.cache);
        let semaphore = NsacSemaphore::new(config.nsac_max_concurrent_requests);

        Ok(Self {
            config: Arc::new(config),
            db,
            crypto,
            nsac,
            fcm,
            clock,
            heuristic,
            caches,
            semaphore,
            realtime: ClassroomRealtimeHub::new(),
        })
    }

    pub fn with_default_ports(config: Config, db: PgPool) -> Result<Self, ServerError> {
        let sentinel_repo = SentinelRepo::new(db.clone());
        let heuristic: Arc<dyn HeuristicObserver> =
            SentinelHeuristic::arc(sentinel_repo, config.sentinel.clone());
        let clock: Arc<dyn Clock> = Arc::new(SystemClock);
        Self::new(config, db, heuristic, clock)
    }

    pub fn accounts(&self) -> AccountRepo {
        AccountRepo::new(self.db.clone())
    }

    pub fn boletim(&self) -> BoletimRepo {
        BoletimRepo::new(self.db.clone())
    }

    pub fn auth(&self) -> AuthRepo {
        AuthRepo::new(self.db.clone())
    }

    pub fn snapshots(&self) -> SnapshotRepo {
        SnapshotRepo::new(self.db.clone())
    }

    pub fn sentinel(&self) -> SentinelRepo {
        SentinelRepo::new(self.db.clone())
    }

    pub fn ranking(&self) -> RankingRepo {
        RankingRepo::new(self.db.clone())
    }

    pub fn social(&self) -> SocialRepo {
        SocialRepo::new(self.db.clone())
    }

    pub fn moderation(&self) -> crate::db::ModerationRepo {
        crate::db::ModerationRepo::new(self.db.clone())
    }

    pub fn feedback(&self) -> crate::db::FeedbackRepo {
        crate::db::FeedbackRepo::new(self.db.clone())
    }

    pub fn classrooms(&self) -> crate::db::classrooms::ClassroomsRepo {
        crate::db::classrooms::ClassroomsRepo::new(self.db.clone())
    }

    pub fn connections(&self) -> crate::db::ConnectionRepo {
        crate::db::ConnectionRepo::new(self.db.clone())
    }
}
