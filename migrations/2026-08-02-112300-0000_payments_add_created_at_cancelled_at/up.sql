ALTER TABLE payments
    ADD COLUMN created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    ADD COLUMN cancelled_at TIMESTAMPTZ NULL;

CREATE INDEX idx_payments_user_active
    ON payments (user_id)
    WHERE is_success = true AND cancelled_at IS NULL;
