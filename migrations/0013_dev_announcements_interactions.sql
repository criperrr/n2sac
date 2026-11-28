-- Reactions, comments, and polls for developer announcements.
-- Startup-only migration (ZERO runtime DDL).

CREATE TABLE IF NOT EXISTS dev_announcement_reactions (
    announcement_id BIGINT NOT NULL REFERENCES dev_announcements(id) ON DELETE CASCADE,
    account_id      BIGINT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    emoji           TEXT NOT NULL,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (announcement_id, account_id, emoji)
);

CREATE INDEX IF NOT EXISTS idx_dev_announcement_reactions_ann
    ON dev_announcement_reactions (announcement_id);

CREATE TABLE IF NOT EXISTS dev_announcement_comments (
    id              BIGSERIAL PRIMARY KEY,
    announcement_id BIGINT NOT NULL REFERENCES dev_announcements(id) ON DELETE CASCADE,
    account_id      BIGINT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    body            TEXT NOT NULL,
    deleted_at      TIMESTAMPTZ,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_dev_announcement_comments_ann_created
    ON dev_announcement_comments (announcement_id, created_at ASC)
    WHERE deleted_at IS NULL;

CREATE TABLE IF NOT EXISTS dev_announcement_poll_options (
    id              BIGSERIAL PRIMARY KEY,
    announcement_id BIGINT NOT NULL REFERENCES dev_announcements(id) ON DELETE CASCADE,
    label           TEXT NOT NULL,
    position        SMALLINT NOT NULL DEFAULT 0
);

CREATE INDEX IF NOT EXISTS idx_dev_announcement_poll_options_ann
    ON dev_announcement_poll_options (announcement_id, position ASC);

CREATE TABLE IF NOT EXISTS dev_announcement_poll_votes (
    announcement_id BIGINT NOT NULL REFERENCES dev_announcements(id) ON DELETE CASCADE,
    option_id       BIGINT NOT NULL REFERENCES dev_announcement_poll_options(id) ON DELETE CASCADE,
    account_id      BIGINT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (announcement_id, account_id)
);

CREATE INDEX IF NOT EXISTS idx_dev_announcement_poll_votes_option
    ON dev_announcement_poll_votes (option_id);
