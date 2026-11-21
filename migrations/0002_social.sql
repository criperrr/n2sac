CREATE TABLE IF NOT EXISTS social_follows (
    follower_id BIGINT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    followee_id BIGINT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    status TEXT NOT NULL CHECK (status IN ('pending', 'accepted')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (follower_id, followee_id),
    CHECK (follower_id <> followee_id)
);

CREATE INDEX IF NOT EXISTS idx_social_follows_followee_status
    ON social_follows (followee_id, status, created_at DESC);

CREATE TABLE IF NOT EXISTS social_blocks (
    blocker_id BIGINT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    blocked_id BIGINT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (blocker_id, blocked_id),
    CHECK (blocker_id <> blocked_id)
);

CREATE INDEX IF NOT EXISTS idx_social_blocks_blocked
    ON social_blocks (blocked_id);

CREATE TABLE IF NOT EXISTS social_messages (
    id BIGSERIAL PRIMARY KEY,
    pair_low BIGINT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    pair_high BIGINT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    sender_id BIGINT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    body_ciphertext TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CHECK (pair_low < pair_high),
    CHECK (sender_id = pair_low OR sender_id = pair_high)
);

CREATE INDEX IF NOT EXISTS idx_social_messages_pair_id
    ON social_messages (pair_low, pair_high, id DESC);

CREATE INDEX IF NOT EXISTS idx_social_messages_sender_created
    ON social_messages (sender_id, created_at DESC);

CREATE TABLE IF NOT EXISTS social_conversation_state (
    account_id BIGINT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    peer_id BIGINT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    last_read_message_id BIGINT NOT NULL DEFAULT 0,
    cleared_before_message_id BIGINT NOT NULL DEFAULT 0,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (account_id, peer_id),
    CHECK (account_id <> peer_id)
);
