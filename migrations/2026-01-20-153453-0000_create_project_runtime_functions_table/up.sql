CREATE TABLE project_runtime_functions (
    id BIGSERIAL PRIMARY KEY,
    project_runtime_id BIGINT NOT NULL REFERENCES project_runtimes(id),
    deployment_metadata JSONB,
    deployment_status deployment_status_type NOT NULL,
    deployment_is_healthy BOOLEAN NOT NULL,
    deployment_last_health TIMESTAMP WITH TIME ZONE,
    code TEXT,
    explanation TEXT
);