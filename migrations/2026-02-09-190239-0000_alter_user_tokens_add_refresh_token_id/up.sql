ALTER TABLE user_tokens
ADD COLUMN refresh_token_id BIGINT NULL REFERENCES user_tokens(id);

CREATE INDEX idx_user_tokens_refresh_token_id ON user_tokens(refresh_token_id);
