DELETE FROM chat_messages WHERE "type"::text = 'tool_finish_architecture_analysis';

ALTER TYPE chat_message_type RENAME TO chat_message_type_old;

CREATE TYPE chat_message_type AS ENUM (
    'text',
    'units',
    'tool_fetch_relevant_units',
    'tool_fetch_latest_releases',
    'tool_analyze_architecture'
);

ALTER TABLE chat_messages
    ALTER COLUMN "type" TYPE chat_message_type
    USING ("type"::text::chat_message_type);

DROP TYPE chat_message_type_old;
