-- A2 侦察：恢复热路径三条 SQL 的计划与「实际触达行数」(scanstats)。
-- 只读副本；schema 与生产 canonical.rs 的 messages/threads DDL 同形。
.bail on
.eqp full
.scanstats on
.timer on
PRAGMA journal_mode=WAL;
PRAGMA synchronous=NORMAL;
PRAGMA foreign_keys=ON;

CREATE TABLE threads (
    id TEXT PRIMARY KEY, title TEXT, cwd TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL, updated_at TEXT NOT NULL, message_count INTEGER NOT NULL DEFAULT 0,
    parent_thread_id TEXT, snapshot_at_message_id TEXT, hidden BOOLEAN NOT NULL DEFAULT 0,
    cancel_policy TEXT NOT NULL DEFAULT 'cascade', config TEXT,
    frozen_context TEXT, inherited_context TEXT, agent_status TEXT NOT NULL DEFAULT 'active',
    workspace_id TEXT NOT NULL, archived BOOLEAN NOT NULL DEFAULT 0
);
CREATE TABLE messages (
    message_id TEXT PRIMARY KEY, thread_id TEXT NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
    role TEXT NOT NULL, content TEXT NOT NULL,
    truncated BOOLEAN NOT NULL DEFAULT 0, excluded BOOLEAN NOT NULL DEFAULT 0, projection TEXT
);
CREATE INDEX idx_messages_thread_id ON messages(thread_id);

INSERT INTO threads (id, title, cwd, created_at, updated_at, message_count, workspace_id)
VALUES ('probe-thread', 'probe', '/tmp', '2026-10-13T00:00:00Z', '2026-10-13T00:00:00Z', 0, 'probe-ws');

-- 归档段（excluded = 1）：与生产夹具同形的「旧前缀被排除」
INSERT INTO messages (message_id, thread_id, role, content, excluded, truncated)
SELECT 'arch-' || printf('%06d', x), 'probe-thread', 'user',
       'archived #' || x || ' ' || replace(hex(zeroblob(512)), '00', 'a'), 1, 0
FROM (WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM c WHERE x < 200000)
      SELECT x FROM c);

-- 有效段（excluded = 0）：3 条 64 KiB 正文，rowid 位于归档之后
INSERT INTO messages (message_id, thread_id, role, content, excluded, truncated)
VALUES ('live-000001', 'probe-thread', 'user', 'live #1 ' || replace(hex(zeroblob(32768)), '00', 'L'), 0, 0),
       ('live-000002', 'probe-thread', 'user', 'live #2 ' || replace(hex(zeroblob(32768)), '00', 'L'), 0, 1),
       ('live-000003', 'probe-thread', 'user', 'live #3 ' || replace(hex(zeroblob(32768)), '00', 'L'), 0, 0);

UPDATE threads SET message_count = (SELECT COUNT(*) FROM messages WHERE thread_id = 'probe-thread')
WHERE id = 'probe-thread';

SELECT '--- 索引清单 ---' AS section;
SELECT name, sql FROM sqlite_schema WHERE type = 'index' AND tbl_name = 'messages';
SELECT '--- idx_messages_thread_id 的列 ---' AS section;
SELECT * FROM pragma_index_xinfo('idx_messages_thread_id');
SELECT '--- 行数探针（= 计划里 SEARCH 范围的行数上界） ---' AS section;
SELECT COUNT(*) AS rows_in_range FROM messages WHERE thread_id = 'probe-thread';
SELECT COUNT(*) AS excluded_rows FROM messages WHERE thread_id = 'probe-thread' AND excluded = 1;
SELECT MIN(rowid) AS first_active_rowid, MAX(rowid) AS last_rowid FROM messages WHERE thread_id = 'probe-thread' AND excluded = 0;

SELECT '=== Q1 ACTIVE_PAYLOADS_SQL ===' AS section;
SELECT message_id, content FROM messages WHERE thread_id = 'probe-thread' AND excluded = 0 ORDER BY rowid;

SELECT '=== Q2 ACTIVE_FLAGS_SQL ===' AS section;
SELECT message_id, truncated, excluded, projection FROM messages WHERE thread_id = 'probe-thread'
  AND excluded = 0 AND (truncated = 1 OR projection IS NOT NULL);

SELECT '=== Q3 ACTIVE_ENTRY_POSITIONS_SQL ===' AS section;
SELECT message_id, rowid FROM messages WHERE thread_id = 'probe-thread' AND excluded = 0 ORDER BY rowid ASC;

SELECT '=== V1 只用 rowid 下界（字面量） ===' AS section;
SELECT message_id, content FROM messages
 WHERE thread_id = 'probe-thread' AND excluded = 0 AND rowid >= 199998 ORDER BY rowid;

SELECT '=== V1e 同一语句的 EXPLAIN QUERY PLAN（看 rowid 是否变成索引范围约束） ===' AS section;
EXPLAIN QUERY PLAN
SELECT message_id, content FROM messages
 WHERE thread_id = 'probe-thread' AND excluded = 0 AND rowid >= 199998 ORDER BY rowid;

SELECT '=== V2 rowid 下界用 message_id 子查询解析（分页语句同形的写法） ===' AS section;
SELECT message_id, content FROM messages
 WHERE thread_id = 'probe-thread' AND excluded = 0 AND rowid >= (
    SELECT m.rowid FROM messages m WHERE m.thread_id = 'probe-thread' AND m.message_id = 'live-000001')
 ORDER BY rowid;

SELECT '=== V2e 同一语句的 EXPLAIN QUERY PLAN ===' AS section;
EXPLAIN QUERY PLAN
SELECT message_id, content FROM messages
 WHERE thread_id = 'probe-thread' AND excluded = 0 AND rowid >= (
    SELECT m.rowid FROM messages m WHERE m.thread_id = 'probe-thread' AND m.message_id = 'live-000001')
 ORDER BY rowid;
