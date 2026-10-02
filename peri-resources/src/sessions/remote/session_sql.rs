//! 远程会话语句：静态 SQL 文本 + 全绑定参数，**表名与列名就是本机形状**。
//!
//! 统一之后远端不再有自己的会话表：语句里的 `threads` / `messages` / `session_bindings`
//! 与 `sqlite_store` 说的是同一份 canonical schema（`sessions::canonical` 是那份 DDL 的
//! 唯一来源）。列语义、`messages.rowid` 承载的 canonical 历史顺序、绑定四列所在的表都逐项
//! 对应；远端独有的只剩执行器机制（托管批与幂等账本），不体现在会话形状上。
//!
//! 规则与 [`super::sql`] 一致，只是范围更大：SQL 文本在编译期定型（`concat!` + `const`），
//! 会话 id、绑定、时间戳、分页游标、scope 过滤值全部走绑定参数。列投影只有一处来源
//! （宏 `meta_columns!`），事实投影与前缀投影由它拼出，避免「同一列清单手抄三遍」漂移。
//!
//! 解码用的列下标与投影同处一个模块，紧邻各自常量：投影改了，下标跟着改，测试拿真实
//! 列文本核对前缀关系。

use peri_acp_types::session_resources::{
    NewSession, NewSessionDraft, SessionMetaPatch, SessionResourceError, SessionResourceErrorKind,
    SessionResourceResult,
};
use peri_acp_types::store::{MessageFlags, PersistedPayload};
use peri_acp_types::thread::AgentStatus;
use peri_acp_types::workspace::{ScopedThreadQuery, SessionBinding, ThreadScope, WorkspaceId};
use turso_serverless::Value;

use super::session_codec::{int_value, optional_text, payload_params};
use super::sql::StatementSpec;
use crate::sessions::canonical;

/// 会话行投影的公共前缀：一列一行，顺序即解码下标。
macro_rules! meta_columns {
    () => {
        "s.id, s.title, s.cwd, s.created_at, s.updated_at, s.message_count,
    (SELECT COALESCE(SUM(LENGTH(m.content)), 0) FROM messages m WHERE m.thread_id = s.id),
    s.parent_thread_id, s.snapshot_at_message_id, s.hidden, s.cancel_policy, s.config, s.agent_status"
    };
}

/// 绑定四列：统一之后它们住在 `session_bindings` 表里（与本地同表同列），需要它们的投影
/// 通过 `LEFT JOIN session_bindings b ON b.thread_id = s.id` 取，不再是事实行上的扁平列。
macro_rules! binding_columns {
    () => {
        "b.schema_version, b.project_id, b.workspace_id, b.relative_cwd"
    };
}

/// 事实行投影 = meta 投影 + frozen/继承区 + 绑定四列（只在单会话读取时用）。
macro_rules! fact_columns {
    () => {
        concat!(
            meta_columns!(),
            ", s.frozen_context, s.inherited_context, ",
            binding_columns!()
        )
    };
}

/// 列表投影 = meta 投影 + 绑定四列（分页列举需要绑定分类，但不要 frozen/继承区正文）。
macro_rules! page_columns {
    () => {
        concat!(meta_columns!(), ", ", binding_columns!())
    };
}

// ─── 列下标（由投影顺序决定，测试核对） ────────────────────────────────────────

pub(super) const META_COLUMN_COUNT: usize = 13;
pub(super) const META_ID: usize = 0;
pub(super) const META_TITLE: usize = 1;
pub(super) const META_CWD: usize = 2;
pub(super) const META_CREATED_AT: usize = 3;
pub(super) const META_UPDATED_AT: usize = 4;
pub(super) const META_MESSAGE_COUNT: usize = 5;
pub(super) const META_CONTENT_SIZE: usize = 6;
pub(super) const META_PARENT: usize = 7;
pub(super) const META_SNAPSHOT_AT: usize = 8;
pub(super) const META_HIDDEN: usize = 9;
pub(super) const META_CANCEL_POLICY: usize = 10;
pub(super) const META_CONFIG: usize = 11;
pub(super) const META_AGENT_STATUS: usize = 12;

