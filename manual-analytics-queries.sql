-- Manual analytics queries (copy/paste one at a time into your DB client)
-- Note: users & projects have no created_at; time filters use UUID v7 timestamp from id.
-- Note: project_units have no timestamps; "last 3 days" uses successful create/modify tool messages.

-- =============================================================================
-- Users
-- =============================================================================

-- Number of users
SELECT COUNT(*) AS users_total
FROM users;

-- Number of users in last 3 days
SELECT COUNT(*) AS users_last_3_days
FROM users
WHERE to_timestamp(
        (
          (get_byte(uuid_send(id), 0)::bigint << 40)
          | (get_byte(uuid_send(id), 1)::bigint << 32)
          | (get_byte(uuid_send(id), 2)::bigint << 24)
          | (get_byte(uuid_send(id), 3)::bigint << 16)
          | (get_byte(uuid_send(id), 4)::bigint << 8)
          | get_byte(uuid_send(id), 5)::bigint
        ) / 1000.0
      ) >= NOW() - INTERVAL '3 days';

-- =============================================================================
-- Projects
-- =============================================================================

-- Number of projects
SELECT COUNT(*) AS projects_total
FROM projects;

-- Number of projects in last 3 days
SELECT COUNT(*) AS projects_last_3_days
FROM projects
WHERE to_timestamp(
        (
          (get_byte(uuid_send(id), 0)::bigint << 40)
          | (get_byte(uuid_send(id), 1)::bigint << 32)
          | (get_byte(uuid_send(id), 2)::bigint << 24)
          | (get_byte(uuid_send(id), 3)::bigint << 16)
          | (get_byte(uuid_send(id), 4)::bigint << 8)
          | get_byte(uuid_send(id), 5)::bigint
        ) / 1000.0
      ) >= NOW() - INTERVAL '3 days';

-- =============================================================================
-- Chat sessions
-- =============================================================================

-- Number of chat sessions made by users
SELECT COUNT(*) AS chat_sessions_total
FROM chat_sessions;

-- Number of chat sessions made by users in last 3 days
SELECT COUNT(*) AS chat_sessions_last_3_days
FROM chat_sessions
WHERE created_at >= NOW() - INTERVAL '3 days';

-- =============================================================================
-- Units created (revision = 1)
-- =============================================================================

-- Number of units created
SELECT COUNT(*) AS units_created_total
FROM project_units
WHERE revision = 1;

-- Number of units created in last 3 days
SELECT COUNT(*) AS units_created_last_3_days
FROM chat_messages
WHERE type = 'tool_create_unit'
  AND status = 'completed'
  AND payload ? 'created_unit'
  AND payload->'created_unit' IS NOT NULL
  AND payload->>'created_unit' <> 'null'
  AND created_at >= NOW() - INTERVAL '3 days';

-- =============================================================================
-- Units modified (revision > 1)
-- =============================================================================

-- Number of units modified
SELECT COUNT(*) AS units_modified_total
FROM project_units
WHERE revision > 1;

-- Number of units modified in last 3 days
SELECT COUNT(*) AS units_modified_last_3_days
FROM chat_messages
WHERE type = 'tool_modify_unit'
  AND status = 'completed'
  AND payload ? 'revised_unit'
  AND payload->'revised_unit' IS NOT NULL
  AND payload->>'revised_unit' <> 'null'
  AND created_at >= NOW() - INTERVAL '3 days';

-- =============================================================================
-- Usages / costs (project_usage_entries rows)
-- =============================================================================

-- Total usages of all users (cost rows)
SELECT COUNT(*) AS usage_rows_total
FROM project_usage_entries;

-- Total usages of all users (cost rows) in last 3 days
SELECT COUNT(*) AS usage_rows_last_3_days
FROM project_usage_entries
WHERE recorded_at >= NOW() - INTERVAL '3 days';
