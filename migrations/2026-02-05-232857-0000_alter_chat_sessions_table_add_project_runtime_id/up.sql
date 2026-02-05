ALTER TABLE chat_sessions ADD COLUMN stage_project_runtime_id BIGINT REFERENCES project_runtimes(id);