pub(super) const FACT_FROZEN: usize = 13;
pub(super) const FACT_INHERITED: usize = 14;
pub(super) const FACT_BINDING_VERSION: usize = 15;
pub(super) const FACT_BINDING_PROJECT: usize = 16;
pub(super) const FACT_BINDING_WORKSPACE: usize = 17;
pub(super) const FACT_BINDING_RELATIVE_CWD: usize = 18;
pub(super) const FACT_COLUMN_TOTAL: usize = 19;

pub(super) const PAGE_BINDING_VERSION: usize = 13;
pub(super) const PAGE_BINDING_PROJECT: usize = 14;
pub(super) const PAGE_BINDING_WORKSPACE: usize = 15;
pub(super) const PAGE_BINDING_RELATIVE_CWD: usize = 16;
pub(super) const PAGE_COLUMN_TOTAL: usize = 17;

/// 会话事实行（单会话读取）的投影。
pub(super) const META_PROJECTION: &str = meta_columns!();
pub(super) const FACT_PROJECTION: &str = fact_columns!();
pub(super) const PAGE_PROJECTION: &str = page_columns!();

// ─── 读取语句 ─────────────────────────────────────────────────────────────────

const SELECT_SESSION_SQL: &str = concat!(
    "SELECT ",
    fact_columns!(),
    " FROM threads s LEFT JOIN session_bindings b ON b.thread_id = s.id WHERE s.id = ?1"
);

const SELECT_META_SQL: &str = concat!(
    "SELECT ",
    meta_columns!(),
    " FROM threads s WHERE s.id = ?1"
);

const SELECT_CHILDREN_SQL: &str = concat!(
    "SELECT ",
    meta_columns!(),
    " FROM threads s WHERE s.parent_thread_id = ?1 ORDER BY s.created_at ASC, s.id ASC"
);

/// 以 `?1` 为根的整棵树（含自身）。CTE 名与投影别名同为 `s`，投影常量因此可复用
/// （内层 `SELECT *` 的形状与表一致，列名即投影里的 `s.<column>`）。
const SELECT_TREE_SQL: &str = concat!(
    "WITH RECURSIVE s AS (
        SELECT * FROM threads WHERE id = ?1
        UNION ALL
        SELECT p.* FROM threads p INNER JOIN s ON p.parent_thread_id = s.id
    )
    SELECT ",
    meta_columns!(),
    " FROM s ORDER BY s.created_at ASC, s.id ASC"
);

const SELECT_MESSAGES_SQL: &str =
    "SELECT m.message_id, m.content, m.truncated, m.excluded, m.projection
    FROM messages m WHERE m.thread_id = ?1 ORDER BY m.rowid ASC";

/// 沿父链上溯到的根（含自身即根的情形）；链上没有根时不返回行，由调用方判为事实不完整。
const SELECT_ROOT_SQL: &str = "WITH RECURSIVE a AS (
    SELECT s.id, s.parent_thread_id FROM threads s WHERE s.id = ?1
    UNION ALL
    SELECT p.id, p.parent_thread_id FROM threads p INNER JOIN a ON p.id = a.parent_thread_id
) SELECT a.id FROM a WHERE a.parent_thread_id IS NULL";

/// 父链根。
pub(super) fn select_root_statement(id: &str) -> StatementSpec {
    StatementSpec::new(SELECT_ROOT_SQL, vec![Value::Text(id.to_owned())])
}

/// 分页列举：scope 过滤、游标与上限全部是绑定参数，SQL 文本静态。
///
/// scope 用整数判别位 + 绑定值表达（`?1`）：0 = 全部、1 = project、2 = workspace、
/// 3 = 精确目录。没有绑定行的会话（远端不应存在，本机 legacy 才有的形状）不匹配任何
/// 带 scope 的查询——远端不借用本机 workspace 登记，也就不假装能做 legacy 路径归属。
const SELECT_PAGE_SQL: &str = concat!(
    "SELECT ",
    page_columns!(),
    " FROM threads s LEFT JOIN session_bindings b ON b.thread_id = s.id
    WHERE s.parent_thread_id IS NULL AND s.hidden = 0 AND s.archived = ?8 AND s.message_count > 0
      AND (?1 = 0
        OR (?1 = 1 AND b.project_id = ?2)
        OR (?1 = 2 AND s.workspace_id = ?2)
        OR (?1 = 3 AND s.workspace_id = ?2 AND b.relative_cwd = ?3))
      AND (?4 = 0 OR (s.updated_at, s.id) < (?5, ?6))
    ORDER BY s.updated_at DESC, s.id DESC LIMIT ?7"
);

