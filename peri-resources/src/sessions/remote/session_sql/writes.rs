//! Bound statements for session creation, metadata, and work state changes.

use super::super::sql::StatementSpec;
use crate::sessions::canonical;
use peri_acp_types::session_resources::NewSession;
use peri_acp_types::session_resources::NewSessionDraft;
use peri_acp_types::session_resources::SessionMetaPatch;
use peri_acp_types::session_resources::SessionResourceError;
use peri_acp_types::session_resources::SessionResourceErrorKind;
use peri_acp_types::session_resources::SessionResourceResult;
use peri_acp_types::store::MessageFlags;
use peri_acp_types::store::PersistedPayload;
use peri_acp_types::thread::AgentStatus;
use peri_acp_types::thread::WorkState;
use peri_acp_types::workspace::SessionBinding;
use peri_acp_types::workspace::WorkspaceId;
use turso_serverless::Value;

// ─── 写入语句 ─────────────────────────────────────────────────────────────────

pub(super) const INSERT_THREAD_SQL: &str =
    "INSERT INTO threads (id, title, cwd, created_at, updated_at, message_count,
        parent_thread_id, snapshot_at_message_id, hidden, cancel_policy, config,
        frozen_context, agent_status, workspace_id)
     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13,
        COALESCE(?14, (SELECT workspace_id FROM threads WHERE id = ?7)))";

/// 未发布创建（J2 第一阶段）：与整份创建同一列形状，只有 `frozen_context` 写 NULL。
///
/// 远端写的是**草稿**，内容由
/// [`crate::sessions::remote::session_data::RemoteSessionData::write_commit_frozen`] 一次性补上。
pub(super) const INSERT_THREAD_DRAFT_SQL: &str =
    "INSERT INTO threads (id, title, cwd, created_at, updated_at, message_count,
        parent_thread_id, snapshot_at_message_id, hidden, cancel_policy, config,
        frozen_context, agent_status, workspace_id)
     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, NULL, ?12,
        COALESCE(?13, (SELECT workspace_id FROM threads WHERE id = ?7)))";

/// 一次性提交 frozen（write-once CAS）：只对尚未提交的草稿生效。
pub(super) const COMMIT_FROZEN_SQL: &str =
    "UPDATE threads SET frozen_context = ?1 WHERE id = ?2 AND frozen_context IS NULL";

/// 删除整段会话历史的全部条目（与 `canonical::THREAD_CHILD_DELETES` 同一份语句）。
pub(in crate::sessions::remote) const DELETE_SESSION_MESSAGES_SQL: &str =
    canonical::DELETE_MESSAGES_BY_THREAD_SQL;

/// 删除会话的不可变绑定行（统一后绑定住在 `session_bindings`，不再是会话行上的扁平列）。
pub(in crate::sessions::remote) const DELETE_SESSION_BINDINGS_SQL: &str =
    canonical::DELETE_BINDINGS_BY_THREAD_SQL;

/// 删除会话行（与 `canonical::DELETE_THREAD_ROW_SQL` 同一份语句）。
pub(in crate::sessions::remote) const DELETE_SESSION_SQL: &str = canonical::DELETE_THREAD_ROW_SQL;

/// write-once 未发布创建的撤销语句集（fork 等失败补偿）：四条删除**不带** frozen 判据。
///
/// 与 [`revoke_draft_statements`] 的唯一差别就是判据：那一条按调用方语义分档
/// （两阶段草稿要求 `frozen IS NULL`），不由本函数推断。
pub(in crate::sessions::remote) fn revoke_session_statements(id: &str) -> Vec<StatementSpec> {
    [
        DELETE_SESSION_MESSAGES_SQL,
        DELETE_SESSION_BINDINGS_SQL,
        DELETE_SESSION_SQL,
    ]
    .into_iter()
    .map(|sql| StatementSpec::new(sql, vec![Value::Text(id.to_owned())]))
    .collect()
}

