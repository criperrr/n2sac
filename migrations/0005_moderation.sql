-- Community moderation: reports on profile photo/banner (NSFW) and name/bio (offensive).
--
-- 3 distinct reporters within 30 days = one strike: the reported content is deleted and the
-- account can't set that kind of content for 7 days. A second strike makes it permanent.

CREATE TABLE IF NOT EXISTS content_reports (
    id BIGSERIAL PRIMARY KEY,
    reporter_id BIGINT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    target_id BIGINT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    kind TEXT NOT NULL CHECK (kind IN ('media', 'text')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    -- Set when a strike consumed the report (or the content it pointed at was removed).
    resolved_at TIMESTAMPTZ NULL,
    CHECK (reporter_id <> target_id)
);

-- One open report per person per target/kind: repeated taps never add votes.
CREATE UNIQUE INDEX IF NOT EXISTS idx_content_reports_open_unique
    ON content_reports (reporter_id, target_id, kind)
    WHERE resolved_at IS NULL;

CREATE INDEX IF NOT EXISTS idx_content_reports_target_open
    ON content_reports (target_id, kind, created_at)
    WHERE resolved_at IS NULL;

CREATE INDEX IF NOT EXISTS idx_content_reports_reporter_recent
    ON content_reports (reporter_id, created_at DESC);

CREATE TABLE IF NOT EXISTS moderation_restrictions (
    account_id BIGINT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    kind TEXT NOT NULL CHECK (kind IN ('media', 'text')),
    strikes INT NOT NULL DEFAULT 0,
    restricted_until TIMESTAMPTZ NULL,
    permanent BOOLEAN NOT NULL DEFAULT FALSE,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (account_id, kind)
);

-- Audit trail for the owner (who acted on whom and why); never exposed through the API.
CREATE TABLE IF NOT EXISTS moderation_events (
    id BIGSERIAL PRIMARY KEY,
    account_id BIGINT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    kind TEXT NOT NULL,
    action TEXT NOT NULL,
    reporter_ids BIGINT[] NOT NULL DEFAULT '{}',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
