-- Restrict `project_unit_type` to `api` and `workflow` only.
-- PostgreSQL cannot remove enum labels in place; we replace the type.
-- Rows whose kind is not kept are removed first (data loss by design).

DELETE FROM project_units WHERE unit_type::text NOT IN ('api');

CREATE TYPE project_unit_type_new AS ENUM ('api', 'workflow');

ALTER TABLE project_units
    ALTER COLUMN unit_type TYPE project_unit_type_new
    USING (unit_type::text::project_unit_type_new);

DROP TYPE project_unit_type;

ALTER TYPE project_unit_type_new RENAME TO project_unit_type;
