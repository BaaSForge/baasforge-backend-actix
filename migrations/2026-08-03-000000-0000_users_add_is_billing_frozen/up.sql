ALTER TABLE users
    ADD COLUMN is_billing_frozen BOOLEAN NOT NULL DEFAULT false;

ALTER TABLE projects
    ADD COLUMN is_billing_frozen BOOLEAN NOT NULL DEFAULT false;
