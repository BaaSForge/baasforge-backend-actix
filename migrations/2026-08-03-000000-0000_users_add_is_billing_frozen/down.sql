ALTER TABLE projects
    DROP COLUMN IF EXISTS is_billing_frozen;

ALTER TABLE users
    DROP COLUMN IF EXISTS is_billing_frozen;
