ALTER TABLE project_runtimes
    DROP COLUMN IF EXISTS db_cluster_id,
    DROP COLUMN IF EXISTS db_cluster_deployment_metadata,
    DROP COLUMN IF EXISTS db_cluster_deployment_status,
    DROP COLUMN IF EXISTS db_cluster_deployment_is_healthy,
    DROP COLUMN IF EXISTS db_cluster_deployment_last_health,
    DROP COLUMN IF EXISTS server_cluster_id,
    DROP COLUMN IF EXISTS server_cluster_deployment_metadata,
    DROP COLUMN IF EXISTS server_cluster_deployment_status,
    DROP COLUMN IF EXISTS server_cluster_deployment_is_healthy,
    DROP COLUMN IF EXISTS server_cluster_deployment_last_health;

DROP TABLE IF EXISTS server_clusters;
DROP TABLE IF EXISTS db_clusters;

DROP TYPE IF EXISTS server_cluster_type;
DROP TYPE IF EXISTS db_cluster_type;
