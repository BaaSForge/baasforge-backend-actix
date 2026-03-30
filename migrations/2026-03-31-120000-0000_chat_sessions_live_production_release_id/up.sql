ALTER TABLE chat_sessions
    ADD COLUMN live_production_release_id UUID REFERENCES project_releases(id) ON DELETE SET NULL;

CREATE INDEX idx_chat_sessions_live_production_release_id ON chat_sessions(live_production_release_id);
