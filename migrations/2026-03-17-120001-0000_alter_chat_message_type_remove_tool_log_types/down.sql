-- Restore the enum with tool_log_debug and tool_log_message.
ALTER TYPE chat_message_type RENAME TO chat_message_type_old;

CREATE TYPE chat_message_type AS ENUM (
    'text',
    'units',
    'unit',
    'generating',
    'generating_failed',
    'generating_succeed',
    'tool_fetch_relevant_units',
    'tool_fetch_latest_releases',
    'tool_log_debug',
    'tool_log_message'
);

ALTER TABLE chat_messages
    ALTER COLUMN "type" TYPE chat_message_type
    USING ("type"::text::chat_message_type);

DROP TYPE chat_message_type_old;
