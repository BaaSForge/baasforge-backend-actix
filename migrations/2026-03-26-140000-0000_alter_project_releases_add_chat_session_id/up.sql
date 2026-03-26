ALTER TABLE project_releases
  ADD COLUMN chat_session_id UUID REFERENCES chat_sessions(id) ON DELETE SET NULL;

CREATE INDEX idx_project_releases_chat_session_id ON project_releases(chat_session_id);
