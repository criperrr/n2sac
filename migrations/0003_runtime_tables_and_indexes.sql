-- Move all runtime-created tables, columns, and performance indexes into startup migrations.
-- Schema creation and evolution MUST happen exclusively during database initialization
-- (`run_migrations`), never inside HTTP request handlers or repository methods.

CREATE TABLE IF NOT EXISTS account_ranking_preferences (
    account_id BIGINT PRIMARY KEY REFERENCES accounts(id) ON DELETE CASCADE,
    opted_in BOOLEAN NOT NULL DEFAULT FALSE,
    hide_name BOOLEAN NOT NULL DEFAULT FALSE,
    display_name TEXT NULL,
    show_avatar BOOLEAN NOT NULL DEFAULT FALSE,
    public_profile BOOLEAN NOT NULL DEFAULT FALSE,
    visibility_customized BOOLEAN NOT NULL DEFAULT FALSE,
    discoverable BOOLEAN NOT NULL DEFAULT FALSE,
    invisible BOOLEAN NOT NULL DEFAULT FALSE,
    has_decided BOOLEAN NOT NULL DEFAULT FALSE,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Upgrade-safe column additions for existing production databases created before this migration.
ALTER TABLE account_ranking_preferences
    ADD COLUMN IF NOT EXISTS show_avatar BOOLEAN NOT NULL DEFAULT FALSE,
    ADD COLUMN IF NOT EXISTS public_profile BOOLEAN NOT NULL DEFAULT FALSE,
    ADD COLUMN IF NOT EXISTS visibility_customized BOOLEAN NOT NULL DEFAULT FALSE,
    ADD COLUMN IF NOT EXISTS discoverable BOOLEAN NOT NULL DEFAULT FALSE,
    ADD COLUMN IF NOT EXISTS invisible BOOLEAN NOT NULL DEFAULT FALSE;

CREATE INDEX IF NOT EXISTS idx_ranking_prefs_opted_in
    ON account_ranking_preferences (opted_in)
    WHERE opted_in = TRUE;

CREATE TABLE IF NOT EXISTS account_profiles (
    account_id BIGINT PRIMARY KEY REFERENCES accounts(id) ON DELETE CASCADE,
    display_name TEXT NULL,
    bio TEXT NULL,
    avatar_data_url TEXT NULL,
    banner_data_url TEXT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS telemetry_events (
    id BIGSERIAL PRIMARY KEY,
    event_type TEXT NOT NULL,
    app_version TEXT NOT NULL DEFAULT '',
    platform TEXT NOT NULL DEFAULT '',
    device_fingerprint TEXT NOT NULL DEFAULT '',
    hardware_json JSONB NOT NULL DEFAULT '{}'::jsonb,
    metrics_json JSONB NOT NULL DEFAULT '{}'::jsonb,
    crash_json JSONB,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_telemetry_events_type_created
    ON telemetry_events (event_type, created_at DESC);

CREATE TABLE IF NOT EXISTS discord_channel_subscriptions (
    id BIGSERIAL PRIMARY KEY,
    guild_id TEXT,
    channel_id TEXT NOT NULL,
    class_code TEXT NOT NULL,
    mention_mode TEXT NOT NULL DEFAULT 'everyone',
    configured_by_discord_id TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (channel_id, class_code)
);

CREATE INDEX IF NOT EXISTS idx_discord_channel_subs_upper_class
    ON discord_channel_subscriptions (UPPER(class_code));

CREATE TABLE IF NOT EXISTS discord_dm_preferences (
    account_id BIGINT PRIMARY KEY REFERENCES accounts(id) ON DELETE CASCADE,
    discord_id TEXT NOT NULL,
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Query-path indexes identified from production pg_stat_user_tables and EXPLAIN ANALYZE.
CREATE INDEX IF NOT EXISTS idx_school_years_year
    ON school_years (year);

CREATE INDEX IF NOT EXISTS idx_grades_account_bimester_posted
    ON grades (account_id, bimester)
    WHERE grade IS NOT NULL;

CREATE INDEX IF NOT EXISTS idx_accounts_upper_class_code_active
    ON accounts (UPPER(class_code))
    WHERE active = TRUE;
