-- Feedback, suggestions & bug reports with separate attachment storage (zero TOAST blobs in list scans).

CREATE TABLE IF NOT EXISTS feedback_submissions (
    id BIGSERIAL PRIMARY KEY,
    account_id BIGINT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    kind TEXT NOT NULL CHECK (kind IN ('bug', 'suggestion', 'feedback')),
    status TEXT NOT NULL DEFAULT 'open' CHECK (status IN ('open', 'triaged', 'resolved', 'dismissed')),
    area TEXT NOT NULL,
    severity TEXT NULL CHECK (severity IS NULL OR severity IN ('blocker', 'major', 'minor')),
    frequency TEXT NULL CHECK (frequency IS NULL OR frequency IN ('always', 'sometimes', 'once')),
    rating SMALLINT NULL CHECK (rating IS NULL OR rating BETWEEN 1 AND 5),
    title TEXT NOT NULL,
    body_json JSONB NOT NULL,
    context_json JSONB NOT NULL DEFAULT '{}'::jsonb,
    content_hash TEXT NOT NULL,
    attachment_count SMALLINT NOT NULL DEFAULT 0 CHECK (attachment_count BETWEEN 0 AND 3),
    dev_note TEXT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CHECK (kind = 'bug' OR (severity IS NULL AND frequency IS NULL AND attachment_count = 0)),
    CHECK (kind = 'feedback' OR rating IS NULL)
);

CREATE INDEX IF NOT EXISTS idx_feedback_account_created
    ON feedback_submissions (account_id, created_at DESC);

CREATE INDEX IF NOT EXISTS idx_feedback_dedup
    ON feedback_submissions (account_id, kind, content_hash, created_at DESC);

CREATE INDEX IF NOT EXISTS idx_feedback_triage
    ON feedback_submissions (status, kind, created_at DESC);

CREATE TABLE IF NOT EXISTS feedback_attachments (
    id BIGSERIAL PRIMARY KEY,
    submission_id BIGINT NOT NULL REFERENCES feedback_submissions(id) ON DELETE CASCADE,
    sort_order SMALLINT NOT NULL CHECK (sort_order BETWEEN 0 AND 2),
    mime_type TEXT NOT NULL DEFAULT 'image/jpeg',
    width INT NOT NULL CHECK (width > 0),
    height INT NOT NULL CHECK (height > 0),
    size_bytes INT NOT NULL CHECK (size_bytes > 0),
    sha256 TEXT NOT NULL,
    image_bytes BYTEA NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (submission_id, sort_order)
);