const SELECT_ENV_PAGE_SQL: &str = concat!(
    "SELECT ",
    page_columns!(),
    " FROM threads s LEFT JOIN session_bindings b ON b.thread_id = s.id
    WHERE s.parent_thread_id IS NULL AND s.hidden = 0 AND s.archived = ?8 AND s.message_count > 0
      AND EXISTS (SELECT 1 FROM workspaces w WHERE w.id = s.workspace_id AND w.machine_id = ?2)
      AND (?4 = 0 OR (s.updated_at, s.id) < (?5, ?6))
    ORDER BY s.updated_at DESC, s.id DESC LIMIT ?7"
);

const SELECT_PAGE_V11_SQL: &str = concat!(
    "SELECT ",
    page_columns!(),
    " FROM threads s LEFT JOIN session_bindings b ON b.thread_id = s.id
    WHERE s.hidden = 0 AND s.message_count > 0
      AND (?1 = 0 OR (?1 = 1 AND b.project_id = ?2)
        OR (?1 = 2 AND b.workspace_id = ?2)
        OR (?1 = 3 AND b.workspace_id = ?2 AND b.relative_cwd = ?3))
      AND (?4 = 0 OR (s.updated_at, s.id) < (?5, ?6))
    ORDER BY s.updated_at DESC, s.id DESC LIMIT ?7"
);
const SELECT_ENV_PAGE_V11_SQL: &str = concat!(
    "SELECT ", page_columns!(),
    " FROM threads s LEFT JOIN session_bindings b ON b.thread_id = s.id
    WHERE s.hidden = 0 AND s.message_count > 0
      AND EXISTS (SELECT 1 FROM session_environments env WHERE env.thread_id = s.id AND env.machine_id = ?2)
      AND (?4 = 0 OR (s.updated_at, s.id) < (?5, ?6))
    ORDER BY s.updated_at DESC, s.id DESC LIMIT ?7"
);

/// 会话事实行（含绑定、frozen、继承区）。
pub(super) fn select_session_statement(id: &str) -> StatementSpec {
    StatementSpec::new(SELECT_SESSION_SQL, vec![Value::Text(id.to_owned())])
}

/// 会话 metadata 行（不含大字段）。
pub(super) fn select_meta_statement(id: &str) -> StatementSpec {
    StatementSpec::new(SELECT_META_SQL, vec![Value::Text(id.to_owned())])
}

/// 直接子会话 metadata。
pub(super) fn select_children_statement(parent: &str) -> StatementSpec {
    StatementSpec::new(SELECT_CHILDREN_SQL, vec![Value::Text(parent.to_owned())])
}

/// 整棵树 metadata（含根自身）。
pub(super) fn select_tree_statement(root: &str) -> StatementSpec {
    StatementSpec::new(SELECT_TREE_SQL, vec![Value::Text(root.to_owned())])
}

/// 自有 payload 行（按 canonical 插入序，即 `rowid`）。
pub(super) fn select_messages_statement(id: &str) -> StatementSpec {
    StatementSpec::new(SELECT_MESSAGES_SQL, vec![Value::Text(id.to_owned())])
}

/// 分页列举语句；scope 里的相对路径必须能作为文本绑定，否则拒绝（不猜路径相等，
/// 也不把「无法比较」静默当成「匹配为空」）。
pub(super) fn page_statement(query: &ScopedThreadQuery) -> SessionResourceResult<StatementSpec> {
    page_statement_for_archive(query, false)
}

