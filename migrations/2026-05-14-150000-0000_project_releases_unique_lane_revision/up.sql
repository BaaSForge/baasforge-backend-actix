-- Prerequisite: run scripts/backfill_project_releases_unique_lane_revision.sql so no
-- duplicate (project_id, is_stage, revision) rows remain.

ALTER TABLE project_releases
    DROP CONSTRAINT uq_project_releases_project_id_is_stage_revision_chat_session;

ALTER TABLE project_releases
    ADD CONSTRAINT uq_project_releases_project_id_is_stage_revision
        UNIQUE (project_id, is_stage, revision);
