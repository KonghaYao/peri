-- 无 Authorization 的请求统一映射到公共 actor，供本地/测试 MCP 客户端直接使用。
-- 显式携带无效 Bearer 的请求不会降级到该身份。
INSERT INTO actors
  (actor_id, kind, agent_session_id, name, metadata, inbox_head_seq, inbox_min_seq, pending_count, created_at, updated_at)
SELECT
  'agt_public',
  'agent',
  'sess_public',
  'Public',
  '{}',
  0,
  0,
  0,
  CAST(strftime('%s', 'now') AS INTEGER) * 1000,
  CAST(strftime('%s', 'now') AS INTEGER) * 1000
WHERE NOT EXISTS (SELECT 1 FROM actors WHERE actor_id = 'agt_public');
