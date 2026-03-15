CREATE TYPE deployment_status_type AS ENUM ('deploying', 'stopped', 'pruned', 'up', 'failed');

CREATE TABLE project_runtimes (
    id UUID PRIMARY KEY DEFAULT uuidv7(),
    project_id UUID NOT NULL REFERENCES projects(id),
    environment platform_environment_type NOT NULL,
    db_cluster_id UUID REFERENCES db_clusters(id),
    db_cluster_deployment_metadata JSONB,
    db_cluster_deployment_status deployment_status_type,
    db_cluster_deployment_is_healthy BOOLEAN,
    db_cluster_deployment_last_health TIMESTAMP WITH TIME ZONE,
    server_cluster_id UUID REFERENCES server_clusters(id),
    server_cluster_deployment_metadata JSONB,
    server_cluster_deployment_status deployment_status_type,
    server_cluster_deployment_is_healthy BOOLEAN,
    server_cluster_deployment_last_health TIMESTAMP WITH TIME ZONE
);