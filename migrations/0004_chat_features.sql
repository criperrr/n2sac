-- Chat features: replies, edits, unsend, reactions and revision-based sync.
--
-- Every mutation of a message (insert, edit, unsend, reaction change) takes a fresh value from
-- `social_messages_rev_seq`, so clients poll with `after_rev` and receive new and changed messages
-- in one query.

CREATE SEQUENCE IF NOT EXISTS social_messages_rev_seq;

ALTER TABLE social_messages
    ADD COLUMN IF NOT EXISTS reply_to_id BIGINT REFERENCES social_messages(id) ON DELETE SET NULL,
    ADD COLUMN IF NOT EXISTS edited_at TIMESTAMPTZ,
    ADD COLUMN IF NOT EXISTS deleted_at TIMESTAMPTZ,
    ADD COLUMN IF NOT EXISTS rev BIGINT NOT NULL DEFAULT nextval('social_messages_rev_seq');

CREATE INDEX IF NOT EXISTS idx_social_messages_pair_rev
    ON social_messages (pair_low, pair_high, rev);

CREATE TABLE IF NOT EXISTS social_message_reactions (
    message_id BIGINT NOT NULL REFERENCES social_messages(id) ON DELETE CASCADE,
    account_id BIGINT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    emoji TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (message_id, account_id)
);
