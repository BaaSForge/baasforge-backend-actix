CREATE TABLE packages (
    id BIGSERIAL PRIMARY KEY,
    title VARCHAR(255) NOT NULL,
    slug VARCHAR(255) NOT NULL UNIQUE,
    description TEXT,
    price FLOAT8 NOT NULL,
    discount_percent INTEGER CHECK (discount_percent BETWEEN 0 AND 100),
    is_trial BOOLEAN DEFAULT FALSE
);