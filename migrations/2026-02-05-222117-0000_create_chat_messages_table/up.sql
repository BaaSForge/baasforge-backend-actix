CREATE TYPE chat_session_role_type AS ENUM ('user', 'ai');

CREATE TYPE chat_message_type AS ENUM (
    'text',
    'units',
    'unit',
    'generating',
    'generating_failed',
    'generating_succeed'
);

CREATE TABLE chat_messages (
    id BIGSERIAL PRIMARY KEY,
    chat_session_id BIGINT NOT NULL REFERENCES chat_sessions(id),
    role chat_session_role_type NOT NULL,
    type chat_message_type NOT NULL,
    payload JSONB,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
