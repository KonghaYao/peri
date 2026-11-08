//! canonical 会话 schema：两种执行器共用的一份形状与一份语句来源。
//!
//! 用户裁决的目标是「远端库完全 = 本地库的模式，两个存储模式一致」，落成工程语言就是
//! **一份 schema、两种 SQL 执行器**。本模块是那份 schema 的唯一来源：本机 SQLite adapter
//! （`sqlite_store`）与远端 over-the-wire adapter（`remote`）把**同一份 DDL** 下发到各自的
//! 连接上，表名、列名、列语义、排序键因此不可能各自漂移。两个 adapter 之间剩下的差别只有
//! 执行器本身（事务与超时、失败分类、远端独有的幂等账本）与各自的机制表。
//!
//! ## 谁的表进这份 schema
//!
//! | 归属 | 表 | 为什么 |
//! | --- | --- | --- |
//! | canonical 会话数据（两端都有） | `threads` / `messages` / `session_bindings` / `projects` / `workspaces` | 会话事实、canonical 历史、不可变执行绑定与它引用的 workspace 记录（契约 §4.1 允许数据 adapter 保存不可变 binding 记录） |
//! | 执行器机制（只有远端） | `peri_op_ledger` / `peri_store_meta` | 幂等资格的账本与版本标记；本机用 `PRAGMA user_version` 与本地事务表达同一件事 |
//!
//! ## 排序键是形状的一部分
//!
//! canonical 历史顺序 = **插入顺序**，两端都由 `messages` 的隐式 `rowid` 承载：写入按批
//! 顺序落行，读取与 rewind 一律显式 `ORDER BY rowid` / `WHERE rowid > ?`。远端曾经用一列
//! 显式 `ordinal` 表达同一件事，那是「远端不依赖引擎隐式列」的设计选择而非实测限制——
//! 真引擎上 `rowid` 可投影、按插入序、跨连接稳定（探测项 3a/3b/3c），因此统一到本机
//! 形状后该列与它的索引一并删除，两种执行器的语句文本才可能逐字一致。
//!
use peri_acp_types::{messages::BaseMessage, store::PersistedPayload};

/// 两种会话数据 adapter 的同一 schema 版本。
///
/// 压缩前 v4 内部用过 11..19 共 9 代，它们都没有进入正式发布（正式发布：agent-v3.19.x = 10、
/// agent-v3.18.0 = 6）。合并进 main 时收敛为**紧接正式基线 10 的一代 11**：外部库只有
/// 「≤10 的正式形状」与「10|11 的正式存储契约」两种输入，中间代形状不再被接受，
/// 也不再有中间代对象与迁移代码（见 `spec/issues/2026-10-10-schema-generation-compression.md`）。
///
/// 12 是 inherited 派生索引这一代：只新增两张**派生**表（可从 inherited 信封重建），
/// 11 的 canonical 形状与数据原样保留。
///
/// 13 是 Thread MQ 这一代：只新增 `thread_mq` 与其排序索引，12 的形状与数据原样保留
/// （见 `docs/design/thread-mq.md` §3）。
///
/// 14 是工作意图与上下文占用快照这一代：`threads` 只新增 `work_state` 与
/// `context_usage_tokens` 两列（见 `docs/design/session-durability.md` §2）；旧行升级后
/// 分别是 `idle` 与 `NULL`，旧 `agent_status = 'active'` 不构成自动续跑资格。
///
/// 15 是**首个删列表代**：`projects` 删 `object_identity` 并把唯一键收窄为 `UNIQUE(locator)`，
/// `workspaces` 删与 `discovery.root_identity` 重复的 `identity`，
/// `session_bindings` 删只写不读的 `evidence_origin`，同时退役两张可重建的 inherited 派生
/// 索引表；绑定行的 `discovery_snapshot` 收缩为最小执行根。因为被唯一键引用的列不能
/// `DROP COLUMN`，本机升级路径从「增量补列」升级为「重建」，远端契约推进到 v9。
pub(super) const CURRENT_SCHEMA_VERSION: i64 = 15;

/// 上一代当前形状（v14，含两列工作快照）的版本号。
///
/// 只作为升级来源：写打开按 v15 的重建链清理三张表并推进到 15。
pub(super) const PREVIOUS_CURRENT_SCHEMA_VERSION: i64 = 14;

/// 再上一代当前形状（v13，已有 `thread_mq`、缺 `threads` 的两列工作快照）的版本号。
///
/// 同样只作为升级来源：写打开先 Incremental 补齐两列，再跑同一条 v15 清理链。
pub(super) const LEGACY_CURRENT_SCHEMA_VERSION: i64 = 13;

/// 压缩前登记形状使用的版本号（历史事实，不随当前版本推进）。
pub(super) const LEGACY_REGISTRATION_SCHEMA_VERSION: i64 = 11;