pub(super) fn page_statement_for_archive(
    query: &ScopedThreadQuery,
    archived: bool,
) -> SessionResourceResult<StatementSpec> {
    let (kind, scope_id, relative) = match &query.scope {
        ThreadScope::Environment(machine_id) => (4, Value::Text(machine_id.clone()), Value::Null),
        ThreadScope::All => (0_i64, Value::Null, Value::Null),
        ThreadScope::Project(project) => (1, Value::Text(project.to_string()), Value::Null),
        ThreadScope::Workspace(workspace) => (2, Value::Text(workspace.to_string()), Value::Null),
        ThreadScope::ExactDirectory {
            workspace_id,
            relative_cwd,
        } => {
            let relative = relative_cwd.to_str().ok_or_else(|| {
                SessionResourceError::new(SessionResourceErrorKind::InvalidInput {
                    detail: "scope cwd is not valid UTF-8".to_owned(),
                })
            })?;
            (
                3,
                Value::Text(workspace_id.to_string()),
                Value::Text(relative.to_owned()),
            )
        }
    };
    let (cursor_flag, cursor_at, cursor_id) = match &query.cursor {
        Some(cursor) => (
            1_i64,
            Value::Text(cursor.updated_at.to_rfc3339()),
            Value::Text(cursor.thread_id.clone()),
        ),
        None => (0, Value::Null, Value::Null),
    };
    let limit = i64::from(query.limit.clamp(1, 200)) + 1;
    Ok(StatementSpec::new(
        if matches!(query.scope, ThreadScope::Environment(_)) {
            SELECT_ENV_PAGE_SQL
        } else {
            SELECT_PAGE_SQL
        },
        vec![
            int_value(kind),
            scope_id,
            relative,
            int_value(cursor_flag),
            cursor_at,
            cursor_id,
            int_value(limit),
            int_value(i64::from(archived)),
        ],
    ))
}

pub(super) fn page_statement_v11(
    query: &ScopedThreadQuery,
) -> SessionResourceResult<StatementSpec> {
    let mut spec = page_statement_for_archive(query, false)?;
    spec.sql = if matches!(query.scope, ThreadScope::Environment(_)) {
        SELECT_ENV_PAGE_V11_SQL
    } else {
        SELECT_PAGE_V11_SQL
    };
    spec.params.pop();
    Ok(spec)
}

// ─── 写入语句 ─────────────────────────────────────────────────────────────────

const INSERT_THREAD_SQL: &str =
    "INSERT INTO threads (id, title, cwd, created_at, updated_at, message_count,
        parent_thread_id, snapshot_at_message_id, hidden, cancel_policy, config,
        frozen_context, agent_status, workspace_id)
     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13,
        COALESCE(?14, (SELECT workspace_id FROM threads WHERE id = ?7)))";

/// 未发布创建（J2 第一阶段）：与整份创建同一列形状，只有 `frozen_context` 写 NULL。
///
/// 远端写的是**草稿**，内容准入在取得本机执行所有权之后由
/// [`super::session_write::RemoteSessionData::write_commit_frozen`] 一次性补上。
const INSERT_THREAD_DRAFT_SQL: &str =
    "INSERT INTO threads (id, title, cwd, created_at, updated_at, message_count,
        parent_thread_id, snapshot_at_message_id, hidden, cancel_policy, config,
        frozen_context, agent_status, workspace_id)
     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, NULL, ?12,
        COALESCE(?13, (SELECT workspace_id FROM threads WHERE id = ?7)))";

/// 一次性提交 frozen（write-once CAS）：只对尚未提交的草稿生效。
const COMMIT_FROZEN_SQL: &str =
    "UPDATE threads SET frozen_context = ?1 WHERE id = ?2 AND frozen_context IS NULL";

/// 删除整段会话历史的全部条目（与 `canonical::THREAD_CHILD_DELETES` 同一份语句）。
pub(super) const DELETE_SESSION_MESSAGES_SQL: &str = canonical::DELETE_MESSAGES_BY_THREAD_SQL;

