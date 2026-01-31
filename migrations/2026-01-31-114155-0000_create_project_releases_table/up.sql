CREATE TABLE project_releases (
    id BIGSERIAL PRIMARY KEY,
    project_id BIGINT NOT NULL REFERENCES projects(id),
    version VARCHAR(50) NOT NULL,
    functions_metadata JSONB
);