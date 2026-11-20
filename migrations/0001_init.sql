-- NSAC Unified Server initial PostgreSQL schema (M1)

CREATE TABLE accounts (
    id BIGSERIAL PRIMARY KEY,
    nsac_email TEXT NOT NULL UNIQUE,
    nsac_pass_enc TEXT NOT NULL,
    nsac_cookies_enc TEXT,
    class_code TEXT,
    fcm_token TEXT,
    discord_id TEXT UNIQUE,
    active BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX idx_accounts_class_code_active
    ON accounts (class_code)
    WHERE active = TRUE;

CREATE TABLE school_years (
    id BIGSERIAL PRIMARY KEY,
    year INT NOT NULL,
    title TEXT NOT NULL,
    UNIQUE (title, year)
);

CREATE TABLE accounts_school_years (
    id BIGSERIAL PRIMARY KEY,
    account_id BIGINT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    school_year_id BIGINT NOT NULL REFERENCES school_years(id) ON DELETE CASCADE,
    status TEXT NOT NULL,
    UNIQUE (account_id, school_year_id)
);

CREATE INDEX idx_accounts_school_years_account_id
    ON accounts_school_years (account_id);

CREATE TABLE subjects (
    id BIGSERIAL PRIMARY KEY,
    school_year_id BIGINT NOT NULL REFERENCES school_years(id) ON DELETE CASCADE,
    abbreviation TEXT NOT NULL DEFAULT '',
    name TEXT NOT NULL,
    UNIQUE (name, school_year_id)
);

CREATE INDEX idx_subjects_school_year_id
    ON subjects (school_year_id);

CREATE TABLE bimesters (
    id BIGSERIAL PRIMARY KEY,
    account_id BIGINT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    school_year_id BIGINT NOT NULL REFERENCES school_years(id) ON DELETE CASCADE,
    bimester SMALLINT NOT NULL CHECK (bimester BETWEEN 1 AND 4),
    user_average DOUBLE PRECISION,
    class_average DOUBLE PRECISION,
    total_absences INT NOT NULL DEFAULT 0,
    UNIQUE (account_id, school_year_id, bimester)
);

CREATE INDEX idx_bimesters_account_id
    ON bimesters (account_id);

CREATE TABLE grades (
    id BIGSERIAL PRIMARY KEY,
    account_id BIGINT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    subject_id BIGINT NOT NULL REFERENCES subjects(id) ON DELETE CASCADE,
    bimester SMALLINT NOT NULL CHECK (bimester BETWEEN 1 AND 4),
    grade DOUBLE PRECISION,
    average_grade DOUBLE PRECISION,
    absences INT NOT NULL DEFAULT 0,
    recovery_code TEXT NOT NULL DEFAULT '',
    recovery_message TEXT NOT NULL DEFAULT '',
    approved BOOLEAN NOT NULL DEFAULT FALSE,
    recovered BOOLEAN NOT NULL DEFAULT FALSE,
    UNIQUE (account_id, subject_id, bimester)
);

CREATE INDEX idx_grades_account_id
    ON grades (account_id);

CREATE INDEX idx_grades_subject_id
    ON grades (subject_id);

CREATE TABLE subject_final_results (
    id BIGSERIAL PRIMARY KEY,
    account_id BIGINT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    subject_id BIGINT NOT NULL REFERENCES subjects(id) ON DELETE CASCADE,
    final_grade DOUBLE PRECISION,
    total_absences INT NOT NULL DEFAULT 0,
    UNIQUE (account_id, subject_id)
);

CREATE INDEX idx_subject_final_results_account_id
    ON subject_final_results (account_id);

CREATE TABLE service_clients (
    id BIGSERIAL PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,
    key_hash TEXT NOT NULL UNIQUE,
    scopes TEXT[] NOT NULL DEFAULT '{}',
    active BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE user_sessions (
    id BIGSERIAL PRIMARY KEY,
    account_id BIGINT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    token_hash TEXT NOT NULL UNIQUE,
    expires_at TIMESTAMPTZ NOT NULL,
    revoked_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX idx_user_sessions_account_id
    ON user_sessions (account_id);

CREATE TABLE auxiliary_snapshots (
    id BIGSERIAL PRIMARY KEY,
    account_id BIGINT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    route_key TEXT NOT NULL,
    payload_json JSONB NOT NULL,
    etag TEXT NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (account_id, route_key)
);

CREATE TABLE sentinel_class_state (
    class_code TEXT PRIMARY KEY,
    last_class_hash TEXT,
    last_checked_at TIMESTAMPTZ,
    last_change_detected_at TIMESTAMPTZ,
    hot_until TIMESTAMPTZ,
    next_check_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    current_certainty DOUBLE PRECISION NOT NULL DEFAULT 0.0,
    current_interval_secs BIGINT NOT NULL DEFAULT 86400,
    consecutive_errors INT NOT NULL DEFAULT 0,
    last_api_bump_at TIMESTAMPTZ,
    active_sentinel_account_id BIGINT REFERENCES accounts(id) ON DELETE SET NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX idx_sentinel_class_state_next_check
    ON sentinel_class_state (next_check_at);

CREATE TABLE grade_posting_events (
    id BIGSERIAL PRIMARY KEY,
    class_code TEXT NOT NULL,
    school_year INT NOT NULL,
    bimester SMALLINT NOT NULL CHECK (bimester BETWEEN 1 AND 4),
    subject_name TEXT NOT NULL,
    day_of_year SMALLINT NOT NULL CHECK (day_of_year BETWEEN 1 AND 366),
    month SMALLINT NOT NULL CHECK (month BETWEEN 1 AND 12),
    day SMALLINT NOT NULL CHECK (day BETWEEN 1 AND 31),
    posted_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX idx_grade_posting_events_class_doy
    ON grade_posting_events (class_code, day_of_year);

CREATE INDEX idx_grade_posting_events_class_year_bim
    ON grade_posting_events (class_code, school_year, bimester);
