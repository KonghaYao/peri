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
pub(super) const CURRENT_SCHEMA_VERSION: i64 = 17;

/// 会话事实表。
pub(super) const THREADS_TABLE: &str = "threads";
pub(super) const THREAD_COLUMN_NAMES: &[&str] = &[
    "id",
    "title",
    "cwd",
    "created_at",
    "updated_at",
    "message_count",
    "parent_thread_id",
    "snapshot_at_message_id",
    "hidden",
    "cancel_policy",
    "config",
    "frozen_context",
    "inherited_context",
    "agent_status",
];

/// canonical 历史表。
pub(super) const MESSAGES_TABLE: &str = "messages";
pub(super) const MESSAGE_COLUMN_NAMES: &[&str] = &[
    "message_id",
    "thread_id",
    "role",
    "content",
    "truncated",
    "excluded",
    "projection",
];

/// 不可变绑定引用的项目记录。
pub(super) const PROJECTS_TABLE: &str = "projects";

/// 不可变绑定引用的 workspace 记录。
pub(super) const WORKSPACES_TABLE: &str = "workspaces";

/// 不可变执行绑定。
pub(super) const SESSION_BINDINGS_TABLE: &str = "session_bindings";
pub(super) const SESSION_ENVIRONMENTS_TABLE: &str = "session_environments";
pub(super) const OAUTH_CREDENTIALS_TABLE: &str = "mcp_oauth_credentials";
pub(super) const CREATE_OAUTH_CREDENTIALS_TABLE_SQL: &str =
    "CREATE TABLE IF NOT EXISTS mcp_oauth_credentials (
    principal_id TEXT NOT NULL,
    machine_id TEXT NOT NULL,
    server_key TEXT NOT NULL,
    credentials_blob TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (principal_id, machine_id, server_key)
)";
pub(super) const BACKFILL_ENVIRONMENTS_SQL: &str = "WITH RECURSIVE tree(thread_id, machine_id) AS (
    SELECT t.id, COALESCE(env.machine_id, ?1) FROM threads t
    LEFT JOIN session_environments env ON env.thread_id = t.id WHERE t.parent_thread_id IS NULL
    UNION ALL SELECT child.id, tree.machine_id FROM threads child JOIN tree ON child.parent_thread_id = tree.thread_id
) INSERT OR IGNORE INTO session_environments(thread_id, machine_id) SELECT thread_id, machine_id FROM tree";

/// canonical 表清单（父表在前，与 [`CREATE_TABLES_SQL`] 的顺序一致）。
pub(super) const CANONICAL_TABLES: &[&str] = &[
    "session_work_commands",
    "session_work_head",
    "session_inputs",
    "session_deliveries",
    "session_processing",
    "session_effects",
    "session_payloads",
    "session_work_events",
    "session_work_receipts",
    "session_control_state",
    "session_control_receipts",
    THREADS_TABLE,
    MESSAGES_TABLE,
    PROJECTS_TABLE,
    WORKSPACES_TABLE,
    SESSION_BINDINGS_TABLE,
    SESSION_ENVIRONMENTS_TABLE,
    OAUTH_CREDENTIALS_TABLE,
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
pub(super) const CREATE_ENVIRONMENTS_TABLE_SQL: &str =
    "CREATE TABLE IF NOT EXISTS session_environments (
    thread_id TEXT PRIMARY KEY REFERENCES threads(id) ON DELETE CASCADE,
    machine_id TEXT NOT NULL
)";

pub(super) const CREATE_TABLES: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS threads (
    id TEXT PRIMARY KEY, title TEXT, cwd TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL, updated_at TEXT NOT NULL, message_count INTEGER NOT NULL DEFAULT 0,
    parent_thread_id TEXT, snapshot_at_message_id TEXT, hidden BOOLEAN NOT NULL DEFAULT 0,
    cancel_policy TEXT NOT NULL DEFAULT 'cascade', config TEXT,
    frozen_context TEXT, inherited_context TEXT, agent_status TEXT NOT NULL DEFAULT 'active'
)",
    "CREATE TABLE IF NOT EXISTS messages (
    message_id TEXT PRIMARY KEY, thread_id TEXT NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
    role TEXT NOT NULL, content_ref TEXT NOT NULL,
    transcript_seq INTEGER NOT NULL CHECK(transcript_seq>=0),
    truncated BOOLEAN NOT NULL DEFAULT 0, excluded BOOLEAN NOT NULL DEFAULT 0, projection TEXT,
    UNIQUE(thread_id,transcript_seq)
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
    CREATE_ENVIRONMENTS_TABLE_SQL,
    CREATE_OAUTH_CREDENTIALS_TABLE_SQL,
];

