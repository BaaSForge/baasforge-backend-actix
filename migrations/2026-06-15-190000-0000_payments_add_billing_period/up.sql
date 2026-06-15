CREATE TYPE billing_period_type AS ENUM ('monthly', 'annual');

ALTER TABLE payments
    ADD COLUMN billing_period billing_period_type NOT NULL DEFAULT 'monthly';
