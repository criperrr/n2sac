//! In-memory realtime hub for Class Rooms (SSE fan-out, zero-DB idle revision cache, typing).

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use moka::future::Cache;
use serde::Serialize;
use tokio::sync::broadcast;

pub const ROOM_TYPING_TTL_SECS: u64 = 6;
const ROOM_BROADCAST_CAPACITY: usize = 128;

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ClassroomStreamEvent {
    RevBump {
        channel: String,
        rev: i64,
    },
    Typing {
        channel: String,
        account_id: i64,
        label: String,
    },
    RepresentativesChanged {
        room_id: i64,
    },
    AppearanceChanged {
        room_id: i64,
    },
}

#[derive(Clone)]
pub struct ClassroomRealtimeHub {
    channels: Arc<RwLock<HashMap<i64, broadcast::Sender<ClassroomStreamEvent>>>>,
    latest_rev: Arc<RwLock<HashMap<(i64, &'static str), i64>>>,
    /// `(room_id, channel_kind, account_id) -> display_label` with 6s TTL.
    typing: Cache<(i64, &'static str, i64), String>,
}

impl Default for ClassroomRealtimeHub {
    fn default() -> Self {
        Self::new()
    }
}

impl ClassroomRealtimeHub {
    pub fn new() -> Self {
        let typing = Cache::builder()
            .max_capacity(4_000)
            .time_to_live(Duration::from_secs(ROOM_TYPING_TTL_SECS))
            .build();
        Self {
            channels: Arc::new(RwLock::new(HashMap::new())),
            latest_rev: Arc::new(RwLock::new(HashMap::new())),
            typing,
        }
    }

    pub fn subscribe(&self, room_id: i64) -> broadcast::Receiver<ClassroomStreamEvent> {
        if let Ok(guard) = self.channels.read()
            && let Some(tx) = guard.get(&room_id)
        {
            return tx.subscribe();
        }
        if let Ok(mut guard) = self.channels.write() {
            let tx = guard
                .entry(room_id)
                .or_insert_with(|| broadcast::channel(ROOM_BROADCAST_CAPACITY).0);
            return tx.subscribe();
        }
        broadcast::channel(1).1
    }

    pub fn publish(&self, room_id: i64, event: ClassroomStreamEvent) {
        if let Ok(guard) = self.channels.read()
            && let Some(tx) = guard.get(&room_id)
        {
            let _ = tx.send(event);
        }
    }

    pub fn record_rev(&self, room_id: i64, channel: &'static str, rev: i64) {
        if let Ok(mut guard) = self.latest_rev.write() {
            let entry = guard.entry((room_id, channel)).or_insert(0);
            if rev > *entry {
                *entry = rev;
            }
        }
    }

    pub fn cached_rev(&self, room_id: i64, channel: &'static str) -> Option<i64> {
        self.latest_rev
            .read()
            .ok()
            .and_then(|g| g.get(&(room_id, channel)).copied())
    }

    pub fn bump_and_notify(&self, room_id: i64, channel: &'static str, rev: i64) {
        self.record_rev(room_id, channel, rev);
        self.publish(
            room_id,
            ClassroomStreamEvent::RevBump {
                channel: channel.to_string(),
                rev,
            },
        );
    }

    pub async fn set_typing(
        &self,
        room_id: i64,
        channel: &'static str,
        account_id: i64,
        label: String,
    ) {
        self.typing
            .insert((room_id, channel, account_id), label.clone())
            .await;
        self.publish(
            room_id,
            ClassroomStreamEvent::Typing {
                channel: channel.to_string(),
                account_id,
                label,
            },
        );
    }

    pub async fn clear_typing(&self, room_id: i64, channel: &'static str, account_id: i64) {
        self.typing.invalidate(&(room_id, channel, account_id)).await;
    }

    pub async fn typing_users(
        &self,
        room_id: i64,
        channel: &'static str,
        exclude_account_id: i64,
    ) -> Vec<String> {
        self.typing.run_pending_tasks().await;
        let mut names = Vec::new();
        for (key, label) in self.typing.iter() {
            if key.0 == room_id && key.1 == channel && key.2 != exclude_account_id {
                names.push(label);
            }
        }
        names.sort();
        names.dedup();
        names
    }
}