/// 删除会话的不可变绑定行（统一后绑定住在 `session_bindings`，不再是会话行上的扁平列）。
pub(super) const DELETE_SESSION_BINDINGS_SQL: &str = canonical::DELETE_BINDINGS_BY_THREAD_SQL;

/// 删除会话行（与 `canonical::DELETE_THREAD_ROW_SQL` 同一份语句）。
pub(super) const DELETE_SESSION_SQL: &str = canonical::DELETE_THREAD_ROW_SQL;

/// write-once 未发布创建的撤销语句集（fork 等失败补偿）：四条删除**不带** frozen 判据。
///
/// 与 [`revoke_draft_statements`] 的唯一差别就是判据：那一条按调用方语义分档
/// （两阶段草稿要求 `frozen IS NULL`），不由本函数推断。
pub(super) fn revoke_session_statements(id: &str) -> Vec<StatementSpec> {
    [
        DELETE_SESSION_MESSAGES_SQL,
        DELETE_SESSION_BINDINGS_SQL,
        "DELETE FROM session_environments WHERE thread_id = ?1",
        DELETE_SESSION_SQL,
    ]
    .into_iter()
    .map(|sql| StatementSpec::new(sql, vec![Value::Text(id.to_owned())]))
    .collect()
}

/// 未发布创建的撤销：各条删除都带同一判据（`frozen_context IS NULL`），因此同一批里
/// 「会话行还在不在」与「历史/绑定删没删」不会分叉——已提交 frozen 的会话一条都不删。
const DELETE_DRAFT_MESSAGES_SQL: &str = concat!(
    "DELETE FROM messages WHERE thread_id = ?1",
    " AND EXISTS (SELECT 1 FROM threads WHERE id = ?1 AND frozen_context IS NULL)"
);
const DELETE_DRAFT_BINDINGS_SQL: &str = concat!(
    "DELETE FROM session_bindings WHERE thread_id = ?1",
    " AND EXISTS (SELECT 1 FROM threads WHERE id = ?1 AND frozen_context IS NULL)"
);
const DELETE_DRAFT_SESSION_SQL: &str =
    "DELETE FROM threads WHERE id = ?1 AND frozen_context IS NULL";

/// 不可变绑定行插入：与本机 `session_rows.rs::insert_binding_row` 同一份语句，绑定住在
/// `session_bindings` 表里（不再是会话行上的四个扁平列）。
const INSERT_BINDING_SQL: &str =
    "INSERT INTO session_bindings (thread_id, schema_version, project_id, workspace_id, relative_cwd,
        discovery_snapshot, evidence_origin)
     VALUES (?1, ?2, ?3, ?4, ?5, COALESCE(?6,
        (SELECT discovery_snapshot FROM session_bindings WHERE thread_id = ?7)), 'creation_snapshot')";

/// 继承区写入：与本机 child 路径同一份语句（创建行之后单独写一次，INSERT 不多带一列）。
const UPDATE_INHERITED_SQL: &str = "UPDATE threads SET inherited_context = ?1 WHERE id = ?2";

/// 历史行插入：列清单与本机 `messages` 一致（含 `role`），顺序由 `rowid` 承载。
const INSERT_MESSAGE_SQL: &str = "INSERT INTO messages (
    message_id, thread_id, role, content, truncated, excluded, projection)
    VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)";

/// 定向更新：`Some(None)` 清除、`None` 保持不变、`None` 之外的值照写。
///
/// 用「标志位 + 值」的 CASE 表达，SQL 文本保持静态；没有字段要改时调用方不该调用它
/// （门面与本地 adapter 同一约定：不写、也不假装更新了时间戳）。
const UPDATE_META_SQL: &str = "UPDATE threads SET
    updated_at = ?1,
    title = CASE WHEN ?2 = 1 THEN ?3 ELSE title END,
    agent_status = CASE WHEN ?4 = 1 THEN ?5 ELSE agent_status END,
    cancel_policy = CASE WHEN ?6 = 1 THEN ?7 ELSE cancel_policy END,
    config = CASE WHEN ?8 = 1 THEN ?9 ELSE config END
    WHERE id = ?10";

