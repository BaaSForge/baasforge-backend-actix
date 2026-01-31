DO $$
DECLARE
    env TEXT;
BEGIN
    SELECT value INTO env FROM app_settings WHERE key = 'env';

    IF env = 'production' THEN
        RAISE NOTICE 'Skipping migration on production';
        RETURN;
    END IF;

    INSERT INTO packages (title, slug, description, price, discount_percent, is_trial, is_active)
    VALUES
        ('Free Trial', 'free-trial', 'some description', 0, 100, TRUE, TRUE),
        ('Basic', 'basic', 'some description', 10, 10, FALSE, TRUE),
        ('Pro', 'pro', 'some description', 30, 20, FALSE, TRUE);
END $$;
