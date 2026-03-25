DELETE FROM chat_messages WHERE "type"::text IN ('tool_list_unit_contexts', 'tool_list_modules_for_unit_contexts');
