CREATE TYPE project_unit_type AS ENUM ('controller');

CREATE TABLE project_units (
    id BIGSERIAL PRIMARY KEY,
    project_id BIGINT NOT NULL REFERENCES projects(id),
    code TEXT,
    explanation TEXT,
    version VARCHAR(50) NOT NULL,
    parent_unit_id BIGINT REFERENCES project_units(id),
    unit_type project_unit_type NOT NULL
);