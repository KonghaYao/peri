//! Session projections, bounded history reads, and scoped listing statements.

use super::super::sql::StatementSpec;
use crate::sessions::remote::sql::Value;
use peri_acp_types::session_resources::SessionResourceError;
use peri_acp_types::session_resources::SessionResourceErrorKind;
use peri_acp_types::session_resources::SessionResourceResult;
use peri_acp_types::workspace::ScopedThreadQuery;
use peri_acp_types::workspace::ThreadScope;

/// 会话行投影的公共前缀：一列一行，顺序即解码下标。
///
/// `content_size` 没有落列，也没有生产消费者；此前的 `SUM(LENGTH(content))` 相关子查询
/// 每次读取都要聚合整份历史（恢复热路径与列表都受影响，随历史增长而变慢）。这里保留
/// 列位（别名 + 常量 0）以维持行形状与解码下标，不做聚合；需要真实字节数时应另设
/// 按需读取，不把全历史聚合塞回这条投影。
macro_rules! meta_columns {
    () => {
        "s.id, s.title, s.cwd, s.created_at, s.updated_at, s.message_count,
    0 as content_size,
    s.parent_thread_id, s.snapshot_at_message_id, s.hidden, s.cancel_policy, s.config, s.agent_status,
    s.context_usage_tokens, s.work_state"
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

pub(in crate::sessions::remote) const META_ID: usize = 0;
pub(in crate::sessions::remote) const META_TITLE: usize = 1;
pub(in crate::sessions::remote) const META_CWD: usize = 2;
pub(in crate::sessions::remote) const META_CREATED_AT: usize = 3;
pub(in crate::sessions::remote) const META_UPDATED_AT: usize = 4;
pub(in crate::sessions::remote) const META_MESSAGE_COUNT: usize = 5;
pub(in crate::sessions::remote) const META_CONTENT_SIZE: usize = 6;
pub(in crate::sessions::remote) const META_PARENT: usize = 7;
pub(in crate::sessions::remote) const META_SNAPSHOT_AT: usize = 8;
pub(in crate::sessions::remote) const META_HIDDEN: usize = 9;
pub(in crate::sessions::remote) const META_CANCEL_POLICY: usize = 10;
pub(in crate::sessions::remote) const META_CONFIG: usize = 11;
pub(in crate::sessions::remote) const META_AGENT_STATUS: usize = 12;
/// 两列工作快照。
pub(in crate::sessions::remote) const META_CONTEXT_USAGE_TOKENS: usize = META_AGENT_STATUS + 1;
pub(in crate::sessions::remote) const META_WORK_STATE: usize = META_CONTEXT_USAGE_TOKENS + 1;
/// meta 投影列数由最后一个下标推出，不另写一遍数字：投影追加列时只改下标。
pub(in crate::sessions::remote) const META_COLUMN_COUNT: usize = META_WORK_STATE + 1;

pub(in crate::sessions::remote) const FACT_FROZEN: usize = META_COLUMN_COUNT;
pub(in crate::sessions::remote) const FACT_INHERITED: usize = FACT_FROZEN + 1;
pub(in crate::sessions::remote) const FACT_BINDING_VERSION: usize = FACT_INHERITED + 1;
pub(in crate::sessions::remote) const FACT_BINDING_PROJECT: usize = FACT_BINDING_VERSION + 1;
pub(in crate::sessions::remote) const FACT_BINDING_WORKSPACE: usize = FACT_BINDING_PROJECT + 1;
pub(in crate::sessions::remote) const FACT_BINDING_RELATIVE_CWD: usize = FACT_BINDING_WORKSPACE + 1;
pub(in crate::sessions::remote) const FACT_COLUMN_TOTAL: usize = FACT_BINDING_RELATIVE_CWD + 1;

pub(in crate::sessions::remote) const PAGE_BINDING_VERSION: usize = META_COLUMN_COUNT;
pub(in crate::sessions::remote) const PAGE_BINDING_PROJECT: usize = PAGE_BINDING_VERSION + 1;
pub(in crate::sessions::remote) const PAGE_BINDING_WORKSPACE: usize = PAGE_BINDING_PROJECT + 1;
pub(in crate::sessions::remote) const PAGE_BINDING_RELATIVE_CWD: usize = PAGE_BINDING_WORKSPACE + 1;
pub(in crate::sessions::remote) const PAGE_COLUMN_TOTAL: usize = PAGE_BINDING_RELATIVE_CWD + 1;

/// 会话事实行（单会话读取）的投影。
pub(in crate::sessions::remote) const META_PROJECTION: &str = meta_columns!();
pub(in crate::sessions::remote) const FACT_PROJECTION: &str = fact_columns!();
pub(in crate::sessions::remote) const PAGE_PROJECTION: &str = page_columns!();

// ─── 读取语句 ─────────────────────────────────────────────────────────────────

const SELECT_SESSION_SQL: &str = concat!(
    "SELECT ",
    fact_columns!(),
    " FROM threads s LEFT JOIN session_bindings b ON b.thread_id = s.id WHERE s.id = $1"
);

const SELECT_META_SQL: &str = concat!(
    "SELECT ",
    meta_columns!(),
    " FROM threads s WHERE s.id = $1"
);

/// 恢复准入的轻量 frozen 读取：只取一列，不拉绑定、继承区或任何正文。
pub(super) const SELECT_FROZEN_SQL: &str = "SELECT s.frozen_context FROM threads s WHERE s.id = $1";

const SELECT_CHILDREN_SQL: &str = concat!(
    "SELECT ",
    meta_columns!(),
    " FROM threads s WHERE s.parent_thread_id = $1 ORDER BY s.created_at ASC, s.id ASC"
);

/// 以 `$1` 为根的整棵树（含自身）。CTE 名与投影别名同为 `s`，投影常量因此可复用
/// （内层 `SELECT *` 的形状与表一致，列名即投影里的 `s.<column>`）。
macro_rules! tree_select {
    ($columns:ident) => {
        concat!(
            "WITH RECURSIVE s AS (
        SELECT * FROM threads WHERE id = $1
        UNION ALL
        SELECT p.* FROM threads p INNER JOIN s ON p.parent_thread_id = s.id
    )
    SELECT ",
            $columns!(),
            " FROM s ORDER BY s.created_at ASC, s.id ASC"
        )
    };
}

pub(super) const SELECT_TREE_SQL: &str = tree_select!(meta_columns);

pub(super) const SELECT_MESSAGES_SQL: &str =
    "SELECT m.message_id, m.content, m.truncated, m.excluded, m.projection
    FROM messages m WHERE m.thread_id = $1 ORDER BY m.rowid ASC";

/// 有效范围的自有 payload 行：`excluded = 0` 是读取条件的一部分，已排除正文不出库、
/// 不进入内存；顺序仍是 canonical 插入序（`rowid`）。
pub(super) const SELECT_ACTIVE_MESSAGES_SQL: &str =
    "SELECT m.message_id, m.content, m.truncated, m.excluded, m.projection
    FROM messages m WHERE m.thread_id = $1 AND m.excluded = 0 ORDER BY m.rowid ASC";

/// 浏览范围的边界事实（分页读取与初始 load 共用的**唯一**一条边界语句）：会话存在吗、
/// 浏览范围起点是谁、锚点行在不在。
///
/// 会话不存在时**没有行**（`NotFound`）；起点缺失为 NULL，锚点列只是**存在性判据**
/// （`NULL` = 锚点已不在；边界读取固定传 NULL，只取起点一列）。刻意不写
/// `COALESCE(..., MAX(rowid)+1)` 这类兜底：锚点消失必须失败关闭，不能悄悄把它变成
/// 「从最新一页开始」。
pub(super) const SELECT_HISTORY_PAGE_BOUNDS_SQL: &str = "SELECT
        (SELECT m.message_id FROM messages m WHERE m.thread_id = t.id ORDER BY m.rowid ASC LIMIT 1),
        (SELECT m.rowid FROM messages m WHERE m.thread_id = t.id AND m.message_id = $2)
    FROM threads t WHERE t.id = $1";

/// 一页候选行的**阶段一**读取：（身份 + 存储序键 + 正文长度），keyset 遍历，最多
/// `limit + 1` 行，**不取正文**。
///
/// `$2` 为空 = 首屏，从最新一行开始——只有首屏允许这条路径：带游标时锚点行的存在性已由
/// [`SELECT_HISTORY_PAGE_BOUNDS_SQL`] 证明，`NULL` 在那里已经变成失败。锚点用**子查询按
/// `message_id` 定位**（同一条语句内完成，两次语句之间不引入新的时间点），排序才用
/// `rowid`。序列值用于排序，游标身份绑定消息 ID。
///
/// 长度是**字节数**（`octet_length(content)`；直接 `length(content)` 数的是字符），
/// 与页预算同一口径。
pub(super) const SELECT_HISTORY_PAGE_ROWS_SQL: &str = "SELECT s.message_id, s.rowid,
        octet_length(s.content)
    FROM messages s
    WHERE s.thread_id = $1 AND ($2::text IS NULL OR s.rowid < (
        SELECT m.rowid FROM messages m WHERE m.thread_id = $1 AND m.message_id = $2))
    ORDER BY s.rowid DESC LIMIT $3";

/// 页内一条条目的正文：**阶段二**只按本页那些行的 id 取，一条语句一个 id，同一批次读出。
///
/// 批内 `LIMIT 1` 与唯一约束共同保证「一个 id 至多一行」：多行即回复形状不符（调用方
/// 按损坏失败），绝不取第一行。
pub(super) const SELECT_HISTORY_PAGE_BODY_SQL: &str =
    "SELECT s.content FROM messages s WHERE s.thread_id = $1 AND s.message_id = $2 LIMIT 1";

/// 自有**有效**条目的存储序键（`message_id → rowid`）：不读正文、不做全历史扫描。
///
/// 读取条件与有效视图一致（`excluded = 0`）：归档正文连行都不必返回，客户端要的只是
/// 「这一条自有有效条目排在哪儿」（初始 load 回放的位置戳）。
pub(super) const SELECT_ACTIVE_ENTRY_POSITIONS_SQL: &str =
    "SELECT m.message_id, m.rowid FROM messages m WHERE m.thread_id = $1 AND m.excluded = 0
    ORDER BY m.rowid ASC";

// ─── 回退候选（有界读取） ──────────────────────────────────────────────────────

/// 阶段一：user 行身份 + 存储序键，keyset 向更早方向，最多 `limit` 行。
///
/// 不取正文；`role = 'user'` 是领域常量（`canonical::role_of` 的稳定取值），不是用户输入。
/// `excluded` 不是读取条件：归档 user 消息仍是合法回退目标。
pub(super) const SELECT_REWIND_CANDIDATE_ROWS_SQL: &str = "SELECT m.message_id, m.rowid
    FROM messages m
    WHERE m.thread_id = $1 AND m.role = 'user' AND ($2::bigint IS NULL OR m.rowid < $2)
    ORDER BY m.rowid DESC LIMIT $3";

/// 阶段二：按本批命中的 id 逐条取正文（主键单行）。
pub(super) const SELECT_REWIND_CANDIDATE_BODY_SQL: &str =
    "SELECT m.content FROM messages m WHERE m.thread_id = $1 AND m.message_id = $2 LIMIT 1";

// ─── 向前段读取（canonical 完整回放，REQ §5.3） ──────────────────────────────

/// 段读取的边界事实：会话存在吗 + 本次回放的固定上界（该线程自有行的最大 `rowid`，
/// 没有自有行时为 NULL）。
///
/// 与 [`SELECT_HISTORY_PAGE_BOUNDS_SQL`] 同形（`threads` 单行 + 子查询）：会话不存在时
/// **没有行**（`NotFound`）。上界在这里读出来交给游标，行则由
/// [`SELECT_HISTORY_SEGMENT_HEAD_SQL`] 在同一批次内再算一次 `MAX(rowid)` 作谓词——两条语句
/// 落在同一个只读批次里，区间与行同源。
pub(super) const SELECT_HISTORY_SEGMENT_TAIL_SQL: &str = "SELECT
        (SELECT MAX(m.rowid) FROM messages m WHERE m.thread_id = t.id)
    FROM threads t WHERE t.id = $1";

/// 首段候选行（**阶段一**：身份 + 存储序键 + 正文长度），最多 `limit + 1` 行，**升序**
/// （canonical 完整回放方向），**不取正文**。
///
/// 与非首段同形，只是去掉 `rowid > $2`，并把上界交给同一条语句内的 `MAX(rowid)` 子查询：
/// 首段的上界就是这一刻的最大 `rowid`，没有可以预先绑定的值（它由本次读取确定）。
pub(super) const SELECT_HISTORY_SEGMENT_HEAD_SQL: &str = "SELECT s.message_id, s.rowid,
        octet_length(s.content)
    FROM messages s
    WHERE s.thread_id = $1 AND s.rowid <= (
        SELECT MAX(m.rowid) FROM messages m WHERE m.thread_id = $1)
    ORDER BY s.rowid ASC LIMIT $2";

/// 锚点存在性 + 会话存在性（一次单行读取）：`rowid = $2` 的那一行就是游标的 `after`。
///
/// `NULL` = 锚点已不在（游标失效）；一条行都没有 = 会话不存在（`NotFound`）。刻意不写
/// `COALESCE(..., 0)` 这类兜底：锚点消失必须失败关闭，不能悄悄退化成一个从头开始的区间。
pub(super) const SELECT_HISTORY_SEGMENT_ANCHOR_SQL: &str = "SELECT
        (SELECT m.rowid FROM messages m WHERE m.thread_id = t.id AND m.rowid = $2)
    FROM threads t WHERE t.id = $1";

/// 非首段候选行：`rowid > after` 且 `rowid <= tail`，升序，最多 `limit + 1` 行。
///
/// `tail` 是**数值**上界，不是一条要查回来的行：回放期间追加的行 `rowid > tail`，因此不进入
/// 本次遍历；`tail` 那一行自己被删掉也不影响区间。
pub(super) const SELECT_HISTORY_SEGMENT_NEXT_SQL: &str = "SELECT s.message_id, s.rowid,
        octet_length(s.content)
    FROM messages s
    WHERE s.thread_id = $1 AND s.rowid > $2 AND s.rowid <= $3
    ORDER BY s.rowid ASC LIMIT $4";

/// 首段要放在最前面的继承区快照（`threads.inherited_context` 单行）：一列，与本地
/// `INHERITED_CONTEXT_SQL` 说同一件事。
///
/// 不借会话事实行（`SELECT_SESSION_SQL`）来读它：那条语句还会拉上绑定四列与 frozen 正文，
/// 而首段只要继承区 payload。
pub(super) const SELECT_INHERITED_CONTEXT_SQL: &str =
    "SELECT s.inherited_context FROM threads s WHERE s.id = $1";

/// 有界 flags 读取的单条语句：`$1` 是会话、`$2` 是条目。
///
/// 范围由调用方给出的 id 集合决定（一条语句一个 id，同一批次读出），不做全历史扫描；
/// 与全量 flags 视图同一规则：只返回非默认标记。
pub(super) const SELECT_MESSAGE_FLAGS_SQL: &str =
    "SELECT message_id, truncated, excluded, projection
    FROM messages WHERE thread_id = $1 AND message_id = $2
      AND (truncated = 1 OR excluded = 1 OR projection IS NOT NULL)";

/// 沿父链上溯到的根（含自身即根的情形）；链上没有根时不返回行，由调用方判为事实不完整。
pub(super) const SELECT_ROOT_SQL: &str = "WITH RECURSIVE a AS (
    SELECT s.id, s.parent_thread_id FROM threads s WHERE s.id = $1
    UNION ALL
    SELECT p.id, p.parent_thread_id FROM threads p INNER JOIN a ON p.id = a.parent_thread_id
) SELECT a.id FROM a WHERE a.parent_thread_id IS NULL";