/// 会话事实表。
pub(super) const THREADS_TABLE: &str = "threads";

/// canonical 历史表。
pub(super) const MESSAGES_TABLE: &str = "messages";

/// 机器身份表：执行归属的一极，`workspaces.machine_id` 的引用目标。
pub(super) const MACHINES_TABLE: &str = "machines";

/// 不可变绑定引用的项目记录。
pub(super) const PROJECTS_TABLE: &str = "projects";

/// 归属表：`(machine_id, path)` 是唯一身份，证据列只描述当前占用该路径的对象。
pub(super) const WORKSPACES_TABLE: &str = "workspaces";

/// 不可变执行绑定。
pub(super) const SESSION_BINDINGS_TABLE: &str = "session_bindings";

/// 会话关闭意图。
pub(super) const SESSION_CLOSE_INTENTS_TABLE: &str = "session_close_intents";

/// Thread MQ：每个 thread 尚未移交的消息（`docs/design/thread-mq.md` §3）。
///
/// 只保存待发事实：没有终态、撤回墓碑、消费留档、`seq`、展示状态或执行状态列。
/// 移交/取消/清理一律物理删除行。
pub(super) const THREAD_MQ_TABLE: &str = "thread_mq";

/// 压缩前形状独有的执行环境表；当前形状不再有它（迁移完成后删除）。
pub(super) const SESSION_ENVIRONMENTS_TABLE: &str = "session_environments";
pub(super) const OAUTH_CREDENTIALS_TABLE: &str = "mcp_oauth_credentials";

/// 迁移输入形状（正式基线 ≤10）的补齐语句：把每个会话的运行机器补进环境表。
///
/// 当前形状没有环境表，这条语句只服务「补齐到 V10 形状」这一步；规划不凭当前 cwd 猜测，
/// 必须先有环境行才读得到机器归属。
pub(super) const BACKFILL_ENVIRONMENTS_SQL: &str = "WITH RECURSIVE tree(thread_id, machine_id) AS (
    SELECT t.id, COALESCE(env.machine_id, ?1) FROM threads t
    LEFT JOIN session_environments env ON env.thread_id = t.id WHERE t.parent_thread_id IS NULL
    UNION ALL SELECT child.id, tree.machine_id FROM threads child JOIN tree ON child.parent_thread_id = tree.thread_id
) INSERT OR IGNORE INTO session_environments(thread_id, machine_id) SELECT thread_id, machine_id FROM tree";

/// 当前形状的表清单（父表在前，与 [`CREATE_TABLES`] 的顺序一致）。
pub(super) const CANONICAL_TABLES: &[&str] = &[
    MACHINES_TABLE,
    PROJECTS_TABLE,
    WORKSPACES_TABLE,
    THREADS_TABLE,
    MESSAGES_TABLE,
    SESSION_BINDINGS_TABLE,
    OAUTH_CREDENTIALS_TABLE,
    SESSION_CLOSE_INTENTS_TABLE,
    THREAD_MQ_TABLE,
];

/// 迁移输入形状（正式基线 ≤10）的表清单：建表预检与补齐按它判定。
pub(super) const V10_CANONICAL_TABLES: &[&str] = &[
    THREADS_TABLE,
    MESSAGES_TABLE,
    PROJECTS_TABLE,
    WORKSPACES_TABLE,
    SESSION_BINDINGS_TABLE,
    SESSION_ENVIRONMENTS_TABLE,
    OAUTH_CREDENTIALS_TABLE,
];

/// 压缩前形状（迁移输入，正式基线 ≤10）的建表清单：登记语义的 `workspaces` + 执行环境表。
///
/// 与当前形状的清单分开命名：这些语句是**历史形状的快照**，迁移实现改动时它们不该跟着漂移
/// ——`V10` 前缀即「正式基线 ≤10 的形状」，只在「补齐到 V10 形状」这一步使用。
pub(super) const V10_CREATE_ENVIRONMENTS_TABLE_SQL: &str =
    "CREATE TABLE IF NOT EXISTS session_environments (
    thread_id TEXT PRIMARY KEY REFERENCES threads(id) ON DELETE CASCADE,
    machine_id TEXT NOT NULL
)";

pub(super) const V10_CREATE_OAUTH_CREDENTIALS_TABLE_SQL: &str =
    "CREATE TABLE IF NOT EXISTS mcp_oauth_credentials (
    principal_id TEXT NOT NULL,
    machine_id TEXT NOT NULL,
    server_key TEXT NOT NULL,
    credentials_blob TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (principal_id, machine_id, server_key)
)";

