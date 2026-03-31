DROP INDEX IF EXISTS idx_project_releases_base_production_release_id;

ALTER TABLE project_releases DROP COLUMN IF EXISTS base_production_release_id;
