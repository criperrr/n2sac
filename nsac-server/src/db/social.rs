use std::collections::HashSet;

use chrono::{DateTime, Utc};
use nsac_core::crypto::CryptoService;
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, PgPool};

use crate::db::profiles::Visibility;
use crate::db::ranking::{avatar_path, format_student_label};
use crate::error::ServerError;

pub const MAX_MESSAGE_CHARS: usize = 500;
pub const MESSAGE_RATE_LIMIT_COUNT: i64 = 10;
pub const MESSAGE_RATE_LIMIT_WINDOW_SECS: i64 = 180;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FollowEdgeStatus {
    None,
    Pending,
    Accepted,
}

impl FollowEdgeStatus {
    fn from_db(opt: Option<&str>) -> Self {
        match opt {
            Some("accepted") => Self::Accepted,
            Some("pending") => Self::Pending,
            _ => Self::None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelationshipStatus {
    pub outgoing: FollowEdgeStatus,
    pub incoming: FollowEdgeStatus,
    pub is_friend: bool,
    pub blocked_by_me: bool,
    pub blocked_me: bool,
}

impl RelationshipStatus {
    pub fn none() -> Self {
        Self {
            outgoing: FollowEdgeStatus::None,
            incoming: FollowEdgeStatus::None,
            is_friend: false,
            blocked_by_me: false,
            blocked_me: false,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SocialUserSummary {
    pub account_id: i64,
    pub display_label: String,
    pub class_code: String,
    pub avatar_url: Option<String>,
    pub has_public_profile: bool,
    pub is_friend: bool,
    pub outgoing: FollowEdgeStatus,
    pub incoming: FollowEdgeStatus,
    pub since: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConversationSummary {
    pub peer: SocialUserSummary,
    pub is_friend: bool,
    pub last_message_id: Option<i64>,
    pub last_message_preview: Option<String>,
    pub last_message_sender_id: Option<i64>,
    pub last_message_at: Option<DateTime<Utc>>,
    pub unread_count: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct SocialOverview {
    pub friends_count: usize,
    pub followers_count: usize,
    pub following_count: usize,
    pub pending_requests_count: usize,
    pub unread_messages_count: i64,
    pub friends: Vec<SocialUserSummary>,
    pub followers: Vec<SocialUserSummary>,
    pub following: Vec<SocialUserSummary>,
    pub incoming_requests: Vec<SocialUserSummary>,
    pub outgoing_requests: Vec<SocialUserSummary>,
    pub blocked: Vec<SocialUserSummary>,
    pub conversations: Vec<ConversationSummary>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FollowMutationResult {
    pub relationship: RelationshipStatus,
    pub became_friends: bool,
    pub status: FollowEdgeStatus,
    pub peer_account_id: i64,
    pub peer_label: String,
    pub peer_avatar_url: Option<String>,
    pub peer_class_code: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChatMessage {
    pub id: i64,
    pub sender_id: i64,
    pub is_me: bool,
    pub text: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChatRateLimitStatus {
    pub limit: i64,
    pub window_secs: i64,
    pub remaining: i64,
    pub retry_after_secs: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConversationMessagesPayload {
    pub peer: SocialUserSummary,
    pub is_friend: bool,
    pub blocked: bool,
    pub messages: Vec<ChatMessage>,
    pub rate_limit: ChatRateLimitStatus,
}

#[derive(Debug, Clone, Serialize)]
pub struct SendMessagePayload {
    pub message: ChatMessage,
    pub rate_limit: ChatRateLimitStatus,
}

#[derive(Debug, Clone, Serialize)]
pub struct FollowCounts {
    pub followers_count: i64,
    pub following_count: i64,
}

#[derive(FromRow)]
struct AccountSummarySqlRow {
    account_id: i64,
    nsac_email: String,
    class_code: Option<String>,
    ranking_display_name: Option<String>,
    opted_in: Option<bool>,
    hide_name: Option<bool>,
    show_avatar: Option<bool>,
    public_profile: Option<bool>,
    discoverable: Option<bool>,
    invisible: Option<bool>,
    avatar_version: Option<i64>,
    since: Option<DateTime<Utc>>,
}

impl AccountSummarySqlRow {
    fn visibility(&self) -> Visibility {
        Visibility {
            opted_in: self.opted_in.unwrap_or(false),
            hide_name: self.hide_name.unwrap_or(false),
            show_avatar: self.show_avatar.unwrap_or(false),
            public_profile: self.public_profile.unwrap_or(false),
            discoverable: self.discoverable.unwrap_or(false),
            invisible: self.invisible.unwrap_or(false),
        }
    }

    fn to_summary(
        &self,
        outgoing: FollowEdgeStatus,
        incoming: FollowEdgeStatus,
    ) -> SocialUserSummary {
        let vis = self.visibility();
        let is_friend =
            outgoing == FollowEdgeStatus::Accepted && incoming == FollowEdgeStatus::Accepted;
        let show_avatar = is_friend || vis.avatar_visible();
        SocialUserSummary {
            account_id: self.account_id,
            display_label: format_student_label(
                self.account_id,
                &self.nsac_email,
                false,
                self.ranking_display_name.as_deref(),
                false,
            ),
            class_code: self
                .class_code
                .clone()
                .unwrap_or_else(|| "CTI".to_string()),
            avatar_url: show_avatar
                .then_some(self.avatar_version)
                .flatten()
                .map(|v| avatar_path(self.account_id, v)),
            has_public_profile: is_friend || vis.profile_visible(),
            is_friend,
            outgoing,
            incoming,
            since: self.since,
        }
    }
}

#[derive(FromRow)]
struct MessageSqlRow {
    id: i64,
    sender_id: i64,
    body_ciphertext: String,
    created_at: DateTime<Utc>,
}

#[derive(FromRow)]
struct PairRow {
    peer_id: i64,
}

#[derive(FromRow)]
struct FollowEdgeRow {
    follower_id: i64,
    followee_id: i64,
    status: String,
}

#[derive(Clone)]
pub struct SocialRepo {
    pool: PgPool,
}

impl SocialRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn is_blocked_between(&self, a: i64, b: i64) -> Result<bool, ServerError> {
        if a == b {
            return Ok(false);
        }
        let exists: bool = sqlx::query_scalar(
            r#"
            SELECT EXISTS (
                SELECT 1 FROM social_blocks
                WHERE (blocker_id = $1 AND blocked_id = $2)
                   OR (blocker_id = $2 AND blocked_id = $1)
            )
            "#,
        )
        .bind(a)
        .bind(b)
        .fetch_one(&self.pool)
        .await?;
        Ok(exists)
    }

    pub async fn blocked_peer_ids(&self, viewer: i64) -> Result<HashSet<i64>, ServerError> {
        let ids: Vec<i64> = sqlx::query_scalar(
            r#"
            SELECT blocked_id FROM social_blocks WHERE blocker_id = $1
            UNION
            SELECT blocker_id FROM social_blocks WHERE blocked_id = $1
            "#,
        )
        .bind(viewer)
        .fetch_all(&self.pool)
        .await?;
        Ok(ids.into_iter().collect())
    }

    pub async fn follow_counts(&self, account_id: i64) -> Result<FollowCounts, ServerError> {
        let (followers_count, following_count): (i64, i64) = sqlx::query_as(
            r#"
            SELECT
                (SELECT COUNT(*)::bigint FROM social_follows WHERE followee_id = $1 AND status = 'accepted'),
                (SELECT COUNT(*)::bigint FROM social_follows WHERE follower_id = $1 AND status = 'accepted')
            "#,
        )
        .bind(account_id)
        .fetch_one(&self.pool)
        .await?;
        Ok(FollowCounts {
            followers_count,
            following_count,
        })
    }

    pub async fn relationship(
        &self,
        viewer: i64,
        other: i64,
    ) -> Result<RelationshipStatus, ServerError> {
        if viewer == other {
            return Ok(RelationshipStatus::none());
        }
        let (outgoing_raw, incoming_raw, blocked_by_me, blocked_me): (
            Option<String>,
            Option<String>,
            bool,
            bool,
        ) = sqlx::query_as(
            r#"
            SELECT
                (SELECT status FROM social_follows WHERE follower_id = $1 AND followee_id = $2),
                (SELECT status FROM social_follows WHERE follower_id = $2 AND followee_id = $1),
                EXISTS (SELECT 1 FROM social_blocks WHERE blocker_id = $1 AND blocked_id = $2),
                EXISTS (SELECT 1 FROM social_blocks WHERE blocker_id = $2 AND blocked_id = $1)
            "#,
        )
        .bind(viewer)
        .bind(other)
        .fetch_one(&self.pool)
        .await?;

        let outgoing = FollowEdgeStatus::from_db(outgoing_raw.as_deref());
        let incoming = FollowEdgeStatus::from_db(incoming_raw.as_deref());
        let is_friend = !blocked_by_me
            && !blocked_me
            && outgoing == FollowEdgeStatus::Accepted
            && incoming == FollowEdgeStatus::Accepted;

        Ok(RelationshipStatus {
            outgoing,
            incoming,
            is_friend,
            blocked_by_me,
            blocked_me,
        })
    }

    pub async fn relationship_map_for(
        &self,
        viewer: i64,
    ) -> Result<(std::collections::HashMap<i64, FollowEdgeStatus>, std::collections::HashMap<i64, FollowEdgeStatus>), ServerError> {
        let edges = sqlx::query_as::<_, FollowEdgeRow>(
            r#"
            SELECT follower_id, followee_id, status
            FROM social_follows
            WHERE follower_id = $1 OR followee_id = $1
            "#,
        )
        .bind(viewer)
        .fetch_all(&self.pool)
        .await?;

        let mut outgoing = std::collections::HashMap::new();
        let mut incoming = std::collections::HashMap::new();
        for e in edges {
            let st = FollowEdgeStatus::from_db(Some(e.status.as_str()));
            if e.follower_id == viewer {
                outgoing.insert(e.followee_id, st);
            } else {
                incoming.insert(e.follower_id, st);
            }
        }
        Ok((outgoing, incoming))
    }

    pub async fn account_label(&self, account_id: i64) -> Result<String, ServerError> {
        let row = self.fetch_user_row(account_id).await?;
        Ok(match row {
            Some(r) => format_student_label(
                r.account_id,
                &r.nsac_email,
                false,
                r.ranking_display_name.as_deref(),
                false,
            ),
            None => format!("Aluno #{account_id}"),
        })
    }

    pub async fn account_fcm_token(&self, account_id: i64) -> Result<Option<String>, ServerError> {
        let token: Option<Option<String>> = sqlx::query_scalar(
            "SELECT fcm_token FROM accounts WHERE id = $1 AND active = TRUE",
        )
        .bind(account_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(token.flatten().filter(|s| !s.trim().is_empty()))
    }

    async fn fetch_user_row(
        &self,
        account_id: i64,
    ) -> Result<Option<AccountSummarySqlRow>, ServerError> {
        Ok(sqlx::query_as::<_, AccountSummarySqlRow>(
            r#"
            SELECT
                a.id AS account_id,
                a.nsac_email,
                a.class_code,
                COALESCE(NULLIF(TRIM(rp.display_name), ''), NULLIF(TRIM(ap.display_name), '')) AS ranking_display_name,
                rp.opted_in,
                rp.hide_name,
                rp.show_avatar,
                rp.public_profile,
                rp.discoverable,
                rp.invisible,
                CASE WHEN ap.avatar_data_url IS NOT NULL
                     THEN EXTRACT(EPOCH FROM ap.updated_at)::bigint END AS avatar_version,
                NULL::timestamptz AS since
            FROM accounts a
            LEFT JOIN account_ranking_preferences rp ON rp.account_id = a.id
            LEFT JOIN account_profiles ap ON ap.account_id = a.id
            WHERE a.id = $1 AND a.active = TRUE
            "#,
        )
        .bind(account_id)
        .fetch_optional(&self.pool)
        .await?)
    }

    pub async fn follow(
        &self,
        follower_id: i64,
        followee_id: i64,
    ) -> Result<FollowMutationResult, ServerError> {
        if follower_id == followee_id {
            return Err(ServerError::invalid_params("Você não pode seguir a si mesmo."));
        }

        let Some(target_row) = self.fetch_user_row(followee_id).await? else {
            return Err(ServerError::not_found("Account not found"));
        };
        let target_vis = target_row.visibility();
        if target_vis.invisible {
            return Err(ServerError::not_found("Account unavailable"));
        }

        let before = self.relationship(follower_id, followee_id).await?;
        if before.blocked_by_me || before.blocked_me {
            return Err(ServerError::with_user_message(
                nsac_core::error::ErrorCode::Forbidden,
                "Não é possível seguir este aluno devido a um bloqueio.",
                "Follow blocked between accounts",
            ));
        }

        // Users who are neither findable nor already following the requester cannot be targeted.
        if !target_vis.findable() && before.incoming == FollowEdgeStatus::None {
            return Err(ServerError::not_found("Account unavailable"));
        }

        // Named in the ranking -> immediate follow; outside the ranking or anonymous -> requires approval.
        let desired_status = decide_follow_status(target_vis, before.outgoing);
        let status_str = match desired_status {
            FollowEdgeStatus::Accepted => "accepted",
            FollowEdgeStatus::Pending => "pending",
            FollowEdgeStatus::None => "pending",
        };

        sqlx::query(
            r#"
            INSERT INTO social_follows (follower_id, followee_id, status, created_at, updated_at)
            VALUES ($1, $2, $3, NOW(), NOW())
            ON CONFLICT (follower_id, followee_id) DO UPDATE SET
                status = CASE
                    WHEN social_follows.status = 'accepted' THEN 'accepted'
                    ELSE EXCLUDED.status
                END,
                updated_at = NOW()
            "#,
        )
        .bind(follower_id)
        .bind(followee_id)
        .bind(status_str)
        .execute(&self.pool)
        .await?;

        let after = self.relationship(follower_id, followee_id).await?;
        let became_friends = !before.is_friend && after.is_friend;
        let summary = target_row.to_summary(after.outgoing, after.incoming);

        Ok(FollowMutationResult {
            relationship: after.clone(),
            became_friends,
            status: after.outgoing,
            peer_account_id: followee_id,
            peer_label: summary.display_label,
            peer_avatar_url: summary.avatar_url,
            peer_class_code: summary.class_code,
        })
    }

    pub async fn unfollow(
        &self,
        follower_id: i64,
        followee_id: i64,
    ) -> Result<RelationshipStatus, ServerError> {
        sqlx::query("DELETE FROM social_follows WHERE follower_id = $1 AND followee_id = $2")
            .bind(follower_id)
            .bind(followee_id)
            .execute(&self.pool)
            .await?;
        self.relationship(follower_id, followee_id).await
    }

    pub async fn accept_request(
        &self,
        accepter_id: i64,
        requester_id: i64,
    ) -> Result<FollowMutationResult, ServerError> {
        let before = self.relationship(accepter_id, requester_id).await?;
        if before.blocked_by_me || before.blocked_me {
            return Err(ServerError::with_user_message(
                nsac_core::error::ErrorCode::Forbidden,
                "Não é possível aceitar o pedido devido a um bloqueio.",
                "Accept request blocked",
            ));
        }

        let res = sqlx::query(
            r#"
            UPDATE social_follows
            SET status = 'accepted', updated_at = NOW()
            WHERE follower_id = $1 AND followee_id = $2 AND status = 'pending'
            "#,
        )
        .bind(requester_id)
        .bind(accepter_id)
        .execute(&self.pool)
        .await?;

        if res.rows_affected() == 0 && before.incoming != FollowEdgeStatus::Accepted {
            return Err(ServerError::not_found("Follow request not found"));
        }

        let after = self.relationship(accepter_id, requester_id).await?;
        let became_friends = !before.is_friend && after.is_friend;
        let target_row = self
            .fetch_user_row(requester_id)
            .await?
            .ok_or_else(|| ServerError::not_found("Requester not found"))?;
        let summary = target_row.to_summary(after.outgoing, after.incoming);

        Ok(FollowMutationResult {
            relationship: after.clone(),
            became_friends,
            status: after.incoming,
            peer_account_id: requester_id,
            peer_label: summary.display_label,
            peer_avatar_url: summary.avatar_url,
            peer_class_code: summary.class_code,
        })
    }

    pub async fn decline_request(
        &self,
        decliner_id: i64,
        requester_id: i64,
    ) -> Result<RelationshipStatus, ServerError> {
        sqlx::query(
            "DELETE FROM social_follows WHERE follower_id = $1 AND followee_id = $2 AND status = 'pending'",
        )
        .bind(requester_id)
        .bind(decliner_id)
        .execute(&self.pool)
        .await?;
        self.relationship(decliner_id, requester_id).await
    }

    pub async fn block(
        &self,
        blocker_id: i64,
        blocked_id: i64,
    ) -> Result<RelationshipStatus, ServerError> {
        if blocker_id == blocked_id {
            return Err(ServerError::invalid_params("Você não pode bloquear a si mesmo."));
        }
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            r#"
            INSERT INTO social_blocks (blocker_id, blocked_id, created_at)
            VALUES ($1, $2, NOW())
            ON CONFLICT (blocker_id, blocked_id) DO NOTHING
            "#,
        )
        .bind(blocker_id)
        .bind(blocked_id)
        .execute(&mut *tx)
        .await?;

        sqlx::query(
            r#"
            DELETE FROM social_follows
            WHERE (follower_id = $1 AND followee_id = $2)
               OR (follower_id = $2 AND followee_id = $1)
            "#,
        )
        .bind(blocker_id)
        .bind(blocked_id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;

        self.relationship(blocker_id, blocked_id).await
    }

    pub async fn unblock(
        &self,
        blocker_id: i64,
        blocked_id: i64,
    ) -> Result<RelationshipStatus, ServerError> {
        sqlx::query("DELETE FROM social_blocks WHERE blocker_id = $1 AND blocked_id = $2")
            .bind(blocker_id)
            .bind(blocked_id)
            .execute(&self.pool)
            .await?;
        self.relationship(blocker_id, blocked_id).await
    }

    pub async fn overview(
        &self,
        viewer: i64,
        crypto: &CryptoService,
    ) -> Result<SocialOverview, ServerError> {
        let fetch_list = |sql: &'static str| {
            sqlx::query_as::<_, AccountSummarySqlRow>(sql)
                .bind(viewer)
                .fetch_all(&self.pool)
        };

        let (
            (out_map, in_map),
            blocked_set,
            followers_rows,
            following_rows,
            incoming_req_rows,
            outgoing_req_rows,
            blocked_rows,
        ) = tokio::try_join!(
            self.relationship_map_for(viewer),
            self.blocked_peer_ids(viewer),
            async {
                fetch_list(
                    r#"
                    SELECT
                        a.id AS account_id, a.nsac_email, a.class_code,
                        COALESCE(NULLIF(TRIM(rp.display_name), ''), NULLIF(TRIM(ap.display_name), '')) AS ranking_display_name,
                        rp.opted_in, rp.hide_name, rp.show_avatar, rp.public_profile, rp.discoverable, rp.invisible,
                        CASE WHEN ap.avatar_data_url IS NOT NULL THEN EXTRACT(EPOCH FROM ap.updated_at)::bigint END AS avatar_version,
                        f.updated_at AS since
                    FROM social_follows f
                    JOIN accounts a ON a.id = f.follower_id AND a.active = TRUE
                    LEFT JOIN account_ranking_preferences rp ON rp.account_id = a.id
                    LEFT JOIN account_profiles ap ON ap.account_id = a.id
                    WHERE f.followee_id = $1 AND f.status = 'accepted'
                    ORDER BY f.updated_at DESC
                    "#,
                )
                .await
                .map_err(ServerError::from)
            },
            async {
                fetch_list(
                    r#"
                    SELECT
                        a.id AS account_id, a.nsac_email, a.class_code,
                        COALESCE(NULLIF(TRIM(rp.display_name), ''), NULLIF(TRIM(ap.display_name), '')) AS ranking_display_name,
                        rp.opted_in, rp.hide_name, rp.show_avatar, rp.public_profile, rp.discoverable, rp.invisible,
                        CASE WHEN ap.avatar_data_url IS NOT NULL THEN EXTRACT(EPOCH FROM ap.updated_at)::bigint END AS avatar_version,
                        f.updated_at AS since
                    FROM social_follows f
                    JOIN accounts a ON a.id = f.followee_id AND a.active = TRUE
                    LEFT JOIN account_ranking_preferences rp ON rp.account_id = a.id
                    LEFT JOIN account_profiles ap ON ap.account_id = a.id
                    WHERE f.follower_id = $1 AND f.status = 'accepted'
                    ORDER BY f.updated_at DESC
                    "#,
                )
                .await
                .map_err(ServerError::from)
            },
            async {
                fetch_list(
                    r#"
                    SELECT
                        a.id AS account_id, a.nsac_email, a.class_code,
                        COALESCE(NULLIF(TRIM(rp.display_name), ''), NULLIF(TRIM(ap.display_name), '')) AS ranking_display_name,
                        rp.opted_in, rp.hide_name, rp.show_avatar, rp.public_profile, rp.discoverable, rp.invisible,
                        CASE WHEN ap.avatar_data_url IS NOT NULL THEN EXTRACT(EPOCH FROM ap.updated_at)::bigint END AS avatar_version,
                        f.created_at AS since
                    FROM social_follows f
                    JOIN accounts a ON a.id = f.follower_id AND a.active = TRUE
                    LEFT JOIN account_ranking_preferences rp ON rp.account_id = a.id
                    LEFT JOIN account_profiles ap ON ap.account_id = a.id
                    WHERE f.followee_id = $1 AND f.status = 'pending'
                    ORDER BY f.created_at DESC
                    "#,
                )
                .await
                .map_err(ServerError::from)
            },
            async {
                fetch_list(
                    r#"
                    SELECT
                        a.id AS account_id, a.nsac_email, a.class_code,
                        COALESCE(NULLIF(TRIM(rp.display_name), ''), NULLIF(TRIM(ap.display_name), '')) AS ranking_display_name,
                        rp.opted_in, rp.hide_name, rp.show_avatar, rp.public_profile, rp.discoverable, rp.invisible,
                        CASE WHEN ap.avatar_data_url IS NOT NULL THEN EXTRACT(EPOCH FROM ap.updated_at)::bigint END AS avatar_version,
                        f.created_at AS since
                    FROM social_follows f
                    JOIN accounts a ON a.id = f.followee_id AND a.active = TRUE
                    LEFT JOIN account_ranking_preferences rp ON rp.account_id = a.id
                    LEFT JOIN account_profiles ap ON ap.account_id = a.id
                    WHERE f.follower_id = $1 AND f.status = 'pending'
                    ORDER BY f.created_at DESC
                    "#,
                )
                .await
                .map_err(ServerError::from)
            },
            async {
                fetch_list(
                    r#"
                    SELECT
                        a.id AS account_id, a.nsac_email, a.class_code,
                        COALESCE(NULLIF(TRIM(rp.display_name), ''), NULLIF(TRIM(ap.display_name), '')) AS ranking_display_name,
                        rp.opted_in, rp.hide_name, rp.show_avatar, rp.public_profile, rp.discoverable, rp.invisible,
                        NULL::bigint AS avatar_version,
                        b.created_at AS since
                    FROM social_blocks b
                    JOIN accounts a ON a.id = b.blocked_id AND a.active = TRUE
                    LEFT JOIN account_ranking_preferences rp ON rp.account_id = a.id
                    LEFT JOIN account_profiles ap ON ap.account_id = a.id
                    WHERE b.blocker_id = $1
                    ORDER BY b.created_at DESC
                    "#,
                )
                .await
                .map_err(ServerError::from)
            },
        )?;

        let to_list = |rows: Vec<AccountSummarySqlRow>| -> Vec<SocialUserSummary> {
            rows.into_iter()
                .filter(|r| !blocked_set.contains(&r.account_id))
                .map(|r| {
                    let out_st = out_map
                        .get(&r.account_id)
                        .copied()
                        .unwrap_or(FollowEdgeStatus::None);
                    let in_st = in_map
                        .get(&r.account_id)
                        .copied()
                        .unwrap_or(FollowEdgeStatus::None);
                    r.to_summary(out_st, in_st)
                })
                .collect()
        };

        let followers = to_list(followers_rows);
        let following = to_list(following_rows);
        let incoming_requests = to_list(incoming_req_rows);
        let outgoing_requests = to_list(outgoing_req_rows);
        let friends: Vec<SocialUserSummary> =
            following.iter().filter(|u| u.is_friend).cloned().collect();
        let blocked: Vec<SocialUserSummary> = blocked_rows
            .into_iter()
            .map(|r| r.to_summary(FollowEdgeStatus::None, FollowEdgeStatus::None))
            .collect();

        let conversations = self
            .list_conversations(viewer, &friends, &blocked_set, &out_map, &in_map, crypto)
            .await?;
        let unread_messages_count: i64 = conversations.iter().map(|c| c.unread_count).sum();

        Ok(SocialOverview {
            friends_count: friends.len(),
            followers_count: followers.len(),
            following_count: following.len(),
            pending_requests_count: incoming_requests.len(),
            unread_messages_count,
            friends,
            followers,
            following,
            incoming_requests,
            outgoing_requests,
            blocked,
            conversations,
        })
    }

    async fn list_conversations(
        &self,
        viewer: i64,
        friends: &[SocialUserSummary],
        blocked_set: &HashSet<i64>,
        out_map: &std::collections::HashMap<i64, FollowEdgeStatus>,
        in_map: &std::collections::HashMap<i64, FollowEdgeStatus>,
        crypto: &CryptoService,
    ) -> Result<Vec<ConversationSummary>, ServerError> {
        let msg_peers = sqlx::query_as::<_, PairRow>(
            r#"
            SELECT DISTINCT CASE WHEN pair_low = $1 THEN pair_high ELSE pair_low END AS peer_id
            FROM social_messages
            WHERE pair_low = $1 OR pair_high = $1
            "#,
        )
        .bind(viewer)
        .fetch_all(&self.pool)
        .await?;

        let mut peer_ids: Vec<i64> = friends.iter().map(|f| f.account_id).collect();
        let mut seen: HashSet<i64> = peer_ids.iter().copied().collect();
        for p in msg_peers {
            if !blocked_set.contains(&p.peer_id) && seen.insert(p.peer_id) {
                peer_ids.push(p.peer_id);
            }
        }

        let mut list = Vec::new();

        for peer_id in peer_ids {
            let (low, high) = ordered_pair(viewer, peer_id);
            let (last_read, cleared_before): (i64, i64) = sqlx::query_as(
                r#"
                SELECT last_read_message_id, cleared_before_message_id
                FROM social_conversation_state
                WHERE account_id = $1 AND peer_id = $2
                "#,
            )
            .bind(viewer)
            .bind(peer_id)
            .fetch_optional(&self.pool)
            .await?
            .unwrap_or((0, 0));

            let last_msg = sqlx::query_as::<_, MessageSqlRow>(
                r#"
                SELECT id, sender_id, body_ciphertext, created_at
                FROM social_messages
                WHERE pair_low = $1 AND pair_high = $2 AND id > $3
                ORDER BY id DESC
                LIMIT 1
                "#,
            )
            .bind(low)
            .bind(high)
            .bind(cleared_before)
            .fetch_optional(&self.pool)
            .await?;

            let out_st = out_map
                .get(&peer_id)
                .copied()
                .unwrap_or(FollowEdgeStatus::None);
            let in_st = in_map
                .get(&peer_id)
                .copied()
                .unwrap_or(FollowEdgeStatus::None);
            let is_friend =
                out_st == FollowEdgeStatus::Accepted && in_st == FollowEdgeStatus::Accepted;

            // Former friends only remain in the inbox while they have un-cleared messages.
            if !is_friend && last_msg.is_none() {
                continue;
            }

            let peer = if let Some(f) = friends.iter().find(|f| f.account_id == peer_id) {
                f.clone()
            } else if let Some(r) = self.fetch_user_row(peer_id).await? {
                r.to_summary(out_st, in_st)
            } else {
                continue;
            };

            let unread_count: i64 = if last_msg.is_some() {
                sqlx::query_scalar(
                    r#"
                    SELECT COUNT(*)::bigint
                    FROM social_messages
                    WHERE pair_low = $1
                      AND pair_high = $2
                      AND id > $3
                      AND id > $4
                      AND sender_id <> $5
                    "#,
                )
                .bind(low)
                .bind(high)
                .bind(cleared_before)
                .bind(last_read)
                .bind(viewer)
                .fetch_one(&self.pool)
                .await?
            } else {
                0
            };

            let (last_message_id, last_message_preview, last_message_sender_id, last_message_at) =
                match last_msg {
                    Some(m) => {
                        let preview = crypto
                            .decrypt(&m.body_ciphertext)
                            .ok()
                            .map(|s| truncate_preview(s.as_str(), 72));
                        (Some(m.id), preview, Some(m.sender_id), Some(m.created_at))
                    }
                    None => (None, None, None, None),
                };

            list.push(ConversationSummary {
                peer,
                is_friend,
                last_message_id,
                last_message_preview,
                last_message_sender_id,
                last_message_at,
                unread_count,
            });
        }

        list.sort_by(|a, b| {
            b.last_message_at
                .cmp(&a.last_message_at)
                .then_with(|| a.peer.display_label.cmp(&b.peer.display_label))
        });

        Ok(list)
    }

    pub async fn message_rate_limit_status(
        &self,
        sender_id: i64,
    ) -> Result<ChatRateLimitStatus, ServerError> {
        let timestamps: Vec<DateTime<Utc>> = sqlx::query_scalar(
            r#"
            SELECT created_at
            FROM social_messages
            WHERE sender_id = $1
              AND created_at > NOW() - ($2::bigint * INTERVAL '1 second')
            ORDER BY created_at ASC
            "#,
        )
        .bind(sender_id)
        .bind(MESSAGE_RATE_LIMIT_WINDOW_SECS)
        .fetch_all(&self.pool)
        .await?;

        let used = timestamps.len() as i64;
        let remaining = (MESSAGE_RATE_LIMIT_COUNT - used).max(0);
        let retry_after_secs = if remaining == 0 {
            timestamps
                .first()
                .map(|oldest| {
                    let elapsed = (Utc::now() - *oldest).num_seconds();
                    (MESSAGE_RATE_LIMIT_WINDOW_SECS - elapsed).max(1)
                })
                .unwrap_or(MESSAGE_RATE_LIMIT_WINDOW_SECS)
        } else {
            0
        };

        Ok(ChatRateLimitStatus {
            limit: MESSAGE_RATE_LIMIT_COUNT,
            window_secs: MESSAGE_RATE_LIMIT_WINDOW_SECS,
            remaining,
            retry_after_secs,
        })
    }

    pub async fn get_messages(
        &self,
        viewer: i64,
        peer_id: i64,
        after_id: Option<i64>,
        limit: i64,
        crypto: &CryptoService,
    ) -> Result<ConversationMessagesPayload, ServerError> {
        if viewer == peer_id {
            return Err(ServerError::invalid_params("Conversa inválida."));
        }
        let rel = self.relationship(viewer, peer_id).await?;
        let blocked = rel.blocked_by_me || rel.blocked_me;
        if blocked {
            return Err(ServerError::with_user_message(
                nsac_core::error::ErrorCode::Forbidden,
                "Esta conversa está indisponível devido a um bloqueio.",
                "Conversation blocked",
            ));
        }
        let peer_row = self
            .fetch_user_row(peer_id)
            .await?
            .ok_or_else(|| ServerError::not_found("Account not found"))?;
        let peer = peer_row.to_summary(rel.outgoing, rel.incoming);

        let (low, high) = ordered_pair(viewer, peer_id);
        let cleared_before: i64 = sqlx::query_scalar(
            r#"
            SELECT cleared_before_message_id
            FROM social_conversation_state
            WHERE account_id = $1 AND peer_id = $2
            "#,
        )
        .bind(viewer)
        .bind(peer_id)
        .fetch_optional(&self.pool)
        .await?
        .unwrap_or(0);

        let floor_id = after_id.map_or(cleared_before, |a| a.max(cleared_before));
        let capped_limit = limit.clamp(1, 100);

        let mut rows = sqlx::query_as::<_, MessageSqlRow>(
            r#"
            SELECT id, sender_id, body_ciphertext, created_at
            FROM social_messages
            WHERE pair_low = $1 AND pair_high = $2 AND id > $3
            ORDER BY id DESC
            LIMIT $4
            "#,
        )
        .bind(low)
        .bind(high)
        .bind(floor_id)
        .bind(capped_limit)
        .fetch_all(&self.pool)
        .await?;
        rows.reverse();

        if let Some(last) = rows.last() {
            sqlx::query(
                r#"
                INSERT INTO social_conversation_state (account_id, peer_id, last_read_message_id, cleared_before_message_id, updated_at)
                VALUES ($1, $2, $3, 0, NOW())
                ON CONFLICT (account_id, peer_id) DO UPDATE SET
                    last_read_message_id = GREATEST(social_conversation_state.last_read_message_id, EXCLUDED.last_read_message_id),
                    updated_at = NOW()
                "#,
            )
            .bind(viewer)
            .bind(peer_id)
            .bind(last.id)
            .execute(&self.pool)
            .await?;
        }

        let mut messages = Vec::with_capacity(rows.len());
        for r in rows {
            if let Ok(plain) = crypto.decrypt(&r.body_ciphertext) {
                messages.push(ChatMessage {
                    id: r.id,
                    sender_id: r.sender_id,
                    is_me: r.sender_id == viewer,
                    text: plain.as_str().to_string(),
                    created_at: r.created_at,
                });
            }
        }

        let rate_limit = self.message_rate_limit_status(viewer).await?;

        Ok(ConversationMessagesPayload {
            peer,
            is_friend: rel.is_friend,
            blocked,
            messages,
            rate_limit,
        })
    }

    pub async fn send_message(
        &self,
        sender_id: i64,
        peer_id: i64,
        raw_text: &str,
        crypto: &CryptoService,
    ) -> Result<SendMessagePayload, ServerError> {
        if sender_id == peer_id {
            return Err(ServerError::invalid_params("Você não pode enviar mensagem para si mesmo."));
        }

        let rel = self.relationship(sender_id, peer_id).await?;
        if rel.blocked_by_me || rel.blocked_me {
            return Err(ServerError::with_user_message(
                nsac_core::error::ErrorCode::Forbidden,
                "Não é possível enviar mensagens devido a um bloqueio.",
                "Message blocked",
            ));
        }
        if !rel.is_friend {
            return Err(ServerError::with_user_message(
                nsac_core::error::ErrorCode::Forbidden,
                "Vocês precisam ser amigos (seguir um ao outro) para enviar mensagens.",
                "Not mutual friends",
            ));
        }

        let trimmed = validate_message_text(raw_text)?;
        let rate = self.message_rate_limit_status(sender_id).await?;
        if rate.remaining <= 0 {
            return Err(ServerError::rate_limited(
                format!(
                    "Limite de {} mensagens a cada 3 minutos atingido. Aguarde {}s.",
                    MESSAGE_RATE_LIMIT_COUNT, rate.retry_after_secs
                ),
                "Social message rate limit exceeded",
            ));
        }

        let ciphertext = crypto.encrypt(trimmed)?;
        let (low, high) = ordered_pair(sender_id, peer_id);

        let inserted = sqlx::query_as::<_, MessageSqlRow>(
            r#"
            INSERT INTO social_messages (pair_low, pair_high, sender_id, body_ciphertext, created_at)
            VALUES ($1, $2, $3, $4, NOW())
            RETURNING id, sender_id, body_ciphertext, created_at
            "#,
        )
        .bind(low)
        .bind(high)
        .bind(sender_id)
        .bind(&ciphertext)
        .fetch_one(&self.pool)
        .await?;

        sqlx::query(
            r#"
            INSERT INTO social_conversation_state (account_id, peer_id, last_read_message_id, cleared_before_message_id, updated_at)
            VALUES ($1, $2, $3, 0, NOW())
            ON CONFLICT (account_id, peer_id) DO UPDATE SET
                last_read_message_id = GREATEST(social_conversation_state.last_read_message_id, EXCLUDED.last_read_message_id),
                updated_at = NOW()
            "#,
        )
        .bind(sender_id)
        .bind(peer_id)
        .bind(inserted.id)
        .execute(&self.pool)
        .await?;

        let updated_rate = self.message_rate_limit_status(sender_id).await?;

        Ok(SendMessagePayload {
            message: ChatMessage {
                id: inserted.id,
                sender_id,
                is_me: true,
                text: trimmed.to_string(),
                created_at: inserted.created_at,
            },
            rate_limit: updated_rate,
        })
    }

    pub async fn clear_conversation(
        &self,
        viewer: i64,
        peer_id: i64,
    ) -> Result<(), ServerError> {
        let (low, high) = ordered_pair(viewer, peer_id);
        let latest_id: i64 = sqlx::query_scalar(
            "SELECT COALESCE(MAX(id), 0)::bigint FROM social_messages WHERE pair_low = $1 AND pair_high = $2",
        )
        .bind(low)
        .bind(high)
        .fetch_one(&self.pool)
        .await?;

        sqlx::query(
            r#"
            INSERT INTO social_conversation_state (account_id, peer_id, last_read_message_id, cleared_before_message_id, updated_at)
            VALUES ($1, $2, $3, $3, NOW())
            ON CONFLICT (account_id, peer_id) DO UPDATE SET
                last_read_message_id = GREATEST(social_conversation_state.last_read_message_id, EXCLUDED.last_read_message_id),
                cleared_before_message_id = GREATEST(social_conversation_state.cleared_before_message_id, EXCLUDED.cleared_before_message_id),
                updated_at = NOW()
            "#,
        )
        .bind(viewer)
        .bind(peer_id)
        .bind(latest_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

pub fn ordered_pair(a: i64, b: i64) -> (i64, i64) {
    if a < b { (a, b) } else { (b, a) }
}

/// Pure rule: named ranking participants are followed immediately; students outside the ranking or
/// anonymous require approval (unless already accepted).
pub fn decide_follow_status(target: Visibility, existing: FollowEdgeStatus) -> FollowEdgeStatus {
    if existing == FollowEdgeStatus::Accepted || target.named_in_ranking() {
        FollowEdgeStatus::Accepted
    } else {
        FollowEdgeStatus::Pending
    }
}

pub fn validate_message_text(raw: &str) -> Result<&str, ServerError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(ServerError::with_user_message(
            nsac_core::error::ErrorCode::InvalidParams,
            "Digite uma mensagem antes de enviar.",
            "Empty chat message",
        ));
    }
    let char_count = trimmed.chars().count();
    if char_count > MAX_MESSAGE_CHARS {
        return Err(ServerError::with_user_message(
            nsac_core::error::ErrorCode::InvalidParams,
            format!("A mensagem pode ter no máximo {MAX_MESSAGE_CHARS} caracteres (enviado: {char_count})."),
            "Message exceeds 500-char limit",
        ));
    }
    Ok(trimmed)
}

fn truncate_preview(text: &str, max_chars: usize) -> String {
    let single_line: String = text
        .chars()
        .map(|c| if c == '\n' || c == '\r' { ' ' } else { c })
        .collect();
    let count = single_line.chars().count();
    if count <= max_chars {
        single_line
    } else {
        let mut out: String = single_line.chars().take(max_chars.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NAMED: Visibility = Visibility {
        opted_in: true,
        hide_name: false,
        show_avatar: false,
        public_profile: false,
        discoverable: false,
        invisible: false,
    };

    #[test]
    fn follow_status_auto_accepts_named_ranking_and_requests_approval_otherwise() {
        assert_eq!(
            decide_follow_status(NAMED, FollowEdgeStatus::None),
            FollowEdgeStatus::Accepted
        );
        assert_eq!(
            decide_follow_status(
                Visibility { discoverable: true, ..Visibility::default() },
                FollowEdgeStatus::None
            ),
            FollowEdgeStatus::Pending,
            "non-ranking discoverable student must approve follow"
        );
        assert_eq!(
            decide_follow_status(
                Visibility { hide_name: true, discoverable: true, ..NAMED },
                FollowEdgeStatus::None
            ),
            FollowEdgeStatus::Pending,
            "anonymous ranking student must approve follow"
        );
        assert_eq!(
            decide_follow_status(Visibility::default(), FollowEdgeStatus::Accepted),
            FollowEdgeStatus::Accepted,
            "existing accepted edge stays accepted"
        );
    }

    #[test]
    fn validate_message_enforces_500_unicode_chars() {
        assert!(validate_message_text("   \n\t ").is_err());
        let ok_500 = "é".repeat(500);
        assert!(validate_message_text(&ok_500).is_ok());
        let too_long = "é".repeat(501);
        assert!(validate_message_text(&too_long).is_err());
    }

    #[test]
    fn ordered_pair_always_sorts() {
        assert_eq!(ordered_pair(10, 2), (2, 10));
        assert_eq!(ordered_pair(2, 10), (2, 10));
    }
}
