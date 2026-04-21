ALTER TABLE chat_messages
    ADD COLUMN is_edit_disabled_due_to_production_release BOOLEAN NOT NULL DEFAULT false;