/// 未发布创建的撤销：各条删除都带同一判据（`frozen_context IS NULL`），因此同一批里
/// 「会话行还在不在」与「历史/绑定删没删」不会分叉——已提交 frozen 的会话一条都不删。
pub(super) const DELETE_DRAFT_MESSAGES_SQL: &str = concat!(
    "DELETE FROM messages WHERE thread_id = ?1",
    " AND EXISTS (SELECT 1 FROM threads WHERE id = ?1 AND frozen_context IS NULL)"
);
pub(super) const DELETE_DRAFT_BINDINGS_SQL: &str = concat!(
    "DELETE FROM session_bindings WHERE thread_id = ?1",
    " AND EXISTS (SELECT 1 FROM threads WHERE id = ?1 AND frozen_context IS NULL)"
);
pub(super) const DELETE_DRAFT_SESSION_SQL: &str =
    "DELETE FROM threads WHERE id = ?1 AND frozen_context IS NULL";

/// 不可变绑定行插入：与本机 `session_rows.rs::insert_binding_row` 同一份语句，绑定住在
/// `session_bindings` 表里（不再是会话行上的四个扁平列）。
///
/// `discovery_snapshot` 由调用方投影成最小执行根后传入（native `{"root":...}`、virtual
/// 另带 `kind`）；`?6` 为空时从父行复制——父行已是最小形态，自复制天然正确。
pub(super) const INSERT_BINDING_SQL: &str =
    "INSERT INTO session_bindings (thread_id, schema_version, project_id, workspace_id, relative_cwd,
        discovery_snapshot)
     VALUES (?1, ?2, ?3, ?4, ?5, COALESCE(?6,
        (SELECT discovery_snapshot FROM session_bindings WHERE thread_id = ?7)))";

/// 继承区写入：与本机 child 路径同一份语句（创建行之后单独写一次，INSERT 不多带一列）。
///
/// 不在这里写 `inherited_*_index` 派生行：远端读取一次性取完整列，从不查询派生索引；
/// 索引缺行是合法状态（本地读取会回退扫描并按需回填）。
pub(super) const UPDATE_INHERITED_SQL: &str =
    "UPDATE threads SET inherited_context = ?1 WHERE id = ?2";

/// 历史行插入：列清单与本机 `messages` 一致（含 `role`），顺序由 `rowid` 承载。
pub(super) const INSERT_MESSAGE_SQL: &str = "INSERT INTO messages (
    message_id, thread_id, role, content, truncated, excluded, projection)
    VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)";

/// 定向更新：`Some(None)` 清除、`None` 保持不变、`None` 之外的值照写。
///
/// 用「标志位 + 值」的 CASE 表达，SQL 文本保持静态；没有字段要改时调用方不该调用它
/// （门面与本地 adapter 同一约定：不写、也不假装更新了时间戳）。
pub(super) const UPDATE_META_SQL: &str = "UPDATE threads SET
    updated_at = ?1,
    title = CASE WHEN ?2 = 1 THEN ?3 ELSE title END,
    agent_status = CASE WHEN ?4 = 1 THEN ?5 ELSE agent_status END,
    cancel_policy = CASE WHEN ?6 = 1 THEN ?7 ELSE cancel_policy END,
    config = CASE WHEN ?8 = 1 THEN ?9 ELSE config END
    WHERE id = ?10";

/// 一行新会话的写入参数；派生计数与时间戳由调用方给定，本层不读时钟。
pub(in crate::sessions::remote) struct SessionInsert<'session> {
    pub(in crate::sessions::remote) thread_id: &'session str,
    pub(in crate::sessions::remote) title: Option<&'session str>,
    pub(in crate::sessions::remote) cwd: &'session str,
    pub(in crate::sessions::remote) created_at: &'session str,
    /// 创建路径为 0；fork 目标为映射后的 payload 数。
    pub(in crate::sessions::remote) message_count: i64,
    pub(in crate::sessions::remote) parent_thread_id: Option<&'session str>,
    /// 快照截止消息 id（文本形式；由调用方从领域 id 定型）。
    pub(in crate::sessions::remote) snapshot_at_message_id: Option<String>,
    pub(in crate::sessions::remote) hidden: bool,
    pub(in crate::sessions::remote) cancel_policy: &'session str,
    /// 会话创建时冻结的上下文快照字节（不解释内容）。
    pub(in crate::sessions::remote) frozen: &'session str,
    /// child 的继承区 JSON；创建与 fork 目标为 `None`。
    pub(in crate::sessions::remote) inherited: Option<&'session str>,
    pub(in crate::sessions::remote) binding: &'session SessionBinding,
    /// 已生效会话的 agent 状态；新行一律 `active`。
    pub(in crate::sessions::remote) agent_status: &'session str,
    pub(in crate::sessions::remote) owner_workspace_id: Option<WorkspaceId>,
    /// 已投影的最小执行根（`binding_evidence::project`）。写入点必须投影，不能直接放完整
    /// 发现快照：绑定行只保存创建时的执行根，完整布局留在 `workspaces.discovery`。
    pub(in crate::sessions::remote) discovery_snapshot: Option<String>,
}

