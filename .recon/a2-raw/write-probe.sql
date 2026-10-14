-- A2 侦察：附加索引的写入/标记成本（一次性副本，不触碰仓库或 Peri 会话库）。
.bail on
.timer on
PRAGMA journal_mode=WAL;
PRAGMA synchronous=NORMAL;
CREATE TABLE messages (
    message_id TEXT PRIMARY KEY, thread_id TEXT NOT NULL,
    role TEXT NOT NULL, content TEXT NOT NULL,
    truncated BOOLEAN NOT NULL DEFAULT 0, excluded BOOLEAN NOT NULL DEFAULT 0, projection TEXT
);
CREATE INDEX idx_messages_thread_id ON messages(thread_id);

SELECT '--- W0 追加 100k 行（只有 idx_messages_thread_id） ---' AS section;
INSERT INTO messages (message_id, thread_id, role, content, excluded, truncated)
SELECT 'w0-' || printf('%06d', x), 't0', 'user', 'body ' || x || ' ' || replace(hex(zeroblob(128)), '00', 'a'), 0, 0
FROM (WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM c WHERE x < 100000) SELECT x FROM c);

SELECT '--- W1 追加 100k 行（加 idx(thread_id, excluded) 后） ---' AS section;
CREATE INDEX idx_active_a ON messages(thread_id, excluded);
INSERT INTO messages (message_id, thread_id, role, content, excluded, truncated)
SELECT 'w1-' || printf('%06d', x), 't1', 'user', 'body ' || x || ' ' || replace(hex(zeroblob(128)), '00', 'a'), 0, 0
FROM (WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM c WHERE x < 100000) SELECT x FROM c);

SELECT '--- W2 追加 100k 行（换成 partial 索引后） ---' AS section;
DROP INDEX idx_active_a;
CREATE INDEX idx_active_b ON messages(thread_id) WHERE excluded = 0;
INSERT INTO messages (message_id, thread_id, role, content, excluded, truncated)
SELECT 'w2-' || printf('%06d', x), 't2', 'user', 'body ' || x || ' ' || replace(hex(zeroblob(128)), '00', 'a'), 0, 0
FROM (WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM c WHERE x < 100000) SELECT x FROM c);

SELECT '--- U0 标记 100k 行为 excluded=1（只有 partial 索引） ---' AS section;
UPDATE messages SET excluded = 1 WHERE thread_id = 't2';

SELECT '--- U1 标记 100k 行为 excluded=1（无附加索引） ---' AS section;
DROP INDEX idx_active_b;
UPDATE messages SET excluded = 1 WHERE thread_id = 't0';

SELECT '--- U2 标记 100k 行为 excluded=1（有 idx(thread_id, excluded)） ---' AS section;
CREATE INDEX idx_active_a ON messages(thread_id, excluded);
UPDATE messages SET excluded = 1 WHERE thread_id = 't1';

SELECT '--- 索引占用 ---' AS section;
SELECT name, SUM(pgsize) AS bytes FROM dbstat GROUP BY name ORDER BY bytes DESC;
