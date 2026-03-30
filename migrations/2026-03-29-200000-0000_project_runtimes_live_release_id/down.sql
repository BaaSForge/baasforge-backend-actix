ALTER TABLE project_runtimes DROP CONSTRAINT IF EXISTS fk_project_runtimes_live_release_id;

ALTER TABLE project_runtimes
    DROP COLUMN live_release_id,
    ADD COLUMN live_release_revision INTEGER NULL;
