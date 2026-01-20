CREATE TYPE user_token_type AS ENUM ('access', 'refresh');

CREATE TABLE user_tokens (
    id BIGSERIAL PRIMARY KEY,
    user_id BIGINT NOT NULL REFERENCES users(id),
    token_type user_token_type NOT NULL,
    token TEXT NOT NULL UNIQUE,
    expires_at TIMESTAMPTZ NOT NULL,
    revoked BOOLEAN DEFAULT false
);

CREATE INDEX idx_user_tokens_user_id ON user_tokens(user_id);