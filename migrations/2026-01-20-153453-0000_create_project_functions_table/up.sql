CREATE TABLE project_functions (
    id BIGSERIAL PRIMARY KEY,
    project_id BIGINT NOT NULL REFERENCES projects(id),
    code TEXT,
    explanation TEXT,
    version VARCHAR(50) NOT NULL,
    parent_function_id BIGINT REFERENCES project_functions(id)
);