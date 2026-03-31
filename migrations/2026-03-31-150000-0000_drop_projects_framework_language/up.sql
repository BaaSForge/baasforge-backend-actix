ALTER TABLE projects
    DROP COLUMN framework,
    DROP COLUMN language;

DROP TYPE framework_type;
DROP TYPE language_type;
