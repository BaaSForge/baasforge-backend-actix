-- Align project_unit_type with the 11 platform unit kinds (see units_definitions).
-- Expects no rows in project_units (drop column + new enum).

ALTER TABLE project_units DROP COLUMN unit_type;
DROP TYPE project_unit_type;
CREATE TYPE project_unit_type AS ENUM (
    'bootstrap',
    'action',
    'api',
    'command',
    'flow',
    'gateway',
    'projection',
    'query',
    'runtime',
    'schema',
    'timer'
);
ALTER TABLE project_units ADD COLUMN unit_type project_unit_type NOT NULL;
