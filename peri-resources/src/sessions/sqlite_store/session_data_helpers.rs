use super::*;
use sqlx::AssertSqlSafe;

pub(in crate::sessions::sqlite_store) fn validate_unbound_legacy_frozen(
    frozen: Option<&str>,
) -> anyhow::Result<()> {
    anyhow::ensure!(frozen.is_none(), WorkspaceError::InvalidBinding);
    Ok(())
}

pub(super) async fn machine_id_on(
    pool: &sqlx::SqlitePool,
    id: &ThreadId,
) -> SessionResourceResult<Option<String>> {
    let identity: Option<(String,)> =
        sqlx::query_as("SELECT w.machine_id FROM threads t JOIN workspaces w ON w.id = t.workspace_id WHERE t.id = ?1")
            .bind(id.as_str())
            .fetch_optional(pool)
            .await
            .map_err(|error| map_sqlx(&error))?;
    Ok(identity.map(|row| row.0))
}

/// 线程树（含自身）：删除与归属判定都用同一遍递归。
pub(in crate::sessions) async fn thread_tree_on(
    connection: &mut SqliteConnection,
    root: &ThreadId,
) -> anyhow::Result<Vec<String>> {
    let rows: Vec<(String,)> = sqlx::query_as(
        "WITH RECURSIVE session_tree AS (
            SELECT id FROM threads WHERE id = ?1
            UNION ALL
            SELECT t.id FROM threads t INNER JOIN session_tree st ON t.parent_thread_id = st.id
        )
        SELECT id FROM session_tree",
    )
    .bind(root.as_str())
    .fetch_all(&mut *connection)
    .await?;
    Ok(rows.into_iter().map(|(id,)| id).collect())
}

/// 会话树根的持久化事实：沿 parent 链向上回溯。
pub(in crate::sessions) async fn thread_root_on(
    connection: &mut SqliteConnection,
    id: &ThreadId,
) -> anyhow::Result<ThreadId> {
    let mut current = id.clone();
    let mut visited = HashSet::new();
    loop {
        if !visited.insert(current.clone()) {
            anyhow::bail!("cyclic thread ancestry");
        }
        let row: Option<(Option<String>,)> =
            sqlx::query_as("SELECT parent_thread_id FROM threads WHERE id = ?1")
                .bind(current.as_str())
                .fetch_optional(&mut *connection)
                .await?;
        match row {
            Some((Some(parent),)) => current = parent,
            Some((None,)) => return Ok(current),
            None => anyhow::bail!("session row is missing"),
        }
    }
}

/// 绑定行的事实：绑定、指向已消失的登记、或没有绑定行。
pub(in crate::sessions) enum BindingRowState {
    Bound(SessionBinding),
    /// 有绑定行，但它指向的本机登记不存在：记录在、身份无法在本机验证。
    Absent,
}

/// 绑定行读取的语句原文（主键单行；计划证据与生产读取同一份文本）。
pub(in crate::sessions) const BINDING_ROW_STATE_SQL: &str =
    "SELECT schema_version, project_id, workspace_id, relative_cwd
         FROM session_bindings WHERE thread_id = ?1";

/// 会话自身 frozen 列的语句原文（主键单行；计划证据与生产读取同一份文本）。
pub(in crate::sessions) const FROZEN_CONTEXT_SQL: &str =
    "SELECT frozen_context FROM threads WHERE id = ?1";

/// 会话存在性判据的语句原文（主键单行；计划证据与生产读取同一份文本）。
pub(in crate::sessions) const THREAD_EXISTS_SQL: &str = "SELECT 1 FROM threads WHERE id = ?1";

/// 绑定行的事实分类；不判断 legacy（那是本机来源证据与执行面的联合结论）。
pub(in crate::sessions) async fn binding_row_state_on(
    connection: &mut SqliteConnection,
    id: &ThreadId,
) -> SessionResourceResult<BindingRowState> {
    let row: Option<(i64, String, String, String)> = sqlx::query_as(BINDING_ROW_STATE_SQL)
        .bind(id.as_str())
        .fetch_optional(&mut *connection)
        .await
        .map_err(|error| map_sqlx(&error))?;
    let Some((version, project_id, workspace_id, relative_cwd)) = row else {
        return Ok(BindingRowState::Absent);
    };
    // 版本不被本构建接受与记录损坏是两种事实，分开报告。
    if version != i64::from(SESSION_BINDING_VERSION) {
        return Err(SessionResourceError::new(
            SessionResourceErrorKind::Unsupported,
        ));
    }
    let binding =
        workspace_store::decode_binding((version, project_id, workspace_id, relative_cwd))
            .map_err(|_| corrupt("session binding is not decodable"))?;
    Ok(BindingRowState::Bound(binding))
}

