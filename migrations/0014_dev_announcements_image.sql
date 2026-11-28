-- Optional high-quality compressed image attachment for developer announcements.
-- Startup-only migration (ZERO runtime DDL).

ALTER TABLE dev_announcements
    ADD COLUMN IF NOT EXISTS image_data_url TEXT;
