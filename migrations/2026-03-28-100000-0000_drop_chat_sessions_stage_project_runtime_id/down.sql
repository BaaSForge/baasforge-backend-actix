ALTER TABLE chat_sessions
    ADD COLUMN stage_project_runtime_id UUID REFERENCES project_runtimes (id);

ALTER TABLE chat_sessions
    ADD CONSTRAINT stage_project_runtime_id_unique_key UNIQUE (stage_project_runtime_id);
