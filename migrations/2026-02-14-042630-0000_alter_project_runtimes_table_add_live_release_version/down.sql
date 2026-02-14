ALTER TABLE project_runtimes
DROP CONSTRAINT IF EXISTS fk_project_runtimes_live_release_version;

ALTER TABLE project_runtimes
DROP COLUMN IF EXISTS live_release_version;

ALTER TABLE project_releases
DROP CONSTRAINT IF EXISTS uq_project_releases_project_id_version;
