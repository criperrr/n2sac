-- Class Channels (Announcements, Class Chat, Off-Topic), Representative Elections,
-- Zipped Channel Media, Granular Notification Preferences & Optional Discord Mirroring.
-- Startup-only migration (ZERO runtime DDL — ARCH-01).

CREATE SEQUENCE IF NOT EXISTS room_events_rev_seq;

-- 1. Predefined mandatory class rooms scoped by (class_code, school_year).
CREATE TABLE IF NOT EXISTS class_rooms (
    id BIGSERIAL PRIMARY KEY,
    class_code TEXT NOT NULL,
    school_year INT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (class_code, school_year)
);

CREATE INDEX IF NOT EXISTS idx_class_rooms_code_year
    ON class_rooms (UPPER(class_code), school_year DESC);

-- Separate TOAST/BYTEA storage for room avatar & banner (stored zipped; zero blobs on list scans).
CREATE TABLE IF NOT EXISTS class_room_media (
    room_id BIGINT PRIMARY KEY REFERENCES class_rooms(id) ON DELETE CASCADE,
    avatar_zip BYTEA NULL,
    avatar_updated_at TIMESTAMPTZ NULL,
    banner_zip BYTEA NULL,
    banner_updated_at TIMESTAMPTZ NULL,
    updated_by_id BIGINT NULL REFERENCES accounts(id) ON DELETE SET NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- 2. Public (non-secret) approval voting for Class Representatives (up to 3 votes per student).
CREATE TABLE IF NOT EXISTS class_representative_votes (
    room_id BIGINT NOT NULL REFERENCES class_rooms(id) ON DELETE CASCADE,
    voter_id BIGINT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    candidate_id BIGINT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (room_id, voter_id, candidate_id),
    CHECK (voter_id <> candidate_id)
);

CREATE INDEX IF NOT EXISTS idx_class_rep_votes_room_candidate
    ON class_representative_votes (room_id, candidate_id, created_at ASC);

CREATE INDEX IF NOT EXISTS idx_class_rep_votes_room_voter
    ON class_representative_votes (room_id, voter_id);

-- Active elected representatives (max 3 per room; dynamically recalculated under advisory lock).
CREATE TABLE IF NOT EXISTS class_representatives (
    room_id BIGINT NOT NULL REFERENCES class_rooms(id) ON DELETE CASCADE,
    account_id BIGINT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    vote_count INT NOT NULL CHECK (vote_count >= 0),
    reached_threshold_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    elected_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (room_id, account_id)
);

CREATE INDEX IF NOT EXISTS idx_class_representatives_room
    ON class_representatives (room_id, vote_count DESC, reached_threshold_at ASC);

-- 3. Official Class Announcements (representatives-only write, encrypted body at rest).
CREATE TABLE IF NOT EXISTS class_announcements (
    id BIGSERIAL PRIMARY KEY,
    room_id BIGINT NOT NULL REFERENCES class_rooms(id) ON DELETE CASCADE,
    author_id BIGINT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    category TEXT NOT NULL DEFAULT 'general'
        CHECK (category IN ('general', 'exam', 'assignment', 'schedule', 'event')),
    title TEXT NOT NULL DEFAULT '',
    body_ciphertext TEXT NOT NULL,
    event_date DATE NULL,
    pinned BOOLEAN NOT NULL DEFAULT FALSE,
    notify_push BOOLEAN NOT NULL DEFAULT TRUE,
    has_attachment BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    edited_at TIMESTAMPTZ NULL,
    deleted_at TIMESTAMPTZ NULL,
    deleted_by_id BIGINT NULL REFERENCES accounts(id) ON DELETE SET NULL,
    rev BIGINT NOT NULL DEFAULT nextval('room_events_rev_seq')
);

CREATE INDEX IF NOT EXISTS idx_class_announcements_room_id
    ON class_announcements (room_id, id DESC);

CREATE INDEX IF NOT EXISTS idx_class_announcements_room_rev
    ON class_announcements (room_id, rev);

CREATE INDEX IF NOT EXISTS idx_class_announcements_room_pinned
    ON class_announcements (room_id, pinned DESC, id DESC)
    WHERE deleted_at IS NULL;

CREATE INDEX IF NOT EXISTS idx_class_announcements_room_push_recent
    ON class_announcements (room_id, created_at DESC)
    WHERE notify_push = TRUE AND deleted_at IS NULL;

-- Zipped JPEG attachments for announcements (representatives only; separate from list scans).
CREATE TABLE IF NOT EXISTS class_announcement_attachments (
    announcement_id BIGINT PRIMARY KEY REFERENCES class_announcements(id) ON DELETE CASCADE,
    mime_type TEXT NOT NULL DEFAULT 'image/jpeg',
    width INT NOT NULL CHECK (width > 0),
    height INT NOT NULL CHECK (height > 0),
    size_bytes INT NOT NULL CHECK (size_bytes > 0),
    sha256 TEXT NOT NULL,
    zip_bytes BYTEA NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS class_announcement_reactions (
    announcement_id BIGINT NOT NULL REFERENCES class_announcements(id) ON DELETE CASCADE,
    account_id BIGINT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    emoji TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (announcement_id, account_id)
);

CREATE TABLE IF NOT EXISTS class_announcement_reads (
    announcement_id BIGINT NOT NULL REFERENCES class_announcements(id) ON DELETE CASCADE,
    account_id BIGINT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    read_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (announcement_id, account_id)
);

CREATE INDEX IF NOT EXISTS idx_class_announcement_reads_account
    ON class_announcement_reads (account_id, announcement_id);

-- 4. Room Group Chats: 'chat' (class discussion) and 'off_topic' (casual/off-topic).
CREATE TABLE IF NOT EXISTS room_messages (
    id BIGSERIAL PRIMARY KEY,
    room_id BIGINT NOT NULL REFERENCES class_rooms(id) ON DELETE CASCADE,
    channel_kind TEXT NOT NULL CHECK (channel_kind IN ('chat', 'off_topic')),
    sender_id BIGINT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    body_ciphertext TEXT NOT NULL,
    reply_to_id BIGINT NULL REFERENCES room_messages(id) ON DELETE SET NULL,
    ref_announcement_id BIGINT NULL REFERENCES class_announcements(id) ON DELETE SET NULL,
    has_attachment BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    edited_at TIMESTAMPTZ NULL,
    deleted_at TIMESTAMPTZ NULL,
    deleted_by_id BIGINT NULL REFERENCES accounts(id) ON DELETE SET NULL,
    deletion_kind TEXT NULL CHECK (deletion_kind IS NULL OR deletion_kind IN ('author', 'representative', 'moderation')),
    rev BIGINT NOT NULL DEFAULT nextval('room_events_rev_seq')
);

CREATE INDEX IF NOT EXISTS idx_room_messages_room_chan_id
    ON room_messages (room_id, channel_kind, id DESC);

CREATE INDEX IF NOT EXISTS idx_room_messages_room_chan_rev
    ON room_messages (room_id, channel_kind, rev);

CREATE INDEX IF NOT EXISTS idx_room_messages_sender_created
    ON room_messages (sender_id, created_at DESC);

-- Zipped JPEG attachments for room chat messages (representatives only).
CREATE TABLE IF NOT EXISTS room_message_attachments (
    message_id BIGINT PRIMARY KEY REFERENCES room_messages(id) ON DELETE CASCADE,
    mime_type TEXT NOT NULL DEFAULT 'image/jpeg',
    width INT NOT NULL CHECK (width > 0),
    height INT NOT NULL CHECK (height > 0),
    size_bytes INT NOT NULL CHECK (size_bytes > 0),
    sha256 TEXT NOT NULL,
    zip_bytes BYTEA NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS room_message_reactions (
    message_id BIGINT NOT NULL REFERENCES room_messages(id) ON DELETE CASCADE,
    account_id BIGINT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    emoji TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (message_id, account_id)
);

CREATE TABLE IF NOT EXISTS room_channel_read_state (
    account_id BIGINT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    room_id BIGINT NOT NULL REFERENCES class_rooms(id) ON DELETE CASCADE,
    channel_kind TEXT NOT NULL CHECK (channel_kind IN ('chat', 'off_topic')),
    last_read_message_id BIGINT NOT NULL DEFAULT 0,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (account_id, room_id, channel_kind)
);

-- 5. Granular Notification Preferences (high-priority grades & announcements ON by default).
CREATE TABLE IF NOT EXISTS account_notification_preferences (
    account_id BIGINT PRIMARY KEY REFERENCES accounts(id) ON DELETE CASCADE,
    grades_enabled BOOLEAN NOT NULL DEFAULT TRUE,
    announcements_enabled BOOLEAN NOT NULL DEFAULT TRUE,
    social_enabled BOOLEAN NOT NULL DEFAULT TRUE,
    room_chat_mode TEXT NOT NULL DEFAULT 'mentions'
        CHECK (room_chat_mode IN ('all', 'mentions', 'muted')),
    room_offtopic_mode TEXT NOT NULL DEFAULT 'muted'
        CHECK (room_offtopic_mode IN ('all', 'mentions', 'muted')),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- 6. Optional Discord announcement mirroring for Gemilona (opt-in, never forced).
ALTER TABLE discord_channel_subscriptions
    ADD COLUMN IF NOT EXISTS mirror_announcements BOOLEAN NOT NULL DEFAULT FALSE;