/// 父链根。
pub(in crate::sessions::remote) fn select_root_statement(id: &str) -> StatementSpec {
    StatementSpec::new(SELECT_ROOT_SQL, vec![Value::Text(id.to_owned())])
}

/// 恢复范围候选：根子树中工作意图仍为 `working` 的 thread（与本地 adapter 同一 CTE）。
///
/// `UNION`（非 `ALL`）保证父链成环时终止；根已由调用方严格解析，语句本身不再兜底。
/// 远端投影与本地同代际形状，没有 legacy 分支。
pub(super) const SELECT_WORKING_TREE_SQL: &str = "WITH RECURSIVE thread_scope(id) AS (
    SELECT id FROM threads WHERE id = $1
    UNION
    SELECT child.id FROM threads AS child
    JOIN thread_scope AS parent ON child.parent_thread_id = parent.id
) SELECT t.id, t.parent_thread_id, t.context_usage_tokens, t.work_state
  FROM threads AS t JOIN thread_scope AS scope ON scope.id = t.id
  WHERE t.work_state = 'working'
  ORDER BY t.created_at ASC";

/// 恢复范围候选行（根子树 + `working`）。
pub(in crate::sessions::remote) fn select_working_tree_statement(root: &str) -> StatementSpec {
    StatementSpec::new(SELECT_WORKING_TREE_SQL, vec![Value::Text(root.to_owned())])
}

