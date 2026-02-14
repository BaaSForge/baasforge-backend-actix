ALTER TABLE payments
ADD CONSTRAINT payments_package_tier_key UNIQUE (package_tier);