pub(super) const V10_CREATE_TABLES: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS threads (
    id TEXT PRIMARY KEY, title TEXT, cwd TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL, updated_at TEXT NOT NULL, message_count INTEGER NOT NULL DEFAULT 0,
    parent_thread_id TEXT, snapshot_at_message_id TEXT, hidden BOOLEAN NOT NULL DEFAULT 0,
    cancel_policy TEXT NOT NULL DEFAULT 'cascade', config TEXT,
    frozen_context TEXT, inherited_context TEXT, agent_status TEXT NOT NULL DEFAULT 'active'
)",
    "CREATE TABLE IF NOT EXISTS messages (
    message_id TEXT PRIMARY KEY, thread_id TEXT NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
    role TEXT NOT NULL, content TEXT NOT NULL,
    truncated BOOLEAN NOT NULL DEFAULT 0, excluded BOOLEAN NOT NULL DEFAULT 0, projection TEXT
)",
    "CREATE TABLE IF NOT EXISTS projects (
    id TEXT PRIMARY KEY, locator TEXT NOT NULL, object_identity TEXT NOT NULL,
    UNIQUE(locator, object_identity)
)",
    "CREATE TABLE IF NOT EXISTS workspaces (
    id TEXT PRIMARY KEY, project_id TEXT NOT NULL REFERENCES projects(id),
    root TEXT NOT NULL, root_identity TEXT NOT NULL, discovery TEXT NOT NULL,
    UNIQUE(root, root_identity), UNIQUE(id, project_id)
)",
    "CREATE TABLE IF NOT EXISTS session_bindings (
    thread_id TEXT PRIMARY KEY REFERENCES threads(id) ON DELETE CASCADE,
    schema_version INTEGER NOT NULL,
    project_id TEXT NOT NULL, workspace_id TEXT NOT NULL, relative_cwd TEXT NOT NULL,
    FOREIGN KEY(workspace_id, project_id) REFERENCES workspaces(id, project_id)
)",
    V10_CREATE_ENVIRONMENTS_TABLE_SQL,
    V10_CREATE_OAUTH_CREDENTIALS_TABLE_SQL,
];

/// 压缩前形状的索引清单：必须在建表**与旧库补列之后**执行（`idx_threads_updated` 引用后补的列）。
pub(super) const V10_CREATE_INDEXES: &[&str] = &[
    "CREATE INDEX IF NOT EXISTS idx_messages_thread_id ON messages(thread_id)",
    "CREATE INDEX IF NOT EXISTS idx_bindings_project ON session_bindings(project_id, thread_id)",
    "CREATE INDEX IF NOT EXISTS idx_bindings_workspace ON session_bindings(workspace_id, relative_cwd, thread_id)",
    "CREATE INDEX IF NOT EXISTS idx_threads_updated ON threads(updated_at DESC, id DESC) WHERE hidden = 0 AND message_count > 0",
    "CREATE INDEX IF NOT EXISTS idx_session_environments_machine ON session_environments(machine_id, thread_id)",
];

/// 当前形状的目标表定义：本机新库、迁移终点与远端初始化下发的是同一份 DDL。
///
/// 顺序即依赖顺序（父表在前）：`machines` → `projects` → `workspaces` → `threads` →
/// `messages` → `session_bindings` → oauth → 关闭意图。
pub(super) const CREATE_MACHINES_TABLE_SQL: &str = "CREATE TABLE IF NOT EXISTS machines (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL CHECK(length(trim(name)) > 0),
    identity_kind TEXT NOT NULL CHECK(identity_kind IN ('known', 'legacy_unknown'))
)";
/// 当前 Machine 的登记行：即使尚无会话也应可查询，且不覆盖已有展示名。
///
/// 建当前形状的三条路径（本机新库、本机迁移终点、远端升级批次）下发同一条语句，
/// 「当前机器叫什么」只有这一处定义。
pub(super) const INSERT_CURRENT_MACHINE_SQL: &str =
    "INSERT OR IGNORE INTO machines(id, name, identity_kind) VALUES (?1, '我的电脑', 'known')";

