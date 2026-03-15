ALTER TABLE project_units ADD COLUMN chat_session_id UUID REFERENCES chat_sessions(id);