/// 进入根时待收敛的 child：根子树中非根、`active` 且 `idle`/`stopped` 的 thread。
pub(super) const SELECT_INTERRUPTED_TREE_SQL: &str = "WITH RECURSIVE thread_scope(id) AS (
    SELECT id FROM threads WHERE id = $1
    UNION
    SELECT child.id FROM threads AS child
    JOIN thread_scope AS parent ON child.parent_thread_id = parent.id
) SELECT t.id, t.work_state
  FROM threads AS t JOIN thread_scope AS scope ON scope.id = t.id
  WHERE t.id <> $1 AND t.agent_status = 'active' AND t.work_state IN ('idle', 'stopped')
  ORDER BY t.created_at ASC";

/// 遗留 Active 收敛候选行（根子树 + 非根 + `idle`/`stopped`）。
pub(in crate::sessions::remote) fn select_interrupted_tree_statement(root: &str) -> StatementSpec {
    StatementSpec::new(
        SELECT_INTERRUPTED_TREE_SQL,
        vec![Value::Text(root.to_owned())],
    )
}

// 分页列举：scope 过滤、游标与上限全部是绑定参数，SQL 文本静态。
//
// scope 用整数判别位 + 绑定值表达（`$1`）：0 = 全部、1 = project、2 = workspace、
// 3 = 精确目录。没有绑定行的会话（远端不应存在，本机 legacy 才有的形状）不匹配任何
// 带 scope 的查询——远端不借用本机 workspace 登记，也就不假装能做 legacy 路径归属。
const SELECT_PAGE_SQL: &str = concat!("SELECT ", page_columns!(),
    " FROM threads s LEFT JOIN session_bindings b ON b.thread_id = s.id
    WHERE s.parent_thread_id IS NULL AND s.hidden = 0 AND s.archived = $8 AND s.message_count > 0
      AND ($1 = 0
        OR ($1 = 1 AND b.project_id = $2)
        OR ($1 = 2 AND s.workspace_id = $2)
        OR ($1 = 3 AND s.workspace_id = $2 AND b.relative_cwd = $3)
        OR ($1 = 4 AND EXISTS (SELECT 1 FROM workspaces w WHERE w.id = s.workspace_id AND w.machine_id = $2)))
      AND ($4 = 0 OR (s.updated_at, s.id) < ($5, $6))
    ORDER BY s.updated_at DESC, s.id DESC LIMIT $7"
);

