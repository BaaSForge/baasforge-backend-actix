CREATE TYPE framework_type AS ENUM ('mvc');

CREATE TYPE language_type AS ENUM ('rust');

CREATE TABLE projects (
    id BIGSERIAL PRIMARY KEY,
    framework framework_type NOT NULL,
    owner_user_id BIGINT NOT NULL REFERENCES users(id),
    name VARCHAR(255) NOT NULL,
    slug VARCHAR(255) NOT NULL UNIQUE,
    language language_type NOT NULL
);