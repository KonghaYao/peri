use super::*;

pub(super) async fn machine_id_on(
    pool: &sqlx::SqlitePool,
    id: &ThreadId,
) -> SessionResourceResult<Option<String>> {
    let table: Option<(i64,)> = sqlx::query_as(
        "SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = 'session_environments'",
    )
    .fetch_optional(pool)
    .await
    .map_err(|error| map_sqlx(&error))?;
    if table.is_none() {
        return Ok(None);
    }
    let identity: Option<(String,)> =
        sqlx::query_as("SELECT machine_id FROM session_environments WHERE thread_id = ?1")
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

/// 本机库自己的会话事实：数据与执行在**同一个库**时（迁移桥、本机组合的内部调用）的读法。
///
/// 判定与远端组合完全一致，差别只在事实来源：这里从本机 `session_bindings` 与 `threads`
/// 父链读出调用方在远端组合里要从数据端口取的三件事（绑定字节、这棵树有没有绑定、树根）。
/// 远端组合**不能**用它——那时本机没有这条会话的行，三件事只能由数据端口回答。
impl SqliteSessionDatabase {
    pub(in crate::sessions) async fn local_session_facts(
        &self,
        id: &ThreadId,
    ) -> anyhow::Result<SessionFacts> {
        let mut connection = self.pool.acquire().await?;
        let root = thread_root_on(&mut connection, id).await?;
        Ok(SessionFacts { root })
    }
}

/// 绑定行的事实：绑定、指向已消失的登记、或没有绑定行。
pub(in crate::sessions) enum BindingRowState {
    Bound(SessionBinding),
    /// 有绑定行，但它指向的本机登记不存在：记录在、身份无法在本机验证。
    Absent,
}

/// 绑定行的事实分类；不判断 legacy（那是本机来源证据与执行面的联合结论）。
pub(in crate::sessions) async fn binding_row_state_on(
    connection: &mut SqliteConnection,
    id: &ThreadId,
) -> SessionResourceResult<BindingRowState> {
    let row: Option<(i64, String, String, String)> = sqlx::query_as(
        "SELECT schema_version, project_id, workspace_id, relative_cwd
         FROM session_bindings WHERE thread_id = ?1",
    )
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
    let row: Option<(Option<String>,)> =
        sqlx::query_as("SELECT frozen_context FROM threads WHERE id = ?1")
            .bind(id.as_str())
            .fetch_optional(&mut *connection)
            .await?;
    match row {
        Some((frozen,)) => Ok(frozen),
        None => anyhow::bail!("session row is missing"),
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
    }
}

/// 会话是否存在（用于 fork 来源、child 父行等关系检查）。
pub(in crate::sessions) async fn thread_exists_on(
    connection: &mut SqliteConnection,
    id: &ThreadId,
) -> anyhow::Result<bool> {
    let row: Option<(i64,)> = sqlx::query_as("SELECT 1 FROM threads WHERE id = ?1")
        .bind(id.as_str())
        .fetch_optional(&mut *connection)
        .await?;
    Ok(row.is_some())
}

// ─── 端口实现 ─────────────────────────────────────────────────────────────────

/// 批量插入 canonical payload；返回批次内 ID 集合。
pub(in crate::sessions) async fn insert_history_rows(
    connection: &mut SqliteConnection,
    thread_id: &ThreadId,
    payloads: &[PersistedPayload],
) -> anyhow::Result<HashSet<MessageId>> {
    let mut ids = HashSet::with_capacity(payloads.len());
    for payload in payloads {
        if !ids.insert(payload.id()) {
            anyhow::bail!("history batch repeats a message id");
        }
        sqlx::query(
            "INSERT INTO messages (message_id, thread_id, role, content)
             VALUES (?1, ?2, ?3, ?4)",
        )
        .bind(payload.id().as_uuid().to_string())
        .bind(thread_id.as_str())
        .bind(payload_role(payload))
        .bind(serialize_persisted_payload(payload)?)
        .execute(&mut *connection)
        .await?;
    }
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
    let now = Utc::now().to_rfc3339();
    sqlx::query(
        "UPDATE threads SET updated_at = ?1,
                message_count = (SELECT COUNT(*) FROM messages WHERE thread_id = ?2),
                cached_context = NULL,
                context_cache_epoch = context_cache_epoch + 1
             WHERE id = ?2",
    )
    .bind(&now)
    .bind(id.as_str())
    .execute(&mut *connection)
    .await
    .map_err(|error| map_sqlx(&error))?;
    Ok(())
}
