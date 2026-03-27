ALTER TABLE project_releases DROP CONSTRAINT IF EXISTS uq_project_releases_project_id_is_stage_revision;

ALTER TABLE project_releases DROP COLUMN revision;

ALTER TABLE project_releases DROP COLUMN is_stage;

ALTER TABLE project_releases ADD COLUMN version VARCHAR(50) NOT NULL;

ALTER TABLE project_runtimes DROP COLUMN live_release_revision;

ALTER TABLE project_runtimes ADD COLUMN live_release_version VARCHAR(50) NULL;

ALTER TABLE project_releases
    ADD CONSTRAINT uq_project_releases_project_id_version UNIQUE (project_id, version);

ALTER TABLE project_runtimes
    ADD CONSTRAINT fk_project_runtimes_live_release_version
    FOREIGN KEY (project_id, live_release_version)
    REFERENCES project_releases (project_id, version);
