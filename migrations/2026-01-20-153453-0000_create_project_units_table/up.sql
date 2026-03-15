CREATE TYPE project_unit_type AS ENUM ('controller');

CREATE TABLE project_units (
    id UUID PRIMARY KEY DEFAULT uuidv7(),
    project_id UUID NOT NULL REFERENCES projects(id),
    code TEXT,
    explanation TEXT,
    version VARCHAR(50) NOT NULL,
    parent_unit_id UUID REFERENCES project_units(id),
    unit_type project_unit_type NOT NULL
);