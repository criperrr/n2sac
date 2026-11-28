-- Global developer announcements channel (write-restricted to accounts with platform_role = 'dev').
-- Startup-only migration (ZERO runtime DDL).

CREATE TABLE IF NOT EXISTS dev_announcements (
    id                BIGSERIAL PRIMARY KEY,
    author_account_id BIGINT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    title             TEXT NOT NULL,
    body              TEXT NOT NULL,
    tag               TEXT NOT NULL DEFAULT 'novidade',
    pinned            BOOLEAN NOT NULL DEFAULT FALSE,
    deleted_at        TIMESTAMPTZ,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at        TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_dev_announcements_active_created
    ON dev_announcements (pinned DESC, created_at DESC)
    WHERE deleted_at IS NULL;

CREATE TABLE IF NOT EXISTS dev_announcement_reads (
    announcement_id BIGINT NOT NULL REFERENCES dev_announcements(id) ON DELETE CASCADE,
    account_id      BIGINT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    read_at         TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (announcement_id, account_id)
);

CREATE INDEX IF NOT EXISTS idx_dev_announcement_reads_account
    ON dev_announcement_reads (account_id);