/// 会话事实行（含绑定、frozen、继承区）。
pub(in crate::sessions::remote) fn select_session_statement(id: &str) -> StatementSpec {
    StatementSpec::new(SELECT_SESSION_SQL, vec![Value::Text(id.to_owned())])
}

/// 会话 metadata 行（不含大字段）。
pub(in crate::sessions::remote) fn select_meta_statement(id: &str) -> StatementSpec {
    StatementSpec::new(SELECT_META_SQL, vec![Value::Text(id.to_owned())])
}

/// The parent marker distinguishes unregistered children from unbound roots.
pub(in crate::sessions::remote) fn select_binding_statement(id: &str) -> StatementSpec {
    StatementSpec::new(
        concat!(
            "SELECT ",
            binding_columns!(),
            ", s.parent_thread_id
                 FROM threads s LEFT JOIN session_bindings b ON b.thread_id = s.id WHERE s.id = $1"
        ),
        vec![Value::Text(id.to_owned())],
    )
}

/// 恢复准入的 frozen 单列读取。
pub(in crate::sessions::remote) fn select_frozen_statement(id: &str) -> StatementSpec {
    StatementSpec::new(SELECT_FROZEN_SQL, vec![Value::Text(id.to_owned())])
}

/// 直接子会话 metadata。
pub(in crate::sessions::remote) fn select_children_statement(parent: &str) -> StatementSpec {
    StatementSpec::new(SELECT_CHILDREN_SQL, vec![Value::Text(parent.to_owned())])
}

