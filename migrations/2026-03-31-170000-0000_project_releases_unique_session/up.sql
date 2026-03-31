-- Revision is computed per chat session (major from session baseline + ordinal in session).
-- Different sessions can legitimately share the same numeric revision for the same project/lane.

ALTER TABLE project_releases
    DROP CONSTRAINT uq_project_releases_project_id_is_stage_revision;

ALTER TABLE project_releases
    ADD CONSTRAINT uq_project_releases_project_id_is_stage_revision_chat_session
        UNIQUE (project_id, is_stage, revision, chat_session_id);