/// 索引语句：必须在建表**与旧库补列之后**执行（`idx_threads_updated` 引用后补的列）。
pub(super) const CREATE_INDEXES: &[&str] = &[
    "CREATE INDEX IF NOT EXISTS idx_messages_thread_id ON messages(thread_id)",
    "CREATE INDEX IF NOT EXISTS idx_bindings_project ON session_bindings(project_id, thread_id)",
    "CREATE INDEX IF NOT EXISTS idx_bindings_workspace ON session_bindings(workspace_id, relative_cwd, thread_id)",
    "CREATE INDEX IF NOT EXISTS idx_threads_updated ON threads(updated_at DESC, id DESC) WHERE hidden = 0 AND message_count > 0",
    "CREATE INDEX IF NOT EXISTS idx_session_environments_machine ON session_environments(machine_id, thread_id)",
];

/// 存储 v2 的目标表定义。11→12 搬运在连接旧读写路径切换前由独立迁移夹具验证；
/// 新库与远端初始化接线后也使用这些常量，避免两份目标 DDL 漂移。
pub(super) const CREATE_V2_MACHINES_TABLE_SQL: &str = "CREATE TABLE IF NOT EXISTS machines (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL CHECK(length(trim(name)) > 0),
    identity_kind TEXT NOT NULL CHECK(identity_kind IN ('known', 'legacy_unknown'))
)";
pub(super) const CREATE_V2_WORKSPACES_TABLE_SQL: &str = "CREATE TABLE IF NOT EXISTS workspaces (
    id TEXT PRIMARY KEY,
    machine_id TEXT NOT NULL REFERENCES machines(id),
    path TEXT NOT NULL,
    path_source TEXT NOT NULL CHECK(path_source IN ('discovered', 'derived_legacy', 'unverified')),
    UNIQUE(machine_id, path)
)";
macro_rules! v2_threads_table_sql {
    ($name:literal) => { concat!("CREATE TABLE IF NOT EXISTS ", $name, " (
    id TEXT PRIMARY KEY, title TEXT, cwd TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL, updated_at TEXT NOT NULL, message_count INTEGER NOT NULL DEFAULT 0,
    parent_thread_id TEXT, snapshot_at_message_id TEXT, hidden BOOLEAN NOT NULL DEFAULT 0,
    cancel_policy TEXT NOT NULL DEFAULT 'cascade', config TEXT,
    frozen_context TEXT, inherited_context TEXT, agent_status TEXT NOT NULL DEFAULT 'active',
    workspace_id TEXT NOT NULL REFERENCES workspaces(id),
    archived BOOLEAN NOT NULL DEFAULT 0 CHECK(archived IN (0, 1))
)") };
}
pub(super) const CREATE_V2_THREADS_TABLE_SQL: &str = v2_threads_table_sql!("threads");
pub(super) const CREATE_V2_TEMP_THREADS_TABLE_SQL: &str = v2_threads_table_sql!("threads_v12");
pub(super) const CREATE_V2_OAUTH_CREDENTIALS_TABLE_SQL: &str =
    "CREATE TABLE IF NOT EXISTS mcp_oauth_credentials (
    principal_id TEXT NOT NULL,
    workspace_id TEXT NOT NULL REFERENCES workspaces(id),
    server_key TEXT NOT NULL,
    credentials_blob TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY(principal_id, workspace_id, server_key)
)";
pub(super) const CREATE_V2_LEGACY_REGISTRATIONS_TABLE_SQL: &str =
    "CREATE TABLE IF NOT EXISTS legacy_execution_registrations (
    id TEXT PRIMARY KEY, project_id TEXT NOT NULL REFERENCES projects(id),
    root TEXT NOT NULL, root_identity TEXT NOT NULL, discovery TEXT NOT NULL,
    UNIQUE(root, root_identity), UNIQUE(id, project_id)
)";
pub(super) const CREATE_V2_BINDINGS_TABLE_SQL: &str =
    "CREATE TABLE IF NOT EXISTS session_bindings (
    thread_id TEXT PRIMARY KEY REFERENCES threads(id) ON DELETE CASCADE,
    schema_version INTEGER NOT NULL,
    project_id TEXT NOT NULL, workspace_id TEXT NOT NULL, relative_cwd TEXT NOT NULL,
    discovery_snapshot TEXT,
    evidence_origin TEXT NOT NULL,
    FOREIGN KEY(workspace_id, project_id) REFERENCES legacy_execution_registrations(id, project_id)
)";
pub(super) const CREATE_V2_TABLES: &[&str] = &[
    CREATE_V2_MACHINES_TABLE_SQL,
    CREATE_V2_WORKSPACES_TABLE_SQL,
    CREATE_V2_THREADS_TABLE_SQL,
    CREATE_TABLES[1],
    CREATE_TABLES[2],
    CREATE_V2_LEGACY_REGISTRATIONS_TABLE_SQL,
    CREATE_V2_BINDINGS_TABLE_SQL,
    CREATE_V2_OAUTH_CREDENTIALS_TABLE_SQL,
    CREATE_SESSION_CLOSE_INTENTS_TABLE_SQL,
];
/// 显式关闭已接纳的持久事实；不保存异步任务目录。
pub(super) const CREATE_SESSION_CLOSE_INTENTS_TABLE_SQL: &str =
    "CREATE TABLE IF NOT EXISTS session_close_intents (
    thread_id TEXT PRIMARY KEY REFERENCES threads(id) ON DELETE CASCADE,
    requested_at TEXT NOT NULL
)";
pub(super) const CREATE_V2_INDEXES: &[&str] = &[
    CREATE_INDEXES[0],
    CREATE_INDEXES[1],
    CREATE_INDEXES[2],
    CREATE_INDEXES[3],
    "CREATE INDEX IF NOT EXISTS idx_threads_workspace_archived ON threads(workspace_id, archived, updated_at DESC, id DESC) WHERE parent_thread_id IS NULL AND message_count > 0",
];
pub(super) const SELECT_V2_OAUTH_CREDENTIAL_SQL: &str = "SELECT credentials_blob FROM mcp_oauth_credentials WHERE principal_id = ?1 AND workspace_id = ?2 AND server_key = ?3";
pub(super) const UPSERT_V2_OAUTH_CREDENTIAL_SQL: &str = "INSERT INTO mcp_oauth_credentials(principal_id, workspace_id, server_key, credentials_blob, updated_at) VALUES (?1, ?2, ?3, ?4, ?5) ON CONFLICT(principal_id, workspace_id, server_key) DO UPDATE SET credentials_blob = excluded.credentials_blob, updated_at = excluded.updated_at";
pub(super) const DELETE_V2_OAUTH_CREDENTIAL_SQL: &str = "DELETE FROM mcp_oauth_credentials WHERE principal_id = ?1 AND workspace_id = ?2 AND server_key = ?3";
pub(super) const DELETE_ALL_V2_OAUTH_CREDENTIALS_SQL: &str =
    "DELETE FROM mcp_oauth_credentials WHERE principal_id = ?1 AND workspace_id = ?2";
pub(super) const LIST_V2_OAUTH_CREDENTIALS_SQL: &str = "SELECT server_key FROM mcp_oauth_credentials WHERE principal_id = ?1 AND workspace_id = ?2 ORDER BY server_key";

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

pub(crate) fn extract_title(msgs: &[BaseMessage]) -> Option<String> {
    use peri_acp_types::messages::{ContentBlock, MessageContent};
    for msg in msgs {
        if let BaseMessage::Human { content, .. } = msg {
            let text = match content {
                MessageContent::Text(t) => t.clone(),
                MessageContent::Blocks(blocks) => blocks
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
