DROP INDEX IF EXISTS idx_user_tokens_refresh_token_id;

ALTER TABLE user_tokens
DROP COLUMN refresh_token_id;
