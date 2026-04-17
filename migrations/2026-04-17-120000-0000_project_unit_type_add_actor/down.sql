DELETE FROM project_units WHERE unit_type::text = 'actor';

CREATE TYPE project_unit_type_old AS ENUM ('api', 'workflow', 'queue_worker');

ALTER TABLE project_units
    ALTER COLUMN unit_type TYPE project_unit_type_old
    USING (unit_type::text::project_unit_type_old);

DROP TYPE project_unit_type;

ALTER TYPE project_unit_type_old RENAME TO project_unit_type;