/// 整棵树 metadata（含根自身）。
pub(in crate::sessions::remote) fn select_tree_statement(root: &str) -> StatementSpec {
    StatementSpec::new(SELECT_TREE_SQL, vec![Value::Text(root.to_owned())])
}

/// 自有 payload 行（按 canonical 插入序，即 `rowid`）。
pub(in crate::sessions::remote) fn select_messages_statement(id: &str) -> StatementSpec {
    StatementSpec::new(SELECT_MESSAGES_SQL, vec![Value::Text(id.to_owned())])
}

pub(in crate::sessions::remote) fn active_flags_statement(id: &str) -> StatementSpec {
    StatementSpec::new(
        "SELECT message_id, truncated, excluded, projection FROM messages
         WHERE thread_id = $1 AND excluded = 0 AND (truncated = 1 OR projection IS NOT NULL)",
        vec![Value::Text(id.to_owned())],
    )
}

/// 有效范围的自有 payload 行（同一顺序，读取条件带未排除约束）。
pub(in crate::sessions::remote) fn select_active_messages_statement(id: &str) -> StatementSpec {
    StatementSpec::new(SELECT_ACTIVE_MESSAGES_SQL, vec![Value::Text(id.to_owned())])
}

/// 回退候选阶段一：`cursor` 为 `Some(rowid)` 时严格向更早方向继续，`None` 为首批。
pub(in crate::sessions::remote) fn rewind_candidate_rows_statement(
    id: &str,
    cursor: Option<i64>,
    limit: i64,
) -> StatementSpec {
    StatementSpec::new(
        SELECT_REWIND_CANDIDATE_ROWS_SQL,
        vec![
            Value::Text(id.to_owned()),
            match cursor {
                Some(rowid) => Value::Integer(rowid),
                None => Value::Null,
            },
            Value::Integer(limit),
        ],
    )
}