/// 由 [`NewSession`] 组装一行插入参数（创建/fork/child 三条路径共用同一列形状）。
///
/// `snapshot_at_message_id` 在这里定型为文本：它是记录事实，不是可再推导的引用。
pub(in crate::sessions::remote) fn session_insert<'session>(
    input: &'session NewSession,
    message_count: i64,
    inherited: Option<&'session str>,
) -> SessionInsert<'session> {
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
pub(in crate::sessions::remote) fn insert_session_statements(
    row: &SessionInsert<'_>,
) -> SessionResourceResult<Vec<StatementSpec>> {
    let relative = binding_relative_text(row.binding)?;
    // 防御性投影：即使调用方传了完整发现快照，落进绑定行的也只有最小执行根。
    let evidence = row
        .discovery_snapshot
        .as_deref()
        .map(|raw| {
            crate::sessions::binding_evidence::project(raw).map_err(|_| {
                super::super::session_data::invalid_input("workspace creation evidence is invalid")
            })
        })
        .transpose()?;
    let mut statements = vec![
        StatementSpec::new(
            INSERT_THREAD_SQL,
            vec![
                Value::Text(row.thread_id.to_owned()),
                super::super::session_codec::optional_text(row.title),
                Value::Text(row.cwd.to_owned()),
                Value::Text(row.created_at.to_owned()),
                Value::Text(row.created_at.to_owned()),
                super::super::session_codec::int_value(row.message_count),
                super::super::session_codec::optional_text(row.parent_thread_id),
                super::super::session_codec::optional_text(row.snapshot_at_message_id.as_deref()),
                super::super::session_codec::int_value(i64::from(row.hidden)),
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
                super::super::session_codec::int_value(i64::from(row.binding.schema_version)),
                Value::Text(row.binding.project_id.to_string()),
                Value::Text(row.binding.workspace_id.to_string()),
                Value::Text(relative),
                super::super::session_codec::optional_text(evidence.as_deref()),
                super::super::session_codec::optional_text(row.parent_thread_id),
            ],
        ),
    ];
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
pub(in crate::sessions::remote) fn insert_session_draft_statements(
    draft: &NewSessionDraft,
    owner_workspace_id: Option<WorkspaceId>,
    discovery_snapshot: Option<&str>,
) -> SessionResourceResult<Vec<StatementSpec>> {
    // 入库值是最小执行根；投影失败按输入错误拒绝，不把完整 JSON 落进绑定行。
    let discovery_snapshot = discovery_snapshot
        .map(|raw| {
            crate::sessions::binding_evidence::project(raw).map_err(|_| {
                super::super::session_data::invalid_input("workspace creation evidence is invalid")
            })
        })
        .transpose()?;
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
                super::super::session_codec::optional_text(draft.meta.title.as_deref()),
                Value::Text(draft.meta.cwd.clone()),
                Value::Text(draft.created_at.clone()),
                Value::Text(draft.created_at.clone()),
                super::super::session_codec::int_value(0),
                super::super::session_codec::optional_text(draft.meta.parent_thread_id.as_deref()),
                super::super::session_codec::optional_text(snapshot_at.as_deref()),
                super::super::session_codec::int_value(i64::from(draft.meta.hidden)),
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
                super::super::session_codec::int_value(i64::from(draft.binding.schema_version)),
                Value::Text(draft.binding.project_id.to_string()),
                Value::Text(draft.binding.workspace_id.to_string()),
                Value::Text(relative),
                super::super::session_codec::optional_text(discovery_snapshot.as_deref()),
                super::super::session_codec::optional_text(draft.meta.parent_thread_id.as_deref()),
            ],
        ),
    ])
}

/// 一次性提交 frozen 的语句（write-once CAS；受影响行数由调用方按 1 核对）。
pub(in crate::sessions::remote) fn commit_frozen_statement(
    id: &str,
    frozen: &str,
) -> StatementSpec {
    StatementSpec::new(
        COMMIT_FROZEN_SQL,
        vec![Value::Text(frozen.to_owned()), Value::Text(id.to_owned())],
    )
}

