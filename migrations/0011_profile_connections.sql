-- 0011_profile_connections.sql
-- External profile connections (Discord, Spotify, GitHub, Steam, Twitch, Instagram,
-- TikTok, YouTube, X, Roblox, LinkedIn, Letterboxd) with OAuth2+PKCE / OpenID 2.0
-- verification states and strict migrations-only schema (zero runtime DDL).

CREATE TABLE IF NOT EXISTS account_connections (
    id BIGSERIAL PRIMARY KEY,
    account_id BIGINT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    provider TEXT NOT NULL,
    external_id TEXT,
    handle TEXT NOT NULL,
    display_name TEXT,
    profile_url TEXT,
    verification_source TEXT NOT NULL DEFAULT 'manual',
    is_verified BOOLEAN NOT NULL DEFAULT FALSE,
    is_visible BOOLEAN NOT NULL DEFAULT TRUE,
    display_order INTEGER NOT NULL DEFAULT 0,
    metadata_json JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT account_connections_unique_provider UNIQUE (account_id, provider),
    CONSTRAINT account_connections_verification_source_chk
        CHECK (verification_source IN ('manual', 'oauth', 'discord_import'))
);

CREATE INDEX IF NOT EXISTS idx_account_connections_account_visible
    ON account_connections (account_id, is_visible, display_order, id);

CREATE TABLE IF NOT EXISTS oauth_link_states (
    state_hash TEXT PRIMARY KEY,
    account_id BIGINT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    provider TEXT NOT NULL,
    pkce_verifier_ciphertext TEXT,
    client_platform TEXT NOT NULL DEFAULT 'web',
    expires_at TIMESTAMPTZ NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT oauth_link_states_platform_chk CHECK (client_platform IN ('web', 'android'))
);

CREATE INDEX IF NOT EXISTS idx_oauth_link_states_expires_at
    ON oauth_link_states (expires_at);
