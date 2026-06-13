ALTER TABLE projects
    ADD COLUMN documentation_share_key VARCHAR(255),
    ADD COLUMN documentation_share_key_expires_at TIMESTAMPTZ;
