-- 从启动失败的历史库提取的 DDL，不含用户行数据。
CREATE TABLE messages (
                message_id  TEXT PRIMARY KEY,
                thread_id   TEXT NOT NULL,
                role        TEXT NOT NULL,
                content     TEXT NOT NULL, truncated BOOLEAN NOT NULL DEFAULT 0, excluded BOOLEAN NOT NULL DEFAULT 0, projection TEXT,
                FOREIGN KEY (thread_id) REFERENCES threads(id) ON DELETE CASCADE
            );

CREATE TABLE thread_goals (
    thread_id         TEXT PRIMARY KEY,
    goal_id           TEXT NOT NULL,
    objective         TEXT NOT NULL,
    status            TEXT NOT NULL CHECK(status IN
                        ('active','paused','blocked','usage_limited','budget_limited','complete')),
    token_budget      INTEGER NULL,
    tokens_used       INTEGER NOT NULL DEFAULT 0,
    time_used_seconds INTEGER NOT NULL DEFAULT 0,
    created_at_ms     INTEGER NOT NULL,
    updated_at_ms     INTEGER NOT NULL,
    FOREIGN KEY(thread_id) REFERENCES threads(id) ON DELETE CASCADE
);

CREATE TABLE threads (
                id          TEXT PRIMARY KEY,
                title       TEXT,
                cwd         TEXT NOT NULL DEFAULT '',
                created_at  TEXT NOT NULL,
                updated_at  TEXT NOT NULL,
                message_count INTEGER NOT NULL DEFAULT 0
            , parent_thread_id TEXT, snapshot_at_message_id TEXT, hidden BOOLEAN NOT NULL DEFAULT 0, cancel_policy TEXT NOT NULL DEFAULT 'cascade', config TEXT, cached_context TEXT, agent_status TEXT NOT NULL DEFAULT 'active', context_cache_epoch INTEGER NOT NULL DEFAULT 0, frozen_context TEXT, inherited_context TEXT);

CREATE INDEX idx_messages_thread_id ON messages (thread_id ASC);

CREATE INDEX idx_threads_parent_thread_id ON threads (parent_thread_id);
PRAGMA user_version = 0;
