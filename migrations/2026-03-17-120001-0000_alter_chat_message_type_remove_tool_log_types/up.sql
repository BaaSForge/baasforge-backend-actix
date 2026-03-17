-- Remove any messages that use the deprecated tool log types.
DELETE FROM chat_messages WHERE "type" IN ('tool_log_debug', 'tool_log_message');

-- Recreate the enum without the removed values (DROP VALUE is not supported in all PostgreSQL setups).
ALTER TYPE chat_message_type RENAME TO chat_message_type_old;

CREATE TYPE chat_message_type AS ENUM (
    'text',
    'units',
    'tool_fetch_relevant_units',
    'tool_fetch_latest_releases'
);

ALTER TABLE chat_messages
    ALTER COLUMN "type" TYPE chat_message_type
    USING ("type"::text::chat_message_type);

DROP TYPE chat_message_type_old;
