-- Empty DB: version -> (is_stage, revision). Unique (project_id, is_stage, revision) so stage and prod can share the same revision number.
-- Runtime stores only live_release_revision; stage vs production is implied by project_runtimes.environment (Stage / Prod).

ALTER TABLE project_runtimes DROP CONSTRAINT IF EXISTS fk_project_runtimes_live_release_version;

ALTER TABLE project_releases DROP CONSTRAINT IF EXISTS uq_project_releases_project_id_version;

ALTER TABLE project_releases DROP COLUMN version;

ALTER TABLE project_releases
    ADD COLUMN is_stage BOOLEAN NOT NULL DEFAULT true,
    ADD COLUMN revision INTEGER NOT NULL;

ALTER TABLE project_runtimes DROP COLUMN live_release_version;

ALTER TABLE project_runtimes
    ADD COLUMN live_release_revision INTEGER NULL;

ALTER TABLE project_releases
    ADD CONSTRAINT uq_project_releases_project_id_is_stage_revision UNIQUE (project_id, is_stage, revision);
