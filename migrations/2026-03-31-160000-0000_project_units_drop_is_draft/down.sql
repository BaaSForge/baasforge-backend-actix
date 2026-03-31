DROP INDEX IF EXISTS project_units_session_logical_revision_uniq;

ALTER TABLE project_units
    ADD COLUMN is_draft BOOLEAN NOT NULL DEFAULT true;