/// 会话自身的 frozen 列；`None` 表示从未保存过快照（legacy 缺失）。
pub(in crate::sessions) async fn frozen_bytes_on(
    connection: &mut SqliteConnection,
    id: &ThreadId,
) -> anyhow::Result<Option<String>> {
    let row: Option<(Option<String>,)> = sqlx::query_as(FROZEN_CONTEXT_SQL)
        .bind(id.as_str())
        .fetch_optional(&mut *connection)
        .await?;
    match row {
        Some((frozen,)) => Ok(frozen),
        None => anyhow::bail!("session row is missing"),
    }
}

/// 恢复准入的 frozen 状态：单列读取，并区分「行缺失」与「列从未保存」。
pub(in crate::sessions) async fn frozen_state_on(
    connection: &mut SqliteConnection,
    id: &ThreadId,
) -> SessionResourceResult<FrozenState> {
    let row: Option<(Option<String>,)> = sqlx::query_as(FROZEN_CONTEXT_SQL)
        .bind(id.as_str())
        .fetch_optional(&mut *connection)
        .await
        .map_err(|error| map_sqlx(&error))?;
    match row {
        Some((Some(bytes),)) => Ok(FrozenState::Present(FrozenSnapshotBytes::new(bytes))),
        Some((None,)) => Ok(FrozenState::LegacyAbsent),
        None => Err(not_found()),
    }
}

/// `NewSession` 的 `threads` 行插入参数（创建路径与 fork/child 共用同一列形状）。
pub(in crate::sessions) fn new_session_row<'a>(
    input: &'a NewSession,
    snapshot_at_message_id: Option<&'a str>,
    frozen: Option<&'a str>,
    message_count: i64,
) -> ThreadRowInsert<'a> {
    ThreadRowInsert {
        id: &input.thread_id,
        title: input.meta.title.as_deref(),
        cwd: &input.meta.cwd,
        created_at: &input.created_at,
        updated_at: &input.created_at,
        message_count,
        parent_thread_id: input.meta.parent_thread_id.as_deref(),
        snapshot_at_message_id,
        hidden: input.meta.hidden,
        cancel_policy: input.meta.cancel_policy.as_str(),
        config: None,
        agent_status: AgentStatus::Active.as_str(),
        frozen_context: frozen,
        owner_workspace_id: Some(&input.binding.workspace_id),
    }
}

/// `NewSessionDraft` 的 `threads` 行插入参数：与 [`new_session_row`] 同一列形状，
/// 只有 `frozen_context` 固定为 `NULL`（内容准入在取得所有权之后定稿）。
pub(in crate::sessions) fn new_session_draft_row<'a>(
    draft: &'a NewSessionDraft,
    snapshot_at_message_id: Option<&'a str>,
    message_count: i64,
) -> ThreadRowInsert<'a> {
    ThreadRowInsert {
        id: &draft.thread_id,
        title: draft.meta.title.as_deref(),
        cwd: &draft.meta.cwd,
        created_at: &draft.created_at,
        updated_at: &draft.created_at,
        message_count,
        parent_thread_id: draft.meta.parent_thread_id.as_deref(),
        snapshot_at_message_id,
        hidden: draft.meta.hidden,
        cancel_policy: draft.meta.cancel_policy.as_str(),
        config: None,
        agent_status: AgentStatus::Active.as_str(),
        frozen_context: None,
        owner_workspace_id: Some(&draft.binding.workspace_id),
    }
}

/// 会话是否存在（用于 fork 来源、child 父行等关系检查）。
pub(in crate::sessions) async fn thread_exists_on(
    connection: &mut SqliteConnection,
    id: &ThreadId,
) -> anyhow::Result<bool> {
    let row: Option<(i64,)> = sqlx::query_as(THREAD_EXISTS_SQL)
        .bind(id.as_str())
        .fetch_optional(&mut *connection)
        .await?;
    Ok(row.is_some())
}

