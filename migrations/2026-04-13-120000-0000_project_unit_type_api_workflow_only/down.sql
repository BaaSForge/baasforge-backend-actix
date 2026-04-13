-- Restore the previous multi-value enum (see 2026-03-23-180000-0000_replace_project_unit_type_with_architecture_kinds).

DELETE FROM project_units WHERE unit_type::text = 'workflow';

CREATE TYPE project_unit_type_old AS ENUM (
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

ALTER TABLE project_units
    ALTER COLUMN unit_type TYPE project_unit_type_old
    USING (unit_type::text::project_unit_type_old);

DROP TYPE project_unit_type;

ALTER TYPE project_unit_type_old RENAME TO project_unit_type;
