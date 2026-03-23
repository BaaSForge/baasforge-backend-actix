ALTER TABLE project_units
    ADD COLUMN is_deleted BOOLEAN NOT NULL DEFAULT false;

ALTER TYPE chat_message_type ADD VALUE IF NOT EXISTS 'tool_remove_unit';
