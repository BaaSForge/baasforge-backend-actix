DO $$
BEGIN
    IF NOT EXISTS (
        SELECT FROM information_schema.tables 
        WHERE table_name = 'app_settings'
    ) THEN
        RAISE EXCEPTION 'Table "app_settings" does not exist, read README.md for more information';
    END IF;
END $$;