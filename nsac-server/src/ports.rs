use std::sync::Arc;

use chrono::{DateTime, Utc};

use crate::db::SentinelRepo;
use crate::error::ServerError;

pub trait Clock: Send + Sync {
    fn now(&self) -> DateTime<Utc>;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

#[async_trait::async_trait]
pub trait HeuristicObserver: Send + Sync {
    async fn initialize_class_on_login(
        &self,
        _class_code: &str,
        _class_hash: &str,
        _account_id: i64,
        _now: DateTime<Utc>,
    ) -> Result<(), ServerError> {
        Ok(())
    }

    async fn record_api_activity(
        &self,
        class_code: &str,
        now: DateTime<Utc>,
    ) -> Result<u64, ServerError>;

    async fn get_poll_hint_secs(&self, class_code: Option<&str>, now: DateTime<Utc>) -> u64;
}

#[derive(Debug, Clone)]
pub struct NoopHeuristicObserver {
    default_hint_secs: u64,
}

impl NoopHeuristicObserver {
    pub const fn new(default_hint_secs: u64) -> Self {
        Self { default_hint_secs }
    }
}

#[async_trait::async_trait]
impl HeuristicObserver for NoopHeuristicObserver {
    async fn record_api_activity(
        &self,
        _class_code: &str,
        _now: DateTime<Utc>,
    ) -> Result<u64, ServerError> {
        Ok(self.default_hint_secs)
    }

    async fn get_poll_hint_secs(&self, _class_code: Option<&str>, _now: DateTime<Utc>) -> u64 {
        self.default_hint_secs
    }
}

#[derive(Debug, Clone)]
pub struct DbHeuristicHintStub {
    sentinel_repo: SentinelRepo,
    default_hint_secs: u64,
}

impl DbHeuristicHintStub {
    pub fn new(sentinel_repo: SentinelRepo, default_hint_secs: u64) -> Self {
        Self {
            sentinel_repo,
            default_hint_secs,
        }
    }

    pub fn arc(sentinel_repo: SentinelRepo, default_hint_secs: u64) -> Arc<Self> {
        Arc::new(Self::new(sentinel_repo, default_hint_secs))
    }
}

#[async_trait::async_trait]
impl HeuristicObserver for DbHeuristicHintStub {
    async fn initialize_class_on_login(
        &self,
        class_code: &str,
        class_hash: &str,
        account_id: i64,
        now: DateTime<Utc>,
    ) -> Result<(), ServerError> {
        let baseline = i64::try_from(self.default_hint_secs)
            .unwrap_or(86_400)
            .max(1);
        self.sentinel_repo
            .initialize_from_login(class_code, class_hash, account_id, baseline, now)
            .await?;
        Ok(())
    }

    async fn record_api_activity(
        &self,
        class_code: &str,
        now: DateTime<Utc>,
    ) -> Result<u64, ServerError> {
        Ok(self.get_poll_hint_secs(Some(class_code), now).await)
    }

    async fn get_poll_hint_secs(&self, class_code: Option<&str>, now: DateTime<Utc>) -> u64 {
        let Some(code) = class_code else {
            return self.default_hint_secs;
        };

        match self.sentinel_repo.get_class_state(code).await {
            Ok(Some(state)) => {
                let remaining = (state.next_check_at - now).num_seconds();
                if remaining <= 0 {
                    0
                } else {
                    u64::try_from(remaining).unwrap_or(self.default_hint_secs)
                }
            }
            Ok(None) | Err(_) => self.default_hint_secs,
        }
    }
}