/// 一行新会话的写入参数；派生计数与时间戳由调用方给定，本层不读时钟。
pub(super) struct SessionInsert<'a> {
    pub(super) thread_id: &'a str,
    pub(super) title: Option<&'a str>,
    pub(super) cwd: &'a str,
    pub(super) created_at: &'a str,
    /// 创建路径为 0；fork 目标为映射后的 payload 数。
    pub(super) message_count: i64,
    pub(super) parent_thread_id: Option<&'a str>,
    /// 快照截止消息 id（文本形式；由调用方从领域 id 定型）。
    pub(super) snapshot_at_message_id: Option<String>,
    pub(super) hidden: bool,
    pub(super) cancel_policy: &'a str,
    /// 会话创建时冻结的上下文快照字节（不解释内容）。
    pub(super) frozen: &'a str,
    /// child 的继承区 JSON；创建与 fork 目标为 `None`。
    pub(super) inherited: Option<&'a str>,
    pub(super) binding: &'a SessionBinding,
    /// 已生效会话的 agent 状态；新行一律 `active`。
    pub(super) agent_status: &'a str,
    pub(super) owner_workspace_id: Option<WorkspaceId>,
    pub(super) discovery_snapshot: Option<&'a str>,
}

/// 由 [`NewSession`] 组装一行插入参数（创建/fork/child 三条路径共用同一列形状）。
///
/// `snapshot_at_message_id` 在这里定型为文本：它是记录事实，不是可再推导的引用。
pub(super) fn session_insert<'a>(
    input: &'a NewSession,
    message_count: i64,
    inherited: Option<&'a str>,
) -> SessionInsert<'a> {
    SessionInsert {
        thread_id: &input.thread_id,
        title: input.meta.title.as_deref(),
        cwd: &input.meta.cwd,
        created_at: &input.created_at,
        message_count,
        parent_thread_id: input.meta.parent_thread_id.as_deref(),
        snapshot_at_message_id: input
            .meta
            .snapshot_at_message_id
            .map(|id| id.as_uuid().to_string()),
        hidden: input.meta.hidden,
        cancel_policy: input.meta.cancel_policy.as_str(),
        frozen: input.frozen.as_str(),
        inherited,
        binding: &input.binding,
        agent_status: AgentStatus::Active.as_str(),
        owner_workspace_id: None,
        discovery_snapshot: None,
    }
}

/// 一条新会话的落库语句集：`threads` 行 + `session_bindings` 行（child 另加继承区写入）。
///
/// `updated_at` 从 `created_at` 起步（创建时刻即最后更新时刻）；顺序即批内执行顺序，
/// 父行先于引用它的绑定行。继承区单独写一次，与本机 child 路径同一节奏。
pub(super) fn insert_session_statements(
    row: &SessionInsert<'_>,
) -> SessionResourceResult<Vec<StatementSpec>> {
    let relative = binding_relative_text(row.binding)?;
    let mut statements = vec![
        StatementSpec::new(
            INSERT_THREAD_SQL,
            vec![
                Value::Text(row.thread_id.to_owned()),
                optional_text(row.title),
                Value::Text(row.cwd.to_owned()),
                Value::Text(row.created_at.to_owned()),
                Value::Text(row.created_at.to_owned()),
                int_value(row.message_count),
                optional_text(row.parent_thread_id),
                optional_text(row.snapshot_at_message_id.as_deref()),
                int_value(i64::from(row.hidden)),
                Value::Text(row.cancel_policy.to_owned()),
                Value::Null,
                Value::Text(row.frozen.to_owned()),
                Value::Text(row.agent_status.to_owned()),
                row.owner_workspace_id
                    .map(|id| Value::Text(id.to_string()))
                    .unwrap_or(Value::Null),
            ],
        ),
        StatementSpec::new(
            INSERT_BINDING_SQL,
            vec![
                Value::Text(row.thread_id.to_owned()),
                int_value(i64::from(row.binding.schema_version)),
                Value::Text(row.binding.project_id.to_string()),
                Value::Text(row.binding.workspace_id.to_string()),
                Value::Text(relative),
                optional_text(row.discovery_snapshot),
                optional_text(row.parent_thread_id),
            ],
        ),
    ];
    statements.push(environment_statement(row.thread_id, row.parent_thread_id)?);
    if let Some(inherited) = row.inherited {
        statements.push(StatementSpec::new(
            UPDATE_INHERITED_SQL,
            vec![
                Value::Text(inherited.to_owned()),
                Value::Text(row.thread_id.to_owned()),
            ],
        ));
    }
    Ok(statements)
}

