DELETE FROM project_units;

ALTER TABLE project_units
    DROP CONSTRAINT IF EXISTS project_units_parent_unit_id_fkey,
    DROP CONSTRAINT IF EXISTS project_units_pkey;

ALTER TABLE project_units
    DROP COLUMN id,
    DROP COLUMN version,
    DROP COLUMN parent_unit_id;

ALTER TABLE project_units
    ALTER COLUMN chat_session_id SET NOT NULL;

ALTER TABLE project_units
    ADD COLUMN context_slug VARCHAR(255) NOT NULL,
    ADD COLUMN module_slug VARCHAR(255) NOT NULL,
    ADD COLUMN unit_name_slug VARCHAR(255) NOT NULL,
    ADD COLUMN id TEXT NOT NULL,
    ADD COLUMN revision INTEGER NOT NULL,
    ADD PRIMARY KEY (project_id, chat_session_id, id, revision);

ALTER TABLE project_units
    ADD CONSTRAINT project_units_context_slug_chk CHECK (context_slug ~ '^[a-z]+(-[a-z]+)*$'),
    ADD CONSTRAINT project_units_module_slug_chk CHECK (module_slug ~ '^[a-z]+(-[a-z]+)*$'),
    ADD CONSTRAINT project_units_unit_name_slug_chk CHECK (unit_name_slug ~ '^[a-z]+(-[a-z]+)*$'),
    ADD CONSTRAINT project_units_id_matches_parts_chk CHECK (
        id = context_slug || '.' || module_slug || '.' || unit_type::text || '.' || unit_name_slug || '.' || revision::text
    ),
    ADD CONSTRAINT project_units_revision_positive_chk CHECK (revision >= 1);

CREATE INDEX project_units_project_id_chat_session_id_idx ON project_units (project_id, chat_session_id, id);
