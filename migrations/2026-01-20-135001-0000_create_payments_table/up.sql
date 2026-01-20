CREATE TABLE payments (
    id BIGSERIAL PRIMARY KEY,
    user_id BIGINT NOT NULL REFERENCES users(id),
    amount FLOAT8 NOT NULL,
    package_id BIGINT NOT NULL REFERENCES packages(id),
    package_title VARCHAR(255),
    package_slug VARCHAR(255),
    package_price FLOAT8,
    package_discount INTEGER CHECK (package_discount BETWEEN 0 AND 100),
    is_success BOOLEAN NOT NULL,
    gateway_metadata JSONB,
    expires_at TIMESTAMP WITH TIME ZONE
);

CREATE INDEX idx_payments_user_id ON payments (user_id);
CREATE INDEX idx_payments_package_id ON payments (package_id);