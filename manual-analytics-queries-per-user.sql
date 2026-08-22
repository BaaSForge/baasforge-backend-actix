-- Manual analytics snapshot per user (page of 5, sorted by full_name)
-- Change `page` (1-based) in params.
-- Notes:
--   users/projects last_3_days: UUID v7 timestamp from id (no created_at)
--   units last_3_days: successful create/modify tool messages
--   compute_usd: $8e-9 per GB·ms (LLM/storage omitted)

WITH params AS (
  SELECT 1 AS page, 5 AS page_size
),
page_users AS (
  SELECT u.id, u.full_name
  FROM users u
  ORDER BY u.full_name NULLS LAST, u.id
  LIMIT (SELECT page_size FROM params)
  OFFSET ((SELECT page FROM params) - 1) * (SELECT page_size FROM params)
),
metrics AS (
  SELECT
    pu.id AS user_id,
    pu.full_name,

    (SELECT COUNT(*) FROM projects p WHERE p.owner_user_id = pu.id) AS projects_total,
    (SELECT COUNT(*) FROM projects p
     WHERE p.owner_user_id = pu.id
       AND to_timestamp((
         (get_byte(uuid_send(p.id), 0)::bigint << 40)
         | (get_byte(uuid_send(p.id), 1)::bigint << 32)
         | (get_byte(uuid_send(p.id), 2)::bigint << 24)
         | (get_byte(uuid_send(p.id), 3)::bigint << 16)
         | (get_byte(uuid_send(p.id), 4)::bigint << 8)
         | get_byte(uuid_send(p.id), 5)::bigint
       ) / 1000.0) >= NOW() - INTERVAL '3 days') AS projects_last_3_days,

    (SELECT COUNT(*) FROM chat_sessions cs WHERE cs.user_id = pu.id) AS chat_sessions_total,
    (SELECT COUNT(*) FROM chat_sessions cs
     WHERE cs.user_id = pu.id
       AND cs.created_at >= NOW() - INTERVAL '3 days') AS chat_sessions_last_3_days,

    (SELECT COUNT(*) FROM project_units un
     JOIN projects p ON p.id = un.project_id
     WHERE p.owner_user_id = pu.id AND un.revision = 1) AS units_created_total,
    (SELECT COUNT(*) FROM chat_messages m
     JOIN chat_sessions cs ON cs.id = m.chat_session_id
     WHERE cs.user_id = pu.id
       AND m.type = 'tool_create_unit'
       AND m.status = 'completed'
       AND m.payload ? 'created_unit'
       AND m.payload->'created_unit' IS NOT NULL
       AND m.payload->>'created_unit' <> 'null'
       AND m.created_at >= NOW() - INTERVAL '3 days') AS units_created_last_3_days,

    (SELECT COUNT(*) FROM project_units un
     JOIN projects p ON p.id = un.project_id
     WHERE p.owner_user_id = pu.id AND un.revision > 1) AS units_modified_total,
    (SELECT COUNT(*) FROM chat_messages m
     JOIN chat_sessions cs ON cs.id = m.chat_session_id
     WHERE cs.user_id = pu.id
       AND m.type = 'tool_modify_unit'
       AND m.status = 'completed'
       AND m.payload ? 'revised_unit'
       AND m.payload->'revised_unit' IS NOT NULL
       AND m.payload->>'revised_unit' <> 'null'
       AND m.created_at >= NOW() - INTERVAL '3 days') AS units_modified_last_3_days,

    (SELECT COALESCE(SUM(e.quantity) * 8e-9, 0)
     FROM project_usage_entries e
     JOIN projects p ON p.id = e.project_id
     WHERE p.owner_user_id = pu.id
       AND e.kind = 'compute'
       AND e.metric = 'compute.gb_ms') AS compute_usd_total,
    (SELECT COALESCE(SUM(e.quantity) * 8e-9, 0)
     FROM project_usage_entries e
     JOIN projects p ON p.id = e.project_id
     WHERE p.owner_user_id = pu.id
       AND e.kind = 'compute'
       AND e.metric = 'compute.gb_ms'
       AND e.recorded_at >= NOW() - INTERVAL '3 days') AS compute_usd_last_3_days
  FROM page_users pu
)
SELECT m.user_id, m.full_name, v.metric, v.total, v.last_3_days
FROM metrics m,
LATERAL (VALUES
  (1, 'projects',       m.projects_total::numeric,       m.projects_last_3_days::numeric),
  (2, 'chat_sessions',  m.chat_sessions_total::numeric,  m.chat_sessions_last_3_days::numeric),
  (3, 'units_created',  m.units_created_total::numeric,  m.units_created_last_3_days::numeric),
  (4, 'units_modified', m.units_modified_total::numeric, m.units_modified_last_3_days::numeric),
  (5, 'compute_usd',    ROUND(m.compute_usd_total::numeric, 6), ROUND(m.compute_usd_last_3_days::numeric, 6))
) AS v(ord, metric, total, last_3_days)
ORDER BY m.full_name NULLS LAST, m.user_id, v.ord;