/// Project 表：`workspaces.project_id` 的引用目标。
///
/// 项目身份就是 `locator`：v15 起唯一键收窄为 `UNIQUE(locator)`，不再把文件对象身份并进
/// 唯一性判定（inode 跨主机/容器不可通用，且从来没有读取方）。
pub(super) const CREATE_PROJECTS_TABLE_SQL: &str = "CREATE TABLE IF NOT EXISTS projects (
    id TEXT PRIMARY KEY, locator TEXT NOT NULL,
    UNIQUE(locator)
)";
/// Machine/path 归属表。它同时承载「该路径最近一次观测」的执行证据：
/// 归属键是 `(machine_id, path)`，证据列（`discovery`）只描述**当前占用该路径的对象**，
/// 不参与身份。v15 删除了与 `discovery.root_identity` 完全重复且无读者的 `identity` 列。
pub(super) const CREATE_WORKSPACES_TABLE_SQL: &str = "CREATE TABLE IF NOT EXISTS workspaces (
    id TEXT PRIMARY KEY,
    machine_id TEXT NOT NULL REFERENCES machines(id),
    path TEXT NOT NULL,
    path_source TEXT NOT NULL CHECK(path_source IN ('discovered', 'derived_legacy', 'unverified')),
    project_id TEXT REFERENCES projects(id),
    discovery TEXT,
    UNIQUE(machine_id, path)
)";
/// 会话事实表：归属由 `workspace_id` 唯一表达。
///
/// v14 的两个新列只能追加在末尾：旧库按同一列序 `ALTER TABLE ADD COLUMN` 补齐，形状判定
/// （`schema_shape`）对列序列逐项比对，迁移终点因此必须与新建库同形。
pub(super) const CREATE_THREADS_TABLE_SQL: &str = "CREATE TABLE IF NOT EXISTS threads (
    id TEXT PRIMARY KEY, title TEXT, cwd TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL, updated_at TEXT NOT NULL, message_count INTEGER NOT NULL DEFAULT 0,
    parent_thread_id TEXT, snapshot_at_message_id TEXT, hidden BOOLEAN NOT NULL DEFAULT 0,
    cancel_policy TEXT NOT NULL DEFAULT 'cascade', config TEXT,
    frozen_context TEXT, inherited_context TEXT, agent_status TEXT NOT NULL DEFAULT 'active',
    workspace_id TEXT NOT NULL REFERENCES workspaces(id),
    archived BOOLEAN NOT NULL DEFAULT 0 CHECK(archived IN (0, 1)),
    context_usage_tokens INTEGER
        CHECK(context_usage_tokens IS NULL OR context_usage_tokens >= 0),
    work_state TEXT NOT NULL DEFAULT 'idle'
        CHECK(work_state IN ('idle', 'working', 'stopped'))
)";

/// v14 相对 v13 的 `threads` 增量列（名称与列定义）；本机与远端迁移共用同一份文本。
///
/// `context_usage_tokens` 允许 NULL（未知不是零占用）；`work_state` 的非空默认值让旧行
/// 升级后落在 `idle`，不会因为旧 `agent_status` 被批量唤醒。
pub(super) const THREADS_V14_COLUMNS: &[(&str, &str)] = &[
    (
        "context_usage_tokens",
        "INTEGER CHECK(context_usage_tokens IS NULL OR context_usage_tokens >= 0)",
    ),
    (
        "work_state",
        "TEXT NOT NULL DEFAULT 'idle' CHECK(work_state IN ('idle', 'working', 'stopped'))",
    ),
];
/// 绑定表：workspace 归属由 `threads.workspace_id` 唯一表达，绑定行只保存自己的
/// 不可变执行证据。没有指向归属表的外键——归属关系不靠外键维护（两端都不强制）。
///
/// `discovery_snapshot` 是最小执行根（v15 起）：native 只有 `root`，virtual 另带
/// `kind`；完整探测诊断只留在 `workspaces.discovery`。
pub(super) const CREATE_BINDINGS_TABLE_SQL: &str = "CREATE TABLE IF NOT EXISTS session_bindings (
    thread_id TEXT PRIMARY KEY REFERENCES threads(id) ON DELETE CASCADE,
    schema_version INTEGER NOT NULL,
    project_id TEXT NOT NULL, workspace_id TEXT NOT NULL, relative_cwd TEXT NOT NULL,
    discovery_snapshot TEXT
)";
pub(super) const CREATE_OAUTH_CREDENTIALS_TABLE_SQL: &str =
    "CREATE TABLE IF NOT EXISTS mcp_oauth_credentials (
    principal_id TEXT NOT NULL,
    workspace_id TEXT NOT NULL REFERENCES workspaces(id),
    server_key TEXT NOT NULL,
    credentials_blob TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY(principal_id, workspace_id, server_key)
)";

