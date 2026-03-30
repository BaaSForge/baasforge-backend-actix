ALTER TABLE project_runtimes
    ADD COLUMN live_release_id UUID NULL,
    ADD CONSTRAINT fk_project_runtimes_live_release_id
    FOREIGN KEY (live_release_id) REFERENCES project_releases (id) ON DELETE SET NULL;

ALTER TABLE project_runtimes DROP COLUMN live_release_revision;
