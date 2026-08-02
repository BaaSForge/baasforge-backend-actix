DROP INDEX IF EXISTS idx_payments_user_active;

ALTER TABLE payments
    DROP COLUMN cancelled_at,
    DROP COLUMN created_at;