/// 建表语句：本机新库与远端初始化下发的**同一份清单**，一条语句一个元素。
///
/// 一条一个元素而不是拼成一段：远端执行器的语句单元就是一条语句（`StatementSpec`），
/// 多条语句塞进一个请求里只有第一条会被解析——形状必须按执行器的最小单位给出，本机再把
/// 它们合成一次 `raw_sql`（本机执行器支持多语句）。
///
/// 带 `IF NOT EXISTS`：本机旧库已存在这些表时是空操作（列由 `sqlite_store` 的迁移路径补齐），
/// 远端重复打开时同样是空操作。`REFERENCES` 子句保留原样：本机读写在同一连接上打开
/// `PRAGMA foreign_keys`，远端服务端不强制外键（读数恒为 0、且不可开启）——同一份 DDL 在两种
/// 执行器上的差别是**强制与否**，不是形状。
pub(super) const CREATE_TABLES: &[&str] = &[
    CREATE_MACHINES_TABLE_SQL,
    CREATE_PROJECTS_TABLE_SQL,
    CREATE_WORKSPACES_TABLE_SQL,
    CREATE_THREADS_TABLE_SQL,
    "CREATE TABLE IF NOT EXISTS messages (
    message_id TEXT PRIMARY KEY, thread_id TEXT NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
    role TEXT NOT NULL, content TEXT NOT NULL,
    truncated BOOLEAN NOT NULL DEFAULT 0, excluded BOOLEAN NOT NULL DEFAULT 0, projection TEXT
)",
    CREATE_BINDINGS_TABLE_SQL,
    CREATE_OAUTH_CREDENTIALS_TABLE_SQL,
    CREATE_SESSION_CLOSE_INTENTS_TABLE_SQL,
    CREATE_THREAD_MQ_TABLE_SQL,
];

/// 显式关闭已接纳的持久事实；不保存异步任务目录。
pub(super) const CREATE_SESSION_CLOSE_INTENTS_TABLE_SQL: &str =
    "CREATE TABLE IF NOT EXISTS session_close_intents (
    thread_id TEXT PRIMARY KEY REFERENCES threads(id) ON DELETE CASCADE,
    requested_at TEXT NOT NULL
)";

/// Thread MQ 表：每个 thread 尚未移交的消息。
///
/// `PRIMARY KEY(thread_id, id)` 表达「行属于 thread」，`UNIQUE(id)` 让同一稳定消息 ID
/// 在整个库里只有一个待发副本——跨 thread 的同 ID 复用因此是真实的身份冲突，而不是
/// 两个各自合法的队列项。`REFERENCES threads(id)` 与其余 canonical 表同源：本机是级联
/// 安全网，远端执行器不强制外键，两端都按 [`THREAD_CHILD_DELETES`] 显式删子行。
///
/// `STRICT` 把这四类列的取值钉在声明类型上：JSON 与时间戳都是按列契约写入的文本，
/// 类型漂移（数字时间戳、BLOB payload）会让读取端解码错误而不是静默容忍。
pub(super) const CREATE_THREAD_MQ_TABLE_SQL: &str = "CREATE TABLE IF NOT EXISTS thread_mq (
    thread_id TEXT NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
    id TEXT NOT NULL,
    enqueued_at TEXT NOT NULL,
    kind TEXT NOT NULL,
    source TEXT NOT NULL,
    policy_json TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    original_draft TEXT,
    PRIMARY KEY(thread_id, id),
    UNIQUE(id)
) STRICT";

/// 消费顺序键 `(enqueued_at, id)` 的覆盖索引：`mq_query` 的排序与 `after` 过滤都走它。
pub(super) const CREATE_THREAD_MQ_ORDER_INDEX_SQL: &str =
    "CREATE INDEX IF NOT EXISTS idx_thread_mq_order ON thread_mq(thread_id, enqueued_at, id)";

/// 索引语句：必须在建表**与旧库补列之后**执行（`idx_threads_updated` 引用后补的列）。
pub(super) const CREATE_INDEXES: &[&str] = &[
    "CREATE INDEX IF NOT EXISTS idx_messages_thread_id ON messages(thread_id)",
    "CREATE INDEX IF NOT EXISTS idx_bindings_project ON session_bindings(project_id, thread_id)",
    "CREATE INDEX IF NOT EXISTS idx_bindings_workspace ON session_bindings(workspace_id, relative_cwd, thread_id)",
    "CREATE INDEX IF NOT EXISTS idx_threads_updated ON threads(updated_at DESC, id DESC) WHERE hidden = 0 AND message_count > 0",
    "CREATE INDEX IF NOT EXISTS idx_threads_workspace_archived ON threads(workspace_id, archived, updated_at DESC, id DESC) WHERE parent_thread_id IS NULL AND message_count > 0",
    // 有效范围读取（`thread_id = ? AND excluded = 0`，见 `sqlite_store::context`）只靠
    // `idx_messages_thread_id` 时要逐行过滤 `excluded`：触达行数 = 线程全部消息数（含归档段）。
    // 加上 `excluded` 这一列后，命中的行数与「有效条数」同阶——A2 实测：20 万行的线程上，
    // 恢复查询的触达行数从 200 003 降到 3，`EXPLAIN QUERY PLAN` 走
    // `SEARCH messages USING INDEX idx_messages_active (thread_id=? AND excluded=?)`。
    // 两个索引并存：`idx_messages_thread_id` 仍是只按线程取全部行的路径（`ORDER BY rowid` 扫描）。
    "CREATE INDEX IF NOT EXISTS idx_messages_active ON messages(thread_id, excluded)",
    // MQ 读取与 `after` 水位过滤的唯一排序键。
    CREATE_THREAD_MQ_ORDER_INDEX_SQL,
];

