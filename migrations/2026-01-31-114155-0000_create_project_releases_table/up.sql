CREATE TABLE project_releases (
    id UUID PRIMARY KEY DEFAULT uuidv7(),
    project_id UUID NOT NULL REFERENCES projects(id),
    version VARCHAR(50) NOT NULL,
    units_metadata JSONB
);