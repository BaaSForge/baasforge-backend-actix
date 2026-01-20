CREATE TYPE platform_type AS ENUM ('google', 'github');

CREATE TABLE users (
    id BIGSERIAL PRIMARY KEY,
    platform platform_type NOT NULL,
    id_in_platform VARCHAR(255) NOT NULL UNIQUE,
    full_name VARCHAR(255)
);