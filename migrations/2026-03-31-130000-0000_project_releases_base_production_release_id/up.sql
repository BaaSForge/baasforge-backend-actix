ALTER TABLE project_releases
    ADD COLUMN base_production_release_id UUID REFERENCES project_releases (id) ON DELETE SET NULL;

CREATE INDEX idx_project_releases_base_production_release_id ON project_releases (base_production_release_id);
