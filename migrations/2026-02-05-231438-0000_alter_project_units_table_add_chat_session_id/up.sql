ALTER TABLE project_units ADD COLUMN chat_session_id BIGINT REFERENCES chat_sessions(id);
