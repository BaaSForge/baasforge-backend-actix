CREATE TYPE server_cluster_type AS ENUM ('k8s');

CREATE TABLE server_clusters (
    id UUID PRIMARY KEY DEFAULT uuidv7(),
    environment platform_environment_type NOT NULL,
    server_type server_cluster_type NOT NULL,
    max_capacity INTEGER NOT NULL,
    metadata JSONB
);