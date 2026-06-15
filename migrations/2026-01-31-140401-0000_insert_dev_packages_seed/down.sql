DO $$
DECLARE
    env TEXT;
BEGIN
    SELECT value INTO env FROM app_settings WHERE key = 'env';

    IF env = 'production' THEN
        RAISE NOTICE 'Skipping rollback on production';
        RETURN;
    END IF;

    DELETE FROM packages
    WHERE slug IN ('free-trial', 'basic', 'pro', 'ultra');
END $$;
