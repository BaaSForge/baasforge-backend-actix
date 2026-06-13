ALTER TABLE projects
    ADD COLUMN last_cost_calculated_at TIMESTAMPTZ;

ALTER TABLE users
    ADD COLUMN last_subscription_cost_calculated_at TIMESTAMPTZ;
