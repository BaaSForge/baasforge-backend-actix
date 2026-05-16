CREATE TABLE project_runtime_encryption_keys (
    runtime_id UUID PRIMARY KEY REFERENCES project_runtimes(id) ON DELETE CASCADE,
    encryption_key BYTEA NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE project_runtime_env_vars (
    id UUID PRIMARY KEY DEFAULT uuidv7(),
    project_id UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    runtime_id UUID NOT NULL REFERENCES project_runtimes(id) ON DELETE CASCADE,
    key TEXT NOT NULL,
    stored_value TEXT NOT NULL,
    is_encrypted BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT project_runtime_env_vars_runtime_key_unique UNIQUE (runtime_id, key)
);

CREATE INDEX project_runtime_env_vars_project_id_idx ON project_runtime_env_vars (project_id);