/// 未发布创建（草稿）的落库语句集：`threads` 行（frozen NULL）+ 绑定行。
///
/// 与 [`insert_session_statements`] 同一列形状与同一顺序，唯一差别是 frozen 写 NULL；
/// 内容准入由 [`commit_frozen_statement`] 一次性补上。
pub(super) fn insert_session_draft_statements(
    draft: &NewSessionDraft,
    owner_workspace_id: Option<WorkspaceId>,
    discovery_snapshot: Option<&str>,
) -> SessionResourceResult<Vec<StatementSpec>> {
    let relative = binding_relative_text(&draft.binding)?;
    let snapshot_at = draft
        .meta
        .snapshot_at_message_id
        .map(|id| id.as_uuid().to_string());
    Ok(vec![
        StatementSpec::new(
            INSERT_THREAD_DRAFT_SQL,
            vec![
                Value::Text(draft.thread_id.as_str().to_owned()),
                optional_text(draft.meta.title.as_deref()),
                Value::Text(draft.meta.cwd.clone()),
                Value::Text(draft.created_at.clone()),
                Value::Text(draft.created_at.clone()),
                int_value(0),
                optional_text(draft.meta.parent_thread_id.as_deref()),
                optional_text(snapshot_at.as_deref()),
                int_value(i64::from(draft.meta.hidden)),
                Value::Text(draft.meta.cancel_policy.as_str().to_owned()),
                Value::Null,
                Value::Text(AgentStatus::Active.as_str().to_owned()),
                owner_workspace_id
                    .map(|id| Value::Text(id.to_string()))
                    .unwrap_or(Value::Null),
            ],
        ),
        StatementSpec::new(
            INSERT_BINDING_SQL,
            vec![
                Value::Text(draft.thread_id.as_str().to_owned()),
                int_value(i64::from(draft.binding.schema_version)),
                Value::Text(draft.binding.project_id.to_string()),
                Value::Text(draft.binding.workspace_id.to_string()),
                Value::Text(relative),
                optional_text(discovery_snapshot),
                optional_text(draft.meta.parent_thread_id.as_deref()),
            ],
        ),
        environment_statement(
            draft.thread_id.as_str(),
            draft.meta.parent_thread_id.as_deref(),
        )?,
    ])
}

fn environment_statement(id: &str, parent: Option<&str>) -> SessionResourceResult<StatementSpec> {
    Ok(StatementSpec::new(
        canonical::INSERT_ENVIRONMENT_SQL,
        vec![
            Value::Text(id.to_owned()),
            optional_text(parent),
            Value::Text(
                crate::sessions::machine::current()
                    .map_err(|_| {
                        crate::sessions::sqlite_store::unavailable(
                            "machine identity is not initialized",
                        )
                    })?
                    .to_owned(),
            ),
        ],
    ))
}

/// 一次性提交 frozen 的语句（write-once CAS；受影响行数由调用方按 1 核对）。
pub(super) fn commit_frozen_statement(id: &str, frozen: &str) -> StatementSpec {
    StatementSpec::new(
        COMMIT_FROZEN_SQL,
        vec![Value::Text(frozen.to_owned()), Value::Text(id.to_owned())],
    )
}

/// 撤销未发布创建的语句集：三条删除共用 `frozen_context IS NULL` 判据（先子后父）。
pub(super) fn revoke_draft_statements(id: &str) -> Vec<StatementSpec> {
    [
        DELETE_DRAFT_MESSAGES_SQL,
        DELETE_DRAFT_BINDINGS_SQL,
        "DELETE FROM session_environments WHERE thread_id = ?1 AND EXISTS (SELECT 1 FROM threads WHERE id = ?1 AND frozen_context IS NULL)",
        DELETE_DRAFT_SESSION_SQL,
    ]
    .into_iter()
    .map(|sql| StatementSpec::new(sql, vec![Value::Text(id.to_owned())]))
    .collect()
}

