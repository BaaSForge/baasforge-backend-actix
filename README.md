## Preparing app settings (before running ./migration-up.dev)
```
CREATE TABLE app_settings (key TEXT PRIMARY KEY, value TEXT);

INSERT INTO app_settings (key, value) VALUES ('env', 'development'); # or 'production'
```

## Creating new migration file
```
diesel migration generate create_users_table
```