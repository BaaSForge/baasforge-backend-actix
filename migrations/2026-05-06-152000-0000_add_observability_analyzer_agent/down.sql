DELETE FROM chat_messages WHERE agent::text = 'observability_analyzer';

DELETE FROM chat_messages WHERE "type"::text IN ('tool_analyze_observability', 'tool_finish_observability_analysis');

ALTER TYPE chat_agent_type RENAME TO chat_agent_type_old;

CREATE TYPE chat_agent_type AS ENUM (
    'planner',
    'architecture_analyzer',
    'solution_analyzer',
    'developer'
);

ALTER TABLE chat_messages
    ALTER COLUMN agent TYPE chat_agent_type
    USING (agent::text::chat_agent_type);

DROP TYPE chat_agent_type_old;

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
    'release_controller_box',
    'reject_controller_box'
);

ALTER TABLE chat_messages
    ALTER COLUMN "type" TYPE chat_message_type
    USING ("type"::text::chat_message_type);

DROP TYPE chat_message_type_old;
