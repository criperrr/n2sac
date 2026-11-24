-- Store Google-verified official full name for institutional @unesp.br accounts.
-- Startup-only migration (ZERO runtime DDL).

ALTER TABLE accounts
    ADD COLUMN IF NOT EXISTS verified_official_name TEXT NULL,
    ADD COLUMN IF NOT EXISTS official_name_verified_at TIMESTAMPTZ NULL;
