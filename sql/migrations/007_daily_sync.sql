CREATE TABLE IF NOT EXISTS ops.daily_sync_job (
    job_id VARCHAR PRIMARY KEY,
    request_fingerprint VARCHAR NOT NULL,
    dataset VARCHAR NOT NULL,
    source VARCHAR NOT NULL,
    symbols_json VARCHAR NOT NULL,
    range_start DATE NOT NULL,
    range_end DATE NOT NULL,
    lookback_days INTEGER NOT NULL,
    status VARCHAR NOT NULL CHECK(status IN ('RUNNING','SUCCESS','FAILED','AUDIT_FAILED','NOT_PUBLISHED','PUBLISHED')),
    cursor_symbol VARCHAR,
    cursor_start DATE,
    cursor_end DATE,
    attempted_count INTEGER NOT NULL DEFAULT 0,
    source_rows INTEGER NOT NULL DEFAULT 0,
    inserted_revisions INTEGER NOT NULL DEFAULT 0,
    duplicate_rows INTEGER NOT NULL DEFAULT 0,
    empty_responses INTEGER NOT NULL DEFAULT 0,
    audit_json VARCHAR,
    snapshot_id VARCHAR,
    error_class VARCHAR,
    error TEXT,
    started_at TIMESTAMPTZ NOT NULL,
    finished_at TIMESTAMPTZ,
    updated_at TIMESTAMPTZ NOT NULL
);

CREATE TABLE IF NOT EXISTS ops.daily_sync_attempt (
    attempt_id VARCHAR PRIMARY KEY,
    job_id VARCHAR NOT NULL,
    symbol VARCHAR NOT NULL,
    window_start DATE NOT NULL,
    window_end DATE NOT NULL,
    attempt_no INTEGER NOT NULL,
    status VARCHAR NOT NULL CHECK(status IN ('SUCCESS','EMPTY','NOT_PUBLISHED','FAILED')),
    source_rows INTEGER NOT NULL DEFAULT 0,
    inserted_revisions INTEGER NOT NULL DEFAULT 0,
    raw_path VARCHAR,
    raw_sha256 VARCHAR,
    request_url VARCHAR,
    error_class VARCHAR,
    error TEXT,
    started_at TIMESTAMPTZ NOT NULL,
    finished_at TIMESTAMPTZ NOT NULL,
    UNIQUE(job_id, symbol, window_start, window_end, attempt_no)
);

CREATE TABLE IF NOT EXISTS ops.daily_snapshot (
    snapshot_id VARCHAR PRIMARY KEY,
    job_id VARCHAR NOT NULL,
    as_of_date DATE NOT NULL,
    data_cutoff_date DATE NOT NULL,
    coverage_status VARCHAR NOT NULL CHECK(coverage_status IN ('complete','gaps','unverified')),
    manifest_path VARCHAR NOT NULL,
    manifest_sha256 VARCHAR NOT NULL,
    created_at TIMESTAMPTZ NOT NULL
);

CREATE TABLE IF NOT EXISTS ops.daily_snapshot_current (
    dataset VARCHAR PRIMARY KEY,
    snapshot_id VARCHAR NOT NULL,
    manifest_path VARCHAR NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL
);

CREATE TABLE IF NOT EXISTS ops.security_status_coverage (
    coverage_id VARCHAR PRIMARY KEY,
    source VARCHAR NOT NULL,
    symbol VARCHAR NOT NULL,
    coverage_start DATE NOT NULL,
    coverage_end DATE NOT NULL,
    declared_coverage VARCHAR NOT NULL CHECK(declared_coverage IN ('complete','gaps','unverified')),
    verification_status VARCHAR NOT NULL CHECK(verification_status IN ('verified','unverified')),
    source_ref VARCHAR NOT NULL,
    raw_path VARCHAR,
    raw_sha256 VARCHAR,
    detail TEXT,
    observed_at TIMESTAMPTZ NOT NULL,
    CHECK(coverage_start <= coverage_end)
);

INSERT INTO ops.schema_version(version) VALUES (7) ON CONFLICT DO NOTHING;

ALTER TABLE core.security_status_revision ADD COLUMN IF NOT EXISTS source_ref VARCHAR;
ALTER TABLE core.security_status_revision ADD COLUMN IF NOT EXISTS raw_path VARCHAR;
ALTER TABLE core.security_status_revision ADD COLUMN IF NOT EXISTS raw_sha256 VARCHAR;
ALTER TABLE core.security_status_revision ADD COLUMN IF NOT EXISTS published_at TIMESTAMPTZ;
ALTER TABLE core.security_status_revision ADD COLUMN IF NOT EXISTS available_at TIMESTAMPTZ;
ALTER TABLE core.security_status_revision ADD COLUMN IF NOT EXISTS verification_status VARCHAR DEFAULT 'unknown';
ALTER TABLE core.security_status_revision ADD COLUMN IF NOT EXISTS coverage_ref VARCHAR;
