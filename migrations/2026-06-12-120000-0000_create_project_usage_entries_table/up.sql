CREATE TABLE project_usage_entries (
    id UUID PRIMARY KEY DEFAULT uuidv7(),
    project_id UUID NOT NULL REFERENCES projects(id),
    recorded_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    kind VARCHAR(32) NOT NULL,
    metric VARCHAR(64) NOT NULL,
    quantity BIGINT NOT NULL CHECK (quantity >= 0),
    price_unit VARCHAR(16) NOT NULL,
    details JSONB NOT NULL DEFAULT '{}'::jsonb
);

CREATE INDEX idx_project_usage_entries_project_id_recorded_at
    ON project_usage_entries (project_id, recorded_at DESC);
