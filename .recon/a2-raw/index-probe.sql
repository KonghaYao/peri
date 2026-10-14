-- A2 侦察：假设性索引的收益（只在 /tmp 风格的一次性副本上做，不触碰仓库或任何 Peri 会话库）。
.bail on
.eqp full
.scanstats on
.timer on
PRAGMA journal_mode=WAL;
PRAGMA synchronous=NORMAL;

SELECT '=== S0 副本基线（无新索引） ===' AS section;
SELECT message_id, content FROM messages WHERE thread_id = 'probe-thread' AND excluded = 0 ORDER BY rowid;
SELECT message_id, truncated, excluded, projection FROM messages WHERE thread_id = 'probe-thread'
  AND excluded = 0 AND (truncated = 1 OR projection IS NOT NULL);
SELECT message_id, rowid FROM messages WHERE thread_id = 'probe-thread' AND excluded = 0 ORDER BY rowid ASC;

SELECT '=== S1 建索引 A：(thread_id, excluded) ===' AS section;
CREATE INDEX idx_messages_thread_active ON messages(thread_id, excluded);
SELECT message_id, content FROM messages WHERE thread_id = 'probe-thread' AND excluded = 0 ORDER BY rowid;
SELECT message_id, truncated, excluded, projection FROM messages WHERE thread_id = 'probe-thread'
  AND excluded = 0 AND (truncated = 1 OR projection IS NOT NULL);
SELECT message_id, rowid FROM messages WHERE thread_id = 'probe-thread' AND excluded = 0 ORDER BY rowid ASC;
DROP INDEX idx_messages_thread_active;

SELECT '=== S2 建索引 B：partial (thread_id) WHERE excluded = 0 ===' AS section;
CREATE INDEX idx_messages_thread_active ON messages(thread_id) WHERE excluded = 0;
SELECT message_id, content FROM messages WHERE thread_id = 'probe-thread' AND excluded = 0 ORDER BY rowid;
SELECT message_id, truncated, excluded, projection FROM messages WHERE thread_id = 'probe-thread'
  AND excluded = 0 AND (truncated = 1 OR projection IS NOT NULL);
SELECT message_id, rowid FROM messages WHERE thread_id = 'probe-thread' AND excluded = 0 ORDER BY rowid ASC;

SELECT '=== S3 索引清单与占用 ===' AS section;
SELECT name, sql FROM sqlite_schema WHERE type = 'index' AND tbl_name = 'messages';
SELECT 'messages 页数' AS fact, SUM(pgsize) AS bytes FROM dbstat WHERE name = 'messages';
SELECT 'partial 索引页数' AS fact, SUM(pgsize) AS bytes FROM dbstat WHERE name = 'idx_messages_thread_active';
SELECT 'thread_id 索引页数' AS fact, SUM(pgsize) AS bytes FROM dbstat WHERE name = 'idx_messages_thread_id';