/// 回退候选阶段二：一条语句取一个本批条目的正文（调用方按同一批次一起发）。
pub(in crate::sessions::remote) fn rewind_candidate_body_statement(
    id: &str,
    message_id: &str,
) -> StatementSpec {
    StatementSpec::new(
        SELECT_REWIND_CANDIDATE_BODY_SQL,
        vec![
            Value::Text(id.to_owned()),
            Value::Text(message_id.to_owned()),
        ],
    )
}

/// 分页读取的边界事实：`anchor` 为 `None` 时锚点列恒为 NULL（首屏）。
pub(in crate::sessions::remote) fn history_page_bounds_statement(
    id: &str,
    anchor: Option<&str>,
) -> StatementSpec {
    StatementSpec::new(
        SELECT_HISTORY_PAGE_BOUNDS_SQL,
        vec![
            Value::Text(id.to_owned()),
            match anchor {
                Some(anchor) => Value::Text(anchor.to_owned()),
                None => Value::Null,
            },
        ],
    )
}

/// Fetch small bodies for the current candidates. Large rows stay in storage
/// until their specific message identity needs to be inspected.
pub(in crate::sessions::remote) fn history_anchor_rows_statement(
    id: &str,
    after: i64,
    tail: i64,
    limit: i64,
) -> StatementSpec {
    StatementSpec::new(
        "SELECT m.message_id, m.rowid, CASE WHEN octet_length(m.content) <= 8192 THEN m.content END,
                (SELECT MAX(last.rowid) FROM messages last WHERE last.thread_id = $1 AND last.excluded = 0)
         FROM messages m WHERE m.thread_id = $1 AND m.excluded = 0 AND m.rowid > $2 AND m.rowid <= $3
         ORDER BY m.rowid LIMIT $4",
        vec![Value::Text(id.to_owned()), Value::Integer(after), Value::Integer(tail), Value::Integer(limit)],
    )
}

/// 一页候选行（阶段一：身份 + 存储序键 + 正文长度）：`anchor` 为 `None` 时取最新
/// `limit + 1` 行（只有首屏会这样调用）。
pub(in crate::sessions::remote) fn history_page_rows_statement(
    id: &str,
    anchor: Option<&str>,
    limit: i64,
) -> StatementSpec {
    StatementSpec::new(
        SELECT_HISTORY_PAGE_ROWS_SQL,
        vec![
            Value::Text(id.to_owned()),
            match anchor {
                Some(anchor) => Value::Text(anchor.to_owned()),
                None => Value::Null,
            },
            Value::Integer(limit),
        ],
    )
}

