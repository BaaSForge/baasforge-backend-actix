CREATE TABLE packages (
    id UUID PRIMARY KEY DEFAULT uuidv7(),
    title VARCHAR(255) NOT NULL,
    slug VARCHAR(255) NOT NULL UNIQUE,
    tier INTEGER NOT NULL UNIQUE CHECK (tier > 0),
    description TEXT,
    price FLOAT8 NOT NULL,
    discount_percent INTEGER CHECK (discount_percent BETWEEN 0 AND 100),
    is_trial BOOLEAN DEFAULT FALSE
);