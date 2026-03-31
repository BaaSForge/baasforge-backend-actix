CREATE TYPE db_cluster_type AS ENUM ('neo4j', 'aws_neptune');

CREATE TABLE db_clusters (
    id UUID PRIMARY KEY DEFAULT uuidv7(),
    environment platform_environment_type NOT NULL,
    db_type db_cluster_type NOT NULL,
    max_capacity INTEGER NOT NULL,
    metadata JSONB
);

CREATE TYPE server_cluster_type AS ENUM ('k8s');

CREATE TABLE server_clusters (
    id UUID PRIMARY KEY DEFAULT uuidv7(),
    environment platform_environment_type NOT NULL,
    server_type server_cluster_type NOT NULL,
    max_capacity INTEGER NOT NULL,
    metadata JSONB
);

ALTER TABLE project_runtimes
    ADD COLUMN db_cluster_id UUID REFERENCES db_clusters (id),
    ADD COLUMN db_cluster_deployment_metadata JSONB,
    ADD COLUMN db_cluster_deployment_status deployment_status_type,
    ADD COLUMN db_cluster_deployment_is_healthy BOOLEAN,
    ADD COLUMN db_cluster_deployment_last_health TIMESTAMP WITH TIME ZONE,
    ADD COLUMN server_cluster_id UUID REFERENCES server_clusters (id),
    ADD COLUMN server_cluster_deployment_metadata JSONB,
    ADD COLUMN server_cluster_deployment_status deployment_status_type,
    ADD COLUMN server_cluster_deployment_is_healthy BOOLEAN,
    ADD COLUMN server_cluster_deployment_last_health TIMESTAMP WITH TIME ZONE;
