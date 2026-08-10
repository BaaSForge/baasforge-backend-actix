-- Manual analytics snapshot (run once → one table)
-- Notes:
--   users/projects last_3_days: UUID v7 timestamp from id (no created_at)
--   units last_3_days: successful create/modify tool messages
--   compute_usd: $8e-9 per GB·ms (LLM/storage omitted)

WITH metrics AS (
  SELECT
    (SELECT COUNT(*) FROM users) AS users_total,
    (SELECT COUNT(*) FROM users
     WHERE to_timestamp((
       (get_byte(uuid_send(id), 0)::bigint << 40)
       | (get_byte(uuid_send(id), 1)::bigint << 32)
       | (get_byte(uuid_send(id), 2)::bigint << 24)
       | (get_byte(uuid_send(id), 3)::bigint << 16)
       | (get_byte(uuid_send(id), 4)::bigint << 8)
       | get_byte(uuid_send(id), 5)::bigint
     ) / 1000.0) >= NOW() - INTERVAL '3 days') AS users_last_3_days,

    (SELECT COUNT(*) FROM projects) AS projects_total,
    (SELECT COUNT(*) FROM projects
     WHERE to_timestamp((
       (get_byte(uuid_send(id), 0)::bigint << 40)
       | (get_byte(uuid_send(id), 1)::bigint << 32)
       | (get_byte(uuid_send(id), 2)::bigint << 24)
       | (get_byte(uuid_send(id), 3)::bigint << 16)
       | (get_byte(uuid_send(id), 4)::bigint << 8)
       | get_byte(uuid_send(id), 5)::bigint
     ) / 1000.0) >= NOW() - INTERVAL '3 days') AS projects_last_3_days,

    (SELECT COUNT(*) FROM chat_sessions) AS chat_sessions_total,
    (SELECT COUNT(*) FROM chat_sessions
     WHERE created_at >= NOW() - INTERVAL '3 days') AS chat_sessions_last_3_days,

    (SELECT COUNT(*) FROM project_units WHERE revision = 1) AS units_created_total,
    (SELECT COUNT(*) FROM chat_messages
     WHERE type = 'tool_create_unit'
       AND status = 'completed'
       AND payload ? 'created_unit'
       AND payload->'created_unit' IS NOT NULL
       AND payload->>'created_unit' <> 'null'
       AND created_at >= NOW() - INTERVAL '3 days') AS units_created_last_3_days,

    (SELECT COUNT(*) FROM project_units WHERE revision > 1) AS units_modified_total,
    (SELECT COUNT(*) FROM chat_messages
     WHERE type = 'tool_modify_unit'
       AND status = 'completed'
       AND payload ? 'revised_unit'
       AND payload->'revised_unit' IS NOT NULL
       AND payload->>'revised_unit' <> 'null'
       AND created_at >= NOW() - INTERVAL '3 days') AS units_modified_last_3_days,

    (SELECT COALESCE(SUM(quantity) * 8e-9, 0)
     FROM project_usage_entries
     WHERE kind = 'compute' AND metric = 'compute.gb_ms') AS compute_usd_total,
    (SELECT COALESCE(SUM(quantity) * 8e-9, 0)
     FROM project_usage_entries
     WHERE kind = 'compute'
       AND metric = 'compute.gb_ms'
       AND recorded_at >= NOW() - INTERVAL '3 days') AS compute_usd_last_3_days
)
SELECT v.metric, v.total, v.last_3_days
FROM metrics,
LATERAL (VALUES
  ('users',          users_total::numeric,          users_last_3_days::numeric),
  ('projects',       projects_total::numeric,       projects_last_3_days::numeric),
  ('chat_sessions',  chat_sessions_total::numeric,  chat_sessions_last_3_days::numeric),
  ('units_created',  units_created_total::numeric,  units_created_last_3_days::numeric),
  ('units_modified', units_modified_total::numeric, units_modified_last_3_days::numeric),
  ('compute_usd',    ROUND(compute_usd_total::numeric, 6), ROUND(compute_usd_last_3_days::numeric, 6))
) AS v(metric, total, last_3_days);
