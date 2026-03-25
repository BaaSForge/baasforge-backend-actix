DELETE FROM project_units;

ALTER TABLE project_units
    DROP CONSTRAINT IF EXISTS project_units_revision_positive_chk,
    DROP CONSTRAINT IF EXISTS project_units_id_matches_parts_chk,
    DROP CONSTRAINT IF EXISTS project_units_context_slug_chk,
    DROP CONSTRAINT IF EXISTS project_units_module_slug_chk,
    DROP CONSTRAINT IF EXISTS project_units_unit_name_slug_chk;

DROP INDEX IF EXISTS project_units_project_id_chat_session_id_revision_idx;

ALTER TABLE project_units DROP CONSTRAINT IF EXISTS project_units_pkey;

ALTER TABLE project_units
    ALTER COLUMN chat_session_id DROP NOT NULL;

ALTER TABLE project_units
    DROP COLUMN IF EXISTS revision,
    DROP COLUMN IF EXISTS context_slug,
    DROP COLUMN IF EXISTS module_slug,
    DROP COLUMN IF EXISTS unit_name_slug,
    DROP COLUMN IF EXISTS id;

ALTER TABLE project_units
    ADD COLUMN id UUID NOT NULL DEFAULT uuidv7(),
    ADD COLUMN version VARCHAR(50) NOT NULL,
    ADD COLUMN parent_unit_id UUID,
    ADD PRIMARY KEY (id);

ALTER TABLE project_units
    ADD CONSTRAINT project_units_parent_unit_id_fkey
    FOREIGN KEY (parent_unit_id) REFERENCES project_units (id);
