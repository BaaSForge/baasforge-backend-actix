CREATE TABLE top_ups (
    id UUID PRIMARY KEY DEFAULT uuidv7(),
    user_id UUID NOT NULL REFERENCES users(id),
    amount FLOAT8 NOT NULL CHECK (amount > 0),
    gateway_metadata JSONB,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX idx_top_ups_user_id ON top_ups (user_id);

CREATE TABLE withdrawals (
    id UUID PRIMARY KEY DEFAULT uuidv7(),
    user_id UUID NOT NULL REFERENCES users(id),
    amount FLOAT8 NOT NULL CHECK (amount > 0),
    gateway_metadata JSONB,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX idx_withdrawals_user_id ON withdrawals (user_id);
