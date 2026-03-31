CREATE TYPE framework_type AS ENUM ('mvc');

CREATE TYPE language_type AS ENUM ('rust');

ALTER TABLE projects
    ADD COLUMN framework framework_type NOT NULL DEFAULT 'mvc',
    ADD COLUMN language language_type NOT NULL DEFAULT 'rust';

ALTER TABLE projects
    ALTER COLUMN framework DROP DEFAULT,
    ALTER COLUMN language DROP DEFAULT;
