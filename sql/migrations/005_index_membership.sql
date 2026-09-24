-- Replayable schema upgrade. Every DDL statement is idempotent so opening a
-- partially upgraded v4 warehouse can safely retry this migration.
CREATE TABLE IF NOT EXISTS core.index_membership_revision(
 revision_id VARCHAR PRIMARY KEY, index_code VARCHAR NOT NULL,
 instrument_id VARCHAR NOT NULL, effective_from DATE NOT NULL, effective_to DATE,
 published_at TIMESTAMPTZ, source_ref VARCHAR NOT NULL,
 source_snapshot_sha256 VARCHAR NOT NULL,
 source_revision_hash VARCHAR NOT NULL,
 verification_status VARCHAR NOT NULL CHECK(verification_status IN ('verified_pit','retrospective_static','unknown')),
 observed_at TIMESTAMPTZ NOT NULL, run_id VARCHAR NOT NULL, row_hash VARCHAR NOT NULL,
 CHECK(effective_to IS NULL OR effective_to > effective_from),
 CHECK(regexp_full_match(source_snapshot_sha256, '[0-9a-f]{64}')),
 CHECK(regexp_full_match(source_revision_hash, '[0-9a-f]{64}')),
 CHECK(verification_status<>'verified_pit' OR published_at IS NOT NULL)
);
CREATE VIEW IF NOT EXISTS core.index_membership_latest AS
 SELECT * FROM core.index_membership_revision
 QUALIFY row_number() OVER(PARTITION BY index_code, instrument_id, effective_from, source_ref
 ORDER BY observed_at DESC, revision_id DESC)=1;

-- The source's declared completeness is preserved separately from the
-- warehouse's verified coverage state.
CREATE TABLE IF NOT EXISTS ops.index_membership_coverage_revision(
 revision_id VARCHAR PRIMARY KEY, index_code VARCHAR NOT NULL,
 coverage_start DATE NOT NULL, coverage_end DATE NOT NULL,
 declared_status VARCHAR NOT NULL CHECK(declared_status IN ('complete','gaps','unverified')),
 coverage_status VARCHAR NOT NULL CHECK(coverage_status IN ('complete','gaps','unverified')),
 gap_detail VARCHAR, source_ref VARCHAR NOT NULL,
 source_snapshot_sha256 VARCHAR NOT NULL,
 source_revision_hash VARCHAR NOT NULL,
 observed_at TIMESTAMPTZ NOT NULL, run_id VARCHAR NOT NULL, row_hash VARCHAR NOT NULL,
 CHECK(coverage_end >= coverage_start),
 CHECK(regexp_full_match(source_snapshot_sha256, '[0-9a-f]{64}')),
 CHECK(regexp_full_match(source_revision_hash, '[0-9a-f]{64}'))
);
CREATE VIEW IF NOT EXISTS ops.index_membership_coverage_latest AS
 SELECT * FROM ops.index_membership_coverage_revision
 QUALIFY row_number() OVER(PARTITION BY index_code, coverage_start, coverage_end, source_ref
 ORDER BY observed_at DESC, revision_id DESC)=1;

INSERT INTO ops.schema_version(version) VALUES (5) ON CONFLICT DO NOTHING;