/// 迁移期重建 `threads` 的暂存表（`threads_rebuild`）：`workspace_id` 必须落在 **NOT NULL**
/// 上，而 SQLite 与远端执行器都不能把已有列改成 `NOT NULL`，只能重建。远端服务端的 SQL
/// 能力边界里没有实测过 `ALTER TABLE ... RENAME`（现有远端迁移只用建表/删表/加列/插入/
/// 更新），因此两端共用同一条重建路径：建暂存 → 搬入 → 删旧 → 建新 → 搬回 → 删暂存。
///
/// 暂存表的建表文本与 [`CREATE_THREADS_TABLE_SQL`] 同形（只有表名不同）但**不带**
/// `IF NOT EXISTS`——重建只允许作用在本迁移刚删掉的位置：同名对象已存在时必须显式失败，
/// 否则会把使用者的同名表当成暂存表搬空、再在最后一步删掉它。
pub(super) const CREATE_REBUILD_THREADS_TABLE_SQL: &str = "CREATE TABLE threads_rebuild (
    id TEXT PRIMARY KEY, title TEXT, cwd TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL, updated_at TEXT NOT NULL, message_count INTEGER NOT NULL DEFAULT 0,
    parent_thread_id TEXT, snapshot_at_message_id TEXT, hidden BOOLEAN NOT NULL DEFAULT 0,
    cancel_policy TEXT NOT NULL DEFAULT 'cascade', config TEXT,
    frozen_context TEXT, inherited_context TEXT, agent_status TEXT NOT NULL DEFAULT 'active',
    workspace_id TEXT NOT NULL REFERENCES workspaces(id),
    archived BOOLEAN NOT NULL DEFAULT 0 CHECK(archived IN (0, 1)),
    context_usage_tokens INTEGER
        CHECK(context_usage_tokens IS NULL OR context_usage_tokens >= 0),
    work_state TEXT NOT NULL DEFAULT 'idle'
        CHECK(work_state IN ('idle', 'working', 'stopped'))
)";

/// 重建搬运：`threads` → 暂存表（列清单与当前 `threads` 形状一致，由
/// `schema_shape_test::rebuild_statements_carry_the_canonical_threads_columns` 断言）。
pub(super) const INSERT_THREADS_INTO_REBUILD_SQL: &str = "INSERT INTO threads_rebuild (id, title, cwd, created_at, updated_at, message_count, parent_thread_id, snapshot_at_message_id, hidden, cancel_policy, config, frozen_context, inherited_context, agent_status, workspace_id, archived, context_usage_tokens, work_state) SELECT id, title, cwd, created_at, updated_at, message_count, parent_thread_id, snapshot_at_message_id, hidden, cancel_policy, config, frozen_context, inherited_context, agent_status, workspace_id, archived, context_usage_tokens, work_state FROM threads";

/// 重建搬运：新建的 `threads` ← 暂存表（列清单同上）。
pub(super) const INSERT_REBUILD_INTO_THREADS_SQL: &str = "INSERT INTO threads (id, title, cwd, created_at, updated_at, message_count, parent_thread_id, snapshot_at_message_id, hidden, cancel_policy, config, frozen_context, inherited_context, agent_status, workspace_id, archived, context_usage_tokens, work_state) SELECT id, title, cwd, created_at, updated_at, message_count, parent_thread_id, snapshot_at_message_id, hidden, cancel_policy, config, frozen_context, inherited_context, agent_status, workspace_id, archived, context_usage_tokens, work_state FROM threads_rebuild";

pub(super) const SELECT_OAUTH_CREDENTIAL_SQL: &str = "SELECT credentials_blob FROM mcp_oauth_credentials WHERE principal_id = ?1 AND workspace_id = ?2 AND server_key = ?3";
pub(super) const UPSERT_OAUTH_CREDENTIAL_SQL: &str = "INSERT INTO mcp_oauth_credentials(principal_id, workspace_id, server_key, credentials_blob, updated_at) VALUES (?1, ?2, ?3, ?4, ?5) ON CONFLICT(principal_id, workspace_id, server_key) DO UPDATE SET credentials_blob = excluded.credentials_blob, updated_at = excluded.updated_at";
pub(super) const DELETE_OAUTH_CREDENTIAL_SQL: &str = "DELETE FROM mcp_oauth_credentials WHERE principal_id = ?1 AND workspace_id = ?2 AND server_key = ?3";
pub(super) const DELETE_ALL_OAUTH_CREDENTIALS_SQL: &str =
    "DELETE FROM mcp_oauth_credentials WHERE principal_id = ?1 AND workspace_id = ?2";
