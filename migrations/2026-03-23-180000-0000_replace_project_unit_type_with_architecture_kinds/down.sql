-- Restore previous MVC enum (expects no rows in project_units).

ALTER TABLE project_units DROP COLUMN unit_type;
DROP TYPE project_unit_type;
CREATE TYPE project_unit_type AS ENUM ('controller', 'model', 'view');
ALTER TABLE project_units ADD COLUMN unit_type project_unit_type NOT NULL;
