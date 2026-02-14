ALTER TABLE packages
ADD CONSTRAINT packages_tier_key UNIQUE (tier);
