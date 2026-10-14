-- A2 侦察：进程内计时（同一进程、同一页缓存），三条生产 SQL 的 WHERE/索引路径逐次对照。
-- COUNT 包裹只是为了不打印 3×64KiB 正文；内层查询的计划与触达行数不变（见 ab-probe.out 的 scanstats）。
.timer on
.eqp off
.print '=== 基线：无附加索引 ==='
SELECT COUNT(*) FROM (SELECT message_id, content FROM messages WHERE thread_id = 'probe-thread' AND excluded = 0 ORDER BY rowid);
SELECT COUNT(*) FROM (SELECT message_id, content FROM messages WHERE thread_id = 'probe-thread' AND excluded = 0 ORDER BY rowid);
SELECT COUNT(*) FROM (SELECT message_id, content FROM messages WHERE thread_id = 'probe-thread' AND excluded = 0 ORDER BY rowid);
SELECT COUNT(*) FROM (SELECT message_id, truncated, excluded, projection FROM messages WHERE thread_id = 'probe-thread' AND excluded = 0 AND (truncated = 1 OR projection IS NOT NULL));
SELECT COUNT(*) FROM (SELECT message_id, truncated, excluded, projection FROM messages WHERE thread_id = 'probe-thread' AND excluded = 0 AND (truncated = 1 OR projection IS NOT NULL));
SELECT COUNT(*) FROM (SELECT message_id, truncated, excluded, projection FROM messages WHERE thread_id = 'probe-thread' AND excluded = 0 AND (truncated = 1 OR projection IS NOT NULL));
SELECT COUNT(*) FROM (SELECT message_id, rowid FROM messages WHERE thread_id = 'probe-thread' AND excluded = 0 ORDER BY rowid ASC);
SELECT COUNT(*) FROM (SELECT message_id, rowid FROM messages WHERE thread_id = 'probe-thread' AND excluded = 0 ORDER BY rowid ASC);
SELECT COUNT(*) FROM (SELECT message_id, rowid FROM messages WHERE thread_id = 'probe-thread' AND excluded = 0 ORDER BY rowid ASC);
