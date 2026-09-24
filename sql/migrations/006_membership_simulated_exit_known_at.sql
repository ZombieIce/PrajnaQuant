-- Preserve separate knowledge times for an interval's entry and exit events.
-- Values synthesized from third-party effective dates remain explicitly marked.
ALTER TABLE core.index_membership_revision
 ADD COLUMN IF NOT EXISTS effective_to_published_at TIMESTAMPTZ;
ALTER TABLE core.index_membership_revision
 ADD COLUMN IF NOT EXISTS publication_time_method VARCHAR;
UPDATE core.index_membership_revision SET publication_time_method='unknown'
 WHERE publication_time_method IS NULL;

INSERT INTO ops.schema_version(version) VALUES (6) ON CONFLICT DO NOTHING;
