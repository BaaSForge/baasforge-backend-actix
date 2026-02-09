DO $$
DECLARE
    env TEXT;
BEGIN
    SELECT value INTO env FROM app_settings WHERE key = 'env';

    IF env = 'production' THEN
        RAISE NOTICE 'Skipping migration on production';
        RETURN;
    END IF;

    INSERT INTO packages (title, slug, tier, description, price, discount_percent, is_trial, is_active)
    VALUES
        ('Free Trial', 'free-trial', 1, E'- feature first here\n- feature second also here\n- feature third', 0, 100, TRUE, TRUE),
        ('Basic', 'basic', 2, E'- feature first here\n- feature second also here\n- feature third', 10, 10, FALSE, TRUE),
        ('Pro', 'pro', 3, E'- feature first here\n- feature second also here\n- feature third', 30, 20, FALSE, TRUE);
END $$;
