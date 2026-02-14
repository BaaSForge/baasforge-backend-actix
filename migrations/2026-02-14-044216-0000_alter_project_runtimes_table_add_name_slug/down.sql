ALTER TABLE project_runtimes
DROP CONSTRAINT IF EXISTS uq_project_runtimes_project_id_slug;

ALTER TABLE project_runtimes
DROP COLUMN IF EXISTS slug;

ALTER TABLE project_runtimes
DROP COLUMN IF EXISTS name;
