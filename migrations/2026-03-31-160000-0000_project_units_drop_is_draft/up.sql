ALTER TABLE project_units
    DROP COLUMN IF EXISTS is_draft;

-- Enforce at most one row per logical unit revision within a chat session (slug tuple + revision).
CREATE UNIQUE INDEX project_units_session_logical_revision_uniq
    ON project_units (
        project_id,
        chat_session_id,
        context_slug,
        module_slug,
        unit_type,
        unit_name_slug,
        revision
    );
