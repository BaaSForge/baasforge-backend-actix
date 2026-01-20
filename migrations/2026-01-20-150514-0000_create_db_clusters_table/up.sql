CREATE TYPE platform_environment_type AS ENUM ('stage', 'prod');
CREATE TYPE db_cluster_type AS ENUM ('neo4j', 'aws_neptune');

CREATE TABLE db_clusters (
    id BIGSERIAL PRIMARY KEY,
    environment platform_environment_type NOT NULL,
    db_type db_cluster_type NOT NULL,
    max_capacity INTEGER NOT NULL,
    metadata JSONB
);