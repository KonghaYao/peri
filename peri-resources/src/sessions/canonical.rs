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
//! | 本机执行事实（只有本机） | `execution_runs` | 执行代际是设备事实；远端没有执行面，不建、也不该建 |
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
//! ## 列的归属细节
//!
//! `threads.cached_context` / `threads.context_cache_epoch` 是本机读取缓存的失效位：两端
//! 同列（形状一致），远端没有缓存消费者，因此它落库后保持缺省值、不参与远端读取。把它
//! 从这份 schema 里剔除只会让本机 DDL 变成「canonical + 追加列」的第二份形状，与目标相反。

use peri_acp_types::store::PersistedPayload;

/// 会话事实表。
pub(super) const THREADS_TABLE: &str = "threads";

/// canonical 历史表。
pub(super) const MESSAGES_TABLE: &str = "messages";

/// 不可变绑定引用的项目记录。
pub(super) const PROJECTS_TABLE: &str = "projects";

/// 不可变绑定引用的 workspace 记录。
pub(super) const WORKSPACES_TABLE: &str = "workspaces";

/// 不可变执行绑定。
pub(super) const SESSION_BINDINGS_TABLE: &str = "session_bindings";

/// canonical 表清单（父表在前，与 [`CREATE_TABLES_SQL`] 的顺序一致）。
pub(super) const CANONICAL_TABLES: &[&str] = &[
    THREADS_TABLE,
    MESSAGES_TABLE,
    PROJECTS_TABLE,
    WORKSPACES_TABLE,
    SESSION_BINDINGS_TABLE,
];

/// 建表语句：本机新库与远端初始化下发的**同一份清单**，一条语句一个元素。
///
/// 一条一个元素而不是拼成一段：远端执行器的语句单元就是一条语句（`StatementSpec`），
/// 多条语句塞进一个请求里只有第一条会被解析——形状必须按执行器的最小单位给出，本机再把
/// 它们合成一次 `raw_sql`（本机执行器支持多语句）。
///
/// 带 `IF NOT EXISTS`：本机旧库已存在这些表时是空操作（列由 `sqlite_store` 的迁移路径补齐），
/// 远端重复打开时同样是空操作。`REFERENCES` 子句保留原样：本机读写在同一连接上打开
/// `PRAGMA foreign_keys`，远端服务端不强制外键（读数恒为 0、且不可开启）——同一份 DDL 在两种
/// 执行器上的差别是**强制与否**，不是形状。顺序即依赖顺序：父表在前。
pub(super) const CREATE_TABLES: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS threads (
    id TEXT PRIMARY KEY, title TEXT, cwd TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL, updated_at TEXT NOT NULL, message_count INTEGER NOT NULL DEFAULT 0,
    parent_thread_id TEXT, snapshot_at_message_id TEXT, hidden BOOLEAN NOT NULL DEFAULT 0,
    cancel_policy TEXT NOT NULL DEFAULT 'cascade', config TEXT, cached_context TEXT,
    frozen_context TEXT, inherited_context TEXT, agent_status TEXT NOT NULL DEFAULT 'active',
    context_cache_epoch INTEGER NOT NULL DEFAULT 0
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
];

/// canonical 索引名（与 [`CREATE_INDEXES`] 的顺序一一对应）：形状核对按名字断言索引齐全。
#[cfg(test)]
pub(super) const CANONICAL_INDEXES: &[&str] = &[
    "idx_messages_thread_id",
    "idx_bindings_project",
    "idx_bindings_workspace",
    "idx_threads_updated",
];

/// 索引语句：必须在建表**与旧库补列之后**执行（`idx_threads_updated` 引用后补的列）。
pub(super) const CREATE_INDEXES: &[&str] = &[
    "CREATE INDEX IF NOT EXISTS idx_messages_thread_id ON messages(thread_id)",
    "CREATE INDEX IF NOT EXISTS idx_bindings_project ON session_bindings(project_id, thread_id)",
    "CREATE INDEX IF NOT EXISTS idx_bindings_workspace ON session_bindings(workspace_id, relative_cwd, thread_id)",
    "CREATE INDEX IF NOT EXISTS idx_threads_updated ON threads(updated_at DESC, id DESC) WHERE hidden = 0 AND message_count > 0",
];

/// 删除一条 `threads` 行之前必须显式清理的子表：子表名 + 语句。
///
/// **为什么两端都显式删**：远端执行器提供不了级联（`PRAGMA foreign_keys` 在服务端读数为 0、
/// 且不可开启；远端也没有 `pragma_foreign_key_check` 等价物）。一份删除逻辑跑在两种执行器上，
/// 唯一能共用的表达就是显式删除，因此本机侧也按同一份语句、同一顺序（先子后父）删。
/// 本机 DDL 里的 `ON DELETE CASCADE` 声明保留，但已退化为空操作式安全网。
pub(super) const THREAD_CHILD_DELETES: &[(&str, &str)] = &[
    (MESSAGES_TABLE, DELETE_MESSAGES_BY_THREAD_SQL),
    (SESSION_BINDINGS_TABLE, DELETE_BINDINGS_BY_THREAD_SQL),
];

/// 删除一个会话的全部历史行。
pub(super) const DELETE_MESSAGES_BY_THREAD_SQL: &str = "DELETE FROM messages WHERE thread_id = ?1";

/// 删除一个会话的不可变绑定行。
pub(super) const DELETE_BINDINGS_BY_THREAD_SQL: &str =
    "DELETE FROM session_bindings WHERE thread_id = ?1";

/// 删除 `threads` 行本身；只在 [`THREAD_CHILD_DELETES`] 之后执行（先子后父）。
pub(super) const DELETE_THREAD_ROW_SQL: &str = "DELETE FROM threads WHERE id = ?1";

/// `messages.role` 的取值：canonical payload 的领域规则，两端写同一列时用同一份派生。
///
/// 规则本身属于领域（`BaseMessage` → 角色名），放在这里只为了不让两个 adapter 各写一份。
pub(super) fn payload_role(payload: &PersistedPayload) -> &'static str {
    match payload {
        PersistedPayload::Message(message) => super::sqlite_store::role_of_message(message),
        PersistedPayload::SystemReminder { .. } => "system_reminder",
    }
}