/// 阶段二：一条语句取**一个**本页条目的正文（一条语句一个 id，调用方按同一批次一起发）。
pub(in crate::sessions::remote) fn history_page_body_statement(
    id: &str,
    message_id: &str,
) -> StatementSpec {
    StatementSpec::new(
        SELECT_HISTORY_PAGE_BODY_SQL,
        vec![
            Value::Text(id.to_owned()),
            Value::Text(message_id.to_owned()),
        ],
    )
}

/// 自有有效条目的存储序键（一次有界读取，不读正文）。
pub(in crate::sessions::remote) fn active_entry_positions_statement(id: &str) -> StatementSpec {
    StatementSpec::new(
        SELECT_ACTIVE_ENTRY_POSITIONS_SQL,
        vec![Value::Text(id.to_owned())],
    )
}

/// 段读取的边界事实（会话存在吗 + 固定上界）。
pub(in crate::sessions::remote) fn history_segment_tail_statement(id: &str) -> StatementSpec {
    StatementSpec::new(
        SELECT_HISTORY_SEGMENT_TAIL_SQL,
        vec![Value::Text(id.to_owned())],
    )
}

/// 首段候选行（阶段一：身份 + 存储序键 + 正文长度，最多 `limit + 1` 行）。
pub(in crate::sessions::remote) fn history_segment_head_statement(
    id: &str,
    limit: i64,
) -> StatementSpec {
    StatementSpec::new(
        SELECT_HISTORY_SEGMENT_HEAD_SQL,
        vec![Value::Text(id.to_owned()), Value::Integer(limit)],
    )
}

/// 锚点存在性（游标的 `after` 行的 `rowid`；`NULL` = 已不在）。
pub(in crate::sessions::remote) fn history_segment_anchor_statement(
    id: &str,
    after: i64,
) -> StatementSpec {
    StatementSpec::new(
        SELECT_HISTORY_SEGMENT_ANCHOR_SQL,
        vec![Value::Text(id.to_owned()), Value::Integer(after)],
    )
}

/// 非首段候选行（`rowid > after AND rowid <= tail`，升序，最多 `limit + 1` 行）。
pub(in crate::sessions::remote) fn history_segment_next_statement(
    id: &str,
    after: i64,
    tail: i64,
    limit: i64,
) -> StatementSpec {
    StatementSpec::new(
        SELECT_HISTORY_SEGMENT_NEXT_SQL,
        vec![
            Value::Text(id.to_owned()),
            Value::Integer(after),
            Value::Integer(tail),
            Value::Integer(limit),
        ],
    )
}

/// 首段的继承区快照（单行一列）。
pub(in crate::sessions::remote) fn inherited_context_statement(id: &str) -> StatementSpec {
    StatementSpec::new(
        SELECT_INHERITED_CONTEXT_SQL,
        vec![Value::Text(id.to_owned())],
    )
}

/// 单个条目的 flags（有界读取的一条语句；调用方按 id 集合组成同一批次）。
pub(in crate::sessions::remote) fn select_message_flags_statement(
    id: &str,
    message_id: &str,
) -> StatementSpec {
    StatementSpec::new(
        SELECT_MESSAGE_FLAGS_SQL,
        vec![
            Value::Text(id.to_owned()),
            Value::Text(message_id.to_owned()),
        ],
    )
}

/// 分页列举语句；scope 里的相对路径必须能作为文本绑定，否则拒绝（不猜路径相等，
/// 也不把「无法比较」静默当成「匹配为空」）。
pub(in crate::sessions::remote) fn page_statement(
    query: &ScopedThreadQuery,
) -> SessionResourceResult<StatementSpec> {
    page_statement_for_archive(query, false)
}

pub(in crate::sessions::remote) fn page_statement_for_archive(
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
        SELECT_PAGE_SQL,
        vec![
            super::super::session_codec::int_value(kind),
            scope_id,
            relative,
            super::super::session_codec::int_value(cursor_flag),
            cursor_at,
            cursor_id,
            super::super::session_codec::int_value(limit),
            super::super::session_codec::int_value(i64::from(archived)),
        ],
    ))
}
