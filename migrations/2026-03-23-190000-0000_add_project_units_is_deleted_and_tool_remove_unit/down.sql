DELETE FROM chat_messages WHERE "type"::text = 'tool_remove_unit';

ALTER TABLE project_units DROP COLUMN IF EXISTS is_deleted;