pub(super) const LIST_OAUTH_CREDENTIALS_SQL: &str = "SELECT server_key FROM mcp_oauth_credentials WHERE principal_id = ?1 AND workspace_id = ?2 ORDER BY server_key";

/// 删除一条 `threads` 行之前必须显式清理的子表：子表名 + 语句。
///
/// **为什么两端都显式删**：远端执行器提供不了级联（`PRAGMA foreign_keys` 在服务端读数为 0、
/// 且不可开启；远端也没有 `pragma_foreign_key_check` 等价物）。一份删除逻辑跑在两种执行器上，
/// 唯一能共用的表达就是显式删除，因此本机侧也按同一份语句、同一顺序（先子后父）删。
/// 本机 DDL 里的 `ON DELETE CASCADE` 声明保留，但已退化为空操作式安全网。
pub(super) const THREAD_CHILD_DELETES: &[(&str, &str)] = &[
    (MESSAGES_TABLE, DELETE_MESSAGES_BY_THREAD_SQL),
    (SESSION_BINDINGS_TABLE, DELETE_BINDINGS_BY_THREAD_SQL),
    (
        "session_close_intents",
        "DELETE FROM session_close_intents WHERE thread_id = ?1",
    ),
    (THREAD_MQ_TABLE, DELETE_THREAD_MQ_BY_THREAD_SQL),
];

/// 删除一个会话的全部历史行。
pub(super) const DELETE_MESSAGES_BY_THREAD_SQL: &str = "DELETE FROM messages WHERE thread_id = ?1";

/// 删除一个会话的不可变绑定行。
pub(super) const DELETE_BINDINGS_BY_THREAD_SQL: &str =
    "DELETE FROM session_bindings WHERE thread_id = ?1";

/// 删除一个会话的全部待发 MQ 行（thread 删除与 rewind 共用）。
pub(super) const DELETE_THREAD_MQ_BY_THREAD_SQL: &str =
    "DELETE FROM thread_mq WHERE thread_id = ?1";

/// 删除 `threads` 行本身；只在 [`THREAD_CHILD_DELETES`] 之后执行（先子后父）。
pub(super) const DELETE_THREAD_ROW_SQL: &str = "DELETE FROM threads WHERE id = ?1";

/// `messages.role` 的取值：canonical payload 的领域规则，两端写同一列时用同一份派生。
///
/// 规则本身属于领域（`BaseMessage` → 角色名），放在这里只为了不让两个 adapter 各写一份。
pub(super) fn payload_role(payload: &PersistedPayload) -> &'static str {
    match payload {
        PersistedPayload::Message(message) => role_of(message),
        PersistedPayload::SystemReminder { .. } => "system_reminder",
    }
}

pub(in crate::sessions) fn role_of(msg: &BaseMessage) -> &'static str {
    match msg {
        BaseMessage::Human { .. } => "user",
        BaseMessage::Ai { .. } => "assistant",
        BaseMessage::System { .. } => "system",
        BaseMessage::Tool { .. } => "tool",
    }
}

pub(crate) fn extract_title<'message>(
    msgs: impl IntoIterator<Item = &'message BaseMessage>,
) -> Option<String> {
    use peri_acp_types::messages::{ContentBlock, MessageContent};
    for msg in msgs {
        if let BaseMessage::Human { content, .. } = msg {
            let text = match content {
                MessageContent::Text(text) => std::borrow::Cow::Borrowed(text.as_str()),
                MessageContent::Blocks(blocks) => std::borrow::Cow::Owned(
                    blocks
                        .iter()
                        .filter_map(|b| {
                            if let ContentBlock::Text { text } = b {
                                Some(text.as_str())
                            } else {
                                None
                            }
                        })
                        .collect::<Vec<_>>()
                        .join(" "),
                ),
                MessageContent::Raw(_) => continue,
            };
            let title: String = text.chars().take(50).collect();
            if !title.is_empty() {
                return Some(title);
            }
        }
    }
    None
}

pub(super) const REMOVE_EXECUTION_RECOVERY_TABLES: &[&str] = &[
    "DROP TABLE IF EXISTS session_work_commands",
    "DROP TABLE IF EXISTS session_work_receipts",
    "DROP TABLE IF EXISTS session_work_events",
    "DROP TABLE IF EXISTS session_work_state",
    "DROP TABLE IF EXISTS session_control_receipts",
    "DROP TABLE IF EXISTS session_control_state",
];