/// 历史行的插入语句：flags 与内容一次写入（fork 目标由门面完成 ID 重映射，本层不重跑算法）。
pub(super) fn insert_message_statement(
    thread_id: &str,
    payload: &PersistedPayload,
    flags: Option<&MessageFlags>,
) -> SessionResourceResult<StatementSpec> {
    Ok(StatementSpec::new(
        INSERT_MESSAGE_SQL,
        payload_params(thread_id, payload, flags)?,
    ))
}

/// 定向 metadata 更新语句（`None` 不动、`Some(None)` 清除）。
pub(super) fn update_meta_statement(
    id: &str,
    patch: &SessionMetaPatch,
    now: &str,
) -> StatementSpec {
    let (title_flag, title) = match &patch.title {
        Some(value) => (1_i64, optional_text(value.as_deref())),
        None => (0, Value::Null),
    };
    let (status_flag, status) = match &patch.status {
        Some(status) => (1_i64, Value::Text(status.as_str().to_owned())),
        None => (0, Value::Null),
    };
    let (policy_flag, policy) = match &patch.cancel_policy {
        Some(policy) => (1_i64, Value::Text(policy.as_str().to_owned())),
        None => (0, Value::Null),
    };
    let (config_flag, config) = match &patch.config {
        Some(value) => (1_i64, optional_text(value.as_deref())),
        None => (0, Value::Null),
    };
    StatementSpec::new(
        UPDATE_META_SQL,
        vec![
            Value::Text(now.to_owned()),
            int_value(title_flag),
            title,
            int_value(status_flag),
            status,
            int_value(policy_flag),
            policy,
            int_value(config_flag),
            config,
            Value::Text(id.to_owned()),
        ],
    )
}

/// 绑定里的相对 cwd 必须能作为文本存储，且形状上确实是「相对 workspace 的路径」。
///
/// 这里只做形状校验：目录是否存在、是否与已登记 workspace 一致由本机 workspace/门面判定，
/// 云端没有目录可查，也不承担那个判断。
pub(super) fn binding_relative_text(binding: &SessionBinding) -> SessionResourceResult<String> {
    let path = &binding.cwd_relative_to_workspace;
    if path.is_absolute() {
        return Err(SessionResourceError::new(
            SessionResourceErrorKind::InvalidInput {
                detail: "session binding cwd must be relative to its workspace".to_owned(),
            },
        ));
    }
    path.to_str().map(str::to_owned).ok_or_else(|| {
        SessionResourceError::new(SessionResourceErrorKind::InvalidInput {
            detail: "session binding cwd is not valid UTF-8".to_owned(),
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 远端两条撤销入口的判据点：write-once（fork 补偿）不叠加 frozen 判据，
    /// 两阶段草稿的四条删除全部带 `frozen_context IS NULL`。
    #[test]
    fn revocation_statements_split_by_entry_point() {
        // 两阶段草稿：判据在每一条删除上（同一批内不会出现「历史删了、会话行还在」）。
        let draft = revoke_draft_statements("s");
        assert_eq!(draft.len(), 4);
        for statement in &draft {
            assert!(
                statement.sql.contains("frozen_context IS NULL"),
                "草稿撤销的每条删除都必须带未提交判据: {}",
                statement.sql
            );
            assert!(
                statement.sql.starts_with("DELETE"),
                "只允许删除语句: {}",
                statement.sql
            );
        }
        // write-once：撤销未发布创建（fork 等）不叠加 frozen 判据——目标创建即带 frozen，
        // 补偿就是把它整条删掉。
        let write_once = revoke_session_statements("s");
        assert_eq!(write_once.len(), 4);
        for statement in &write_once {
            assert!(
                !statement.sql.contains("frozen_context"),
                "write-once 撤销不得叠加 frozen 判据: {}",
                statement.sql
            );
        }
    }
}
