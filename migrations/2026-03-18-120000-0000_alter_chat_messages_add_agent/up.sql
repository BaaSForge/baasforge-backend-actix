CREATE TYPE chat_agent_type AS ENUM (
    'planner',
    'architecture_analyzer',
    'solution_analyzer',
    'developer'
);

-- Agent is only set for AI role messages; user messages have NULL.
ALTER TABLE chat_messages
    ADD COLUMN agent chat_agent_type NULL;
