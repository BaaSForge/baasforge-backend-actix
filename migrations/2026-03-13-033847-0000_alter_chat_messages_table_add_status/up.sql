CREATE TYPE chat_message_status AS ENUM (
    'generating',
    'failed',
    'stopped',
    'completed'
);

ALTER TABLE chat_messages
    ADD COLUMN status chat_message_status NOT NULL DEFAULT 'completed';
