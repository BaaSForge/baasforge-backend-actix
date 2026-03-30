DROP INDEX IF EXISTS idx_chat_sessions_live_production_release_id;

ALTER TABLE chat_sessions DROP COLUMN IF EXISTS live_production_release_id;
