ALTER TABLE project_runtimes
ADD COLUMN name VARCHAR(255);

ALTER TABLE project_runtimes
ADD COLUMN slug VARCHAR(100);

ALTER TABLE project_runtimes
ADD CONSTRAINT uq_project_runtimes_project_id_slug UNIQUE (project_id, slug);
