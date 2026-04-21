DELETE FROM chat_messages WHERE "type"::text = 'reject_controller_box';

ALTER TYPE chat_message_type RENAME TO chat_message_type_old;

CREATE TYPE chat_message_type AS ENUM (
    'text',
    'units',
    'tool_fetch_relevant_units',
    'tool_fetch_latest_releases',
    'tool_analyze_architecture',
    'tool_finish_architecture_analysis',
    'tool_analyze_solution',
    'tool_finish_solution_analysis',
    'tool_implement_solution',
    'tool_finish_implementation',
    'tool_create_unit',
    'tool_modify_unit',
    'tool_remove_unit',
    'tool_list_unit_contexts',
    'tool_list_modules_for_unit_contexts',
    'release_controller_box'
);

ALTER TABLE chat_messages
    ALTER COLUMN "type" TYPE chat_message_type
    USING ("type"::text::chat_message_type);

DROP TYPE chat_message_type_old;