/// 撤销未发布创建的语句集：三条删除共用 `frozen_context IS NULL` 判据（先子后父）。
pub(in crate::sessions::remote) fn revoke_draft_statements(id: &str) -> Vec<StatementSpec> {
    [
        DELETE_DRAFT_MESSAGES_SQL,
        DELETE_DRAFT_BINDINGS_SQL,
        DELETE_DRAFT_SESSION_SQL,
    ]
    .into_iter()
    .map(|sql| StatementSpec::new(sql, vec![Value::Text(id.to_owned())]))
    .collect()
}

/// 历史行的插入语句：flags 与内容一次写入（fork 目标由门面完成 ID 重映射，本层不重跑算法）。
pub(in crate::sessions::remote) fn insert_message_statement(
    thread_id: &str,
    payload: &PersistedPayload,
    flags: Option<&MessageFlags>,
) -> SessionResourceResult<StatementSpec> {
    Ok(StatementSpec::new(
        INSERT_MESSAGE_SQL,
        super::super::session_codec::payload_params(thread_id, payload, flags)?,
    ))
}

/// durable 工作意图：只写这一列，不碰 `updated_at`，也不改运行态。
pub(super) const UPDATE_WORK_STATE_SQL: &str = "UPDATE threads SET work_state = ?1 WHERE id = ?2";

/// 上下文占用快照：只写这一列，不碰 `updated_at`；`None` 绑定 NULL（未知，不是零占用）。
pub(super) const UPDATE_CONTEXT_USAGE_SQL: &str =
    "UPDATE threads SET context_usage_tokens = ?1 WHERE id = ?2";

/// 工作意图写入语句。
pub(in crate::sessions::remote) fn work_state_statement(
    id: &str,
    state: WorkState,
) -> StatementSpec {
    StatementSpec::new(
        UPDATE_WORK_STATE_SQL,
        vec![
            Value::Text(state.as_str().to_owned()),
            Value::Text(id.to_owned()),
        ],
    )
}

/// 上下文占用写入语句；超出列范围的计数是输入错误，不冒充「暂不可用」。
pub(in crate::sessions::remote) fn context_usage_statement(
    id: &str,
    tokens: Option<u64>,
) -> SessionResourceResult<StatementSpec> {
    let tokens = match tokens {
        Some(tokens) => {
            super::super::session_codec::int_value(i64::try_from(tokens).map_err(|_| {
                SessionResourceError::new(SessionResourceErrorKind::InvalidInput {
                    detail: "context usage tokens exceed the storable range".to_owned(),
                })
            })?)
        }
        None => Value::Null,
    };
    Ok(StatementSpec::new(
        UPDATE_CONTEXT_USAGE_SQL,
        vec![tokens, Value::Text(id.to_owned())],
    ))
}

/// 定向 metadata 更新语句（`None` 不动、`Some(None)` 清除）。
pub(in crate::sessions::remote) fn update_meta_statement(
    id: &str,
    patch: &SessionMetaPatch,
    now: &str,
) -> StatementSpec {
    let (title_flag, title) = match &patch.title {
        Some(value) => (
            1_i64,
            super::super::session_codec::optional_text(value.as_deref()),
        ),
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
        Some(value) => (
            1_i64,
            super::super::session_codec::optional_text(value.as_deref()),
        ),
        None => (0, Value::Null),
    };
    StatementSpec::new(
        UPDATE_META_SQL,
        vec![
            Value::Text(now.to_owned()),
            super::super::session_codec::int_value(title_flag),
            title,
            super::super::session_codec::int_value(status_flag),
            status,
            super::super::session_codec::int_value(policy_flag),
            policy,
            super::super::session_codec::int_value(config_flag),
            config,
            Value::Text(id.to_owned()),
        ],
    )
}

/// 绑定里的相对 cwd 必须能作为文本存储，且形状上确实是「相对 workspace 的路径」。
///
/// 这里只做形状校验：目录是否存在、是否与已登记 workspace 一致由本机 workspace/门面判定，
/// 云端没有目录可查，也不承担那个判断。
pub(in crate::sessions::remote) fn binding_relative_text(
    binding: &SessionBinding,
) -> SessionResourceResult<String> {
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