// ─── v15 三表重建：删列与收紧唯一键 ──────────────────────────────────────────
//
// v15 是本 schema 的第一个**删列**代：`projects` 丢 `object_identity` 并把唯一键收窄为
// `UNIQUE(locator)`，`workspaces` 丢与 `discovery.root_identity` 重复的 `identity`，
// `session_bindings` 丢只写不读的 `evidence_origin`。被唯一键引用的列在两种执行器上都不能
// `DROP COLUMN`，表级 UNIQUE 也不能后补，因此统一走 `threads_rebuild` 同款重建链：
// 建暂存 → 搬入 → 删旧 → 建新（同一份 DDL）→ 搬回 → 删暂存。本机与远端共用这份文本。
//
// 暂存表**不带** `IF NOT EXISTS`：重建只允许作用在本迁移刚删掉的位置，同名对象已存在时
// 必须显式失败，否则会把使用者的同名表当成暂存表搬空、再在最后一步删掉它。

/// v15 重建暂存的 `projects`（与 [`CREATE_PROJECTS_TABLE_SQL`] 同形，只换表名）。
pub(super) const CREATE_REBUILD_PROJECTS_TABLE_SQL: &str = "CREATE TABLE projects_v15 (
    id TEXT PRIMARY KEY, locator TEXT NOT NULL,
    UNIQUE(locator)
)";

/// v15 重建搬运：`projects` → 暂存表。`object_identity` 不搬。
pub(super) const INSERT_PROJECTS_INTO_REBUILD_SQL: &str =
    "INSERT INTO projects_v15 (id, locator) SELECT id, locator FROM projects";

/// v15 重建搬运：新建的 `projects` ← 暂存表。
pub(super) const INSERT_REBUILD_INTO_PROJECTS_SQL: &str =
    "INSERT INTO projects (id, locator) SELECT id, locator FROM projects_v15";

/// v15 重建暂存的 `workspaces`（与 [`CREATE_WORKSPACES_TABLE_SQL`] 同形）。
pub(super) const CREATE_REBUILD_WORKSPACES_TABLE_SQL: &str = "CREATE TABLE workspaces_v15 (
    id TEXT PRIMARY KEY,
    machine_id TEXT NOT NULL REFERENCES machines(id),
    path TEXT NOT NULL,
    path_source TEXT NOT NULL CHECK(path_source IN ('discovered', 'derived_legacy', 'unverified')),
    project_id TEXT REFERENCES projects(id),
    discovery TEXT,
    UNIQUE(machine_id, path)
)";

/// v15 重建搬运：`workspaces` → 暂存表。`identity` 不搬。
pub(super) const INSERT_WORKSPACES_INTO_REBUILD_SQL: &str = "INSERT INTO workspaces_v15 (id, machine_id, path, path_source, project_id, discovery) SELECT id, machine_id, path, path_source, project_id, discovery FROM workspaces";

/// v15 重建搬运：新建的 `workspaces` ← 暂存表。
pub(super) const INSERT_REBUILD_INTO_WORKSPACES_SQL: &str = "INSERT INTO workspaces (id, machine_id, path, path_source, project_id, discovery) SELECT id, machine_id, path, path_source, project_id, discovery FROM workspaces_v15";

/// v15 重建暂存的 `session_bindings`（与 [`CREATE_BINDINGS_TABLE_SQL`] 同形）。
pub(super) const CREATE_REBUILD_BINDINGS_TABLE_SQL: &str = "CREATE TABLE session_bindings_v15 (
    thread_id TEXT PRIMARY KEY REFERENCES threads(id) ON DELETE CASCADE,
    schema_version INTEGER NOT NULL,
    project_id TEXT NOT NULL, workspace_id TEXT NOT NULL, relative_cwd TEXT NOT NULL,
    discovery_snapshot TEXT
)";

/// v15 重建搬运：新建的 `session_bindings` ← 暂存表。
///
/// 暂存表本身由 Rust 侧逐行投影后插入（`binding_evidence::project`）：`discovery_snapshot`
/// 要收缩为最小执行根，SQL 里做不了这件事（远端 json1 未实测），因此这里只有搬回一条。
pub(super) const INSERT_REBUILD_INTO_BINDINGS_SQL: &str = "INSERT INTO session_bindings (thread_id, schema_version, project_id, workspace_id, relative_cwd, discovery_snapshot) SELECT thread_id, schema_version, project_id, workspace_id, relative_cwd, discovery_snapshot FROM session_bindings_v15";

/// v15 退役的两张 derived 表（payload 必须先于 snapshot：payload 依赖 snapshot 定位）。
///
/// v15 之前它们只是**派生**对象：内容全部可从 `threads.inherited_context` 重建，读取本来
/// 就有全列扫描回退。删除只移除索引，不改动 `inherited_context` 的任何字节。
pub(super) const DROP_INHERITED_INDEX_TABLES: &[&str] = &[
    "DROP TABLE IF EXISTS inherited_payload_index",
    "DROP TABLE IF EXISTS inherited_snapshot_index",
];
