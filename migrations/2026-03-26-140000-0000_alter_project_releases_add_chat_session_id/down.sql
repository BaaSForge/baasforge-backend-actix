DROP INDEX IF EXISTS idx_project_releases_chat_session_id;
ALTER TABLE project_releases DROP COLUMN IF EXISTS chat_session_id;
