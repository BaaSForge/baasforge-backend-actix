ALTER TABLE projects DROP COLUMN IF EXISTS last_cost_calculated_at;

ALTER TABLE users DROP COLUMN IF EXISTS last_subscription_cost_calculated_at;
