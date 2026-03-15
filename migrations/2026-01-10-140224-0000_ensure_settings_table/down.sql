DO $$
BEGIN
    IF EXISTS (
        SELECT FROM information_schema.tables 
        WHERE table_name = 'app_settings'
    ) THEN
        RAISE EXCEPTION 'Table "app_settings" does exist, read README.md for more information';
    END IF;
END $$;