// ─── 端口实现 ─────────────────────────────────────────────────────────────────

/// 待写入 `messages` 的一行：正文已完成序列化，SQL 文本里不出现任何数据。
pub(in crate::sessions) struct MessageInsertRow {
    pub id: String,
    pub role: &'static str,
    pub content: String,
}

/// `messages` 插入的列数（message_id、thread_id、role、content）。
const MESSAGE_INSERT_COLUMNS: usize = 4;

/// 每条 INSERT 的绑定变量上限。SQLite 旧版上限是 999，取 900 留余量；按列数折算成
/// 行数后分块（当前 225 行/语句）。分块不改变事务边界：调用方仍在同一事务里执行全部分块。
const MESSAGE_INSERT_MAX_VARIABLES: usize = 900;

/// 单条多行 INSERT 的行数上限；列数变化时分块自动收紧。
pub(in crate::sessions) fn message_insert_chunk_rows() -> usize {
    (MESSAGE_INSERT_MAX_VARIABLES / MESSAGE_INSERT_COLUMNS).max(1)
}

/// 分块多行插入 `messages`：保持传入顺序，每块仍全部走绑定参数。
///
/// 中途失败直接上抛，已写入的块由调用方的事务整批回滚（与本函数出现前的逐行插入同一
/// 可观察结果）。`thread_id` 每行重复绑定一次，不为省变量数复用到跨行占位符。
pub(in crate::sessions) async fn insert_message_rows(
    connection: &mut SqliteConnection,
    thread_id: &ThreadId,
    rows: &[MessageInsertRow],
) -> Result<(), sqlx::Error> {
    for chunk in rows.chunks(message_insert_chunk_rows()) {
        let mut statement =
            String::from("INSERT INTO messages (message_id, thread_id, role, content) VALUES ");
        for index in 0..chunk.len() {
            if index > 0 {
                statement.push(',');
            }
            let base = index * MESSAGE_INSERT_COLUMNS;
            statement.push_str(&format!(
                "(?{},?{},?{},?{})",
                base + 1,
                base + 2,
                base + 3,
                base + 4
            ));
        }
        let mut query = sqlx::query(AssertSqlSafe(statement));
        for row in chunk {
            query = query
                .bind(&row.id)
                .bind(thread_id.as_str())
                .bind(row.role)
                .bind(&row.content);
        }
        query.execute(&mut *connection).await?;
    }
    Ok(())
}

/// 批量插入 canonical payload；返回批次内 ID 集合。
pub(in crate::sessions) async fn insert_history_rows(
    connection: &mut SqliteConnection,
    thread_id: &ThreadId,
    payloads: &[PersistedPayload],
) -> anyhow::Result<HashSet<MessageId>> {
    let mut ids = HashSet::with_capacity(payloads.len());
    let mut rows = Vec::with_capacity(payloads.len());
    for payload in payloads {
        if !ids.insert(payload.id()) {
            anyhow::bail!("history batch repeats a message id");
        }
        rows.push(MessageInsertRow {
            id: payload.id().as_uuid().to_string(),
            role: payload_role(payload),
            content: serialize_persisted_payload(payload)?,
        });
    }
    insert_message_rows(connection, thread_id, &rows).await?;
    Ok(ids)
}

pub(in crate::sessions) fn projection_json(
    flags: &MessageFlags,
) -> SessionResourceResult<Option<String>> {
    flags
        .projection
        .as_ref()
        .map(serde_json::to_string)
        .transpose()
        .map_err(|_| corrupt("message projection is not serializable"))
}

/// 历史变更后同步派生视图：计数、时间戳与缓存版本一起更新。
pub(in crate::sessions) async fn refresh_history_derivations(
    connection: &mut SqliteConnection,
    id: &ThreadId,
) -> SessionResourceResult<()> {
    let now = peri_time::now_utc_rfc3339();
    sqlx::query(
        "UPDATE threads SET updated_at = ?1,
                message_count = (SELECT COUNT(*) FROM messages WHERE thread_id = ?2)
             WHERE id = ?2",
    )
    .bind(&now)
    .bind(id.as_str())
    .execute(&mut *connection)
    .await
    .map_err(|error| map_sqlx(&error))?;
    Ok(())
}
