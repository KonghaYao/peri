//! Swarm 窄口的远端实现：共享会话连接上的静态 SQL（design §4.5 方案 A）。
//!
//! 本模块不另开连接：每次调用经 [`RemoteSessionData::store`] 取**门面当前持有**的那条连接
//! （含代际失效后的重建与关闭后的明确失败），因此 Swarm 事实与 Peri 会话事实落在同一个
//! 逻辑库里，连接生命周期也只由部署关闭权决定。
//!
//! 写法沿用远程会话 adapter 的三条既有约束：
//!
//! - SQL 全部是 `&'static str`（[`StatementSpec`]），动态内容只走位置绑定参数；
//! - 写走 [`RemoteStore::apply_batch`] 的托管事务批（`BEGIN IMMEDIATE` … `COMMIT`），
//!   失败按既有三分类解释：`QualificationConflict` ⇒ `Conflict`（不改号、不重试）、
//!   `NotApplied` ⇒ 分类错误、`Unknown` ⇒ `PersistenceUncertain`（不重试写）；
//! - 读走单语句读取或一致读，回复不完整按错误上报，不降级成「没有数据」。
//!
//! `swarm_put_input` 的幂等边界是 `(logical_id, input_id)` 主键上的
//! `INSERT ... ON CONFLICT DO NOTHING`：影响 1 行即 `Inserted`，0 行再一致读回既有收据。
//! 同 ID 不同 fingerprint 不在本层判冲突——端口只返回事实（design §4.4）。

use std::sync::Arc;

use async_trait::async_trait;
use peri_acp_types::session_resources::{
    SessionResourceError, SessionResourceErrorKind, SessionResourceResult,
};
use turso_serverless::Value;

use super::mutation::RemoteStore;
use super::session_data::RemoteSessionData;
use super::sql::{int_at, text_at, StatementSpec};
use crate::sessions::{
    SwarmBindingRow, SwarmInputPut, SwarmInputRow, SwarmItemRow, SwarmSessionRow,
    SwarmSessionUpdate, SwarmStorePort,
};

/// 共享连接上的 Swarm 端口：只持有会话数据 adapter，不持有第二份连接。
pub(in crate::sessions::remote) struct RemoteSwarmStore {
    data: Arc<RemoteSessionData>,
}

impl RemoteSwarmStore {
    pub(in crate::sessions::remote) fn new(data: Arc<RemoteSessionData>) -> Self {
        Self { data }
    }
}

#[async_trait]
impl SwarmStorePort for RemoteSwarmStore {
    async fn swarm_upsert_session(&self, row: SwarmSessionRow) -> SessionResourceResult<()> {
        let store = self.data.store().await?;
        upsert_session(&store, &row).await
    }

    async fn swarm_get_session(
        &self,
        logical_id: &str,
    ) -> SessionResourceResult<Option<SwarmSessionRow>> {
        let store = self.data.store().await?;
        get_session(&store, logical_id).await
    }

    async fn swarm_update_session(&self, update: SwarmSessionUpdate) -> SessionResourceResult<()> {
        let store = self.data.store().await?;
        update_session(&store, &update).await
    }

    async fn swarm_append_item(&self, row: SwarmItemRow) -> SessionResourceResult<()> {
        let store = self.data.store().await?;
        append_item(&store, &row).await
    }

    async fn swarm_items_after(
        &self,
        logical_id: &str,
        after: u64,
        limit: u32,
    ) -> SessionResourceResult<Vec<SwarmItemRow>> {
        let store = self.data.store().await?;
        items_after(&store, logical_id, after, limit).await
    }

    async fn swarm_items_before(
        &self,
        logical_id: &str,
        before: Option<u64>,
        limit: u32,
    ) -> SessionResourceResult<Vec<SwarmItemRow>> {
        let store = self.data.store().await?;
        items_before(&store, logical_id, before, limit).await
    }

    async fn swarm_last_seq(&self, logical_id: &str) -> SessionResourceResult<u64> {
        let store = self.data.store().await?;
        last_seq(&store, logical_id).await
    }

    async fn swarm_put_input(&self, row: SwarmInputRow) -> SessionResourceResult<SwarmInputPut> {
        let store = self.data.store().await?;
        put_input(&store, &row).await
    }

    async fn swarm_get_input(
        &self,
        logical_id: &str,
        input_id: &str,
    ) -> SessionResourceResult<Option<SwarmInputRow>> {
        let store = self.data.store().await?;
        get_input(&store, logical_id, input_id).await
    }

    async fn swarm_put_binding(&self, row: SwarmBindingRow) -> SessionResourceResult<()> {
        let store = self.data.store().await?;
        put_binding(&store, &row).await
    }

    async fn swarm_get_binding(
        &self,
        logical_env_id: &str,
    ) -> SessionResourceResult<Option<SwarmBindingRow>> {
        let store = self.data.store().await?;
        get_binding(&store, logical_env_id).await
    }
}

// ─── 语句 ─────────────────────────────────────────────────────────────────────

/// 会话登记：插入或按同一身份更新可变字段。///
/// `ON CONFLICT ... WHERE` 只在 `root_session_id` 与 `cwd` 都相同时更新；否则影响 0 行，
/// 由 `upsert_session` 判成身份冲突（与 S3 内存实现的判据一致）。另一个逻辑会话占用同一
/// `root_session_id` 时走唯一键冲突，同样折成 `Conflict`。
const UPSERT_SESSION_SQL: &str = "INSERT INTO swarm_sessions (
    logical_id, root_session_id, actor_key, cwd, state, model, instructions,
    mcp_json, environment_json, created_at, last_activity_at, last_item_seq)
VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
ON CONFLICT(logical_id) DO UPDATE SET
    actor_key = excluded.actor_key,
    state = excluded.state,
    model = excluded.model,
    instructions = excluded.instructions,
    mcp_json = excluded.mcp_json,
    environment_json = excluded.environment_json,
    last_activity_at = excluded.last_activity_at,
    last_item_seq = excluded.last_item_seq
WHERE swarm_sessions.root_session_id = excluded.root_session_id
  AND swarm_sessions.cwd = excluded.cwd";

const SELECT_SESSION_SQL: &str = "SELECT logical_id, root_session_id, actor_key, cwd, state,
    model, instructions, mcp_json, environment_json, created_at, last_activity_at, last_item_seq
    FROM swarm_sessions WHERE logical_id = ?1";

const UPDATE_SESSION_SQL: &str = "UPDATE swarm_sessions
    SET state = ?2, last_item_seq = ?3, last_activity_at = ?4 WHERE logical_id = ?1";

const INSERT_ITEM_SQL: &str = "INSERT INTO swarm_items
    (logical_id, seq, item_id, kind, source, payload_json, created_at, truncated)
    VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)";

const SELECT_ITEMS_AFTER_SQL: &str = "SELECT logical_id, seq, item_id, kind, source,
    payload_json, created_at, truncated FROM swarm_items
    WHERE logical_id = ?1 AND seq > ?2 ORDER BY seq ASC LIMIT ?3";

const SELECT_ITEMS_BEFORE_SQL: &str = "SELECT logical_id, seq, item_id, kind, source,
    payload_json, created_at, truncated FROM swarm_items
    WHERE logical_id = ?1 AND seq < ?2 ORDER BY seq DESC LIMIT ?3";

const SELECT_ITEMS_TAIL_SQL: &str = "SELECT logical_id, seq, item_id, kind, source,
    payload_json, created_at, truncated FROM swarm_items
    WHERE logical_id = ?1 ORDER BY seq DESC LIMIT ?2";

const SELECT_LAST_SEQ_SQL: &str =
    "SELECT COALESCE(MAX(seq), 0) FROM swarm_items WHERE logical_id = ?1";

/// 幂等裁决点：同 `(logical_id, input_id)` 已存在时不影响任何行，随后读回既有收据。
const INSERT_INPUT_SQL: &str = "INSERT INTO swarm_inputs
    (logical_id, input_id, fingerprint, state, created_at)
    VALUES (?1, ?2, ?3, ?4, ?5)
    ON CONFLICT(logical_id, input_id) DO NOTHING";

const SELECT_INPUT_SQL: &str = "SELECT logical_id, input_id, fingerprint, state, created_at
    FROM swarm_inputs WHERE logical_id = ?1 AND input_id = ?2";

const UPSERT_BINDING_SQL: &str = "INSERT INTO swarm_bindings
    (logical_env_id, logical_id, sandbox_id, workspace_path, sandbox_root,
     settings_profile, token_file, last_run_id, last_used_at)
    VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
    ON CONFLICT(logical_env_id) DO UPDATE SET
        logical_id = excluded.logical_id,
        sandbox_id = excluded.sandbox_id,
        workspace_path = excluded.workspace_path,
        sandbox_root = excluded.sandbox_root,
        settings_profile = excluded.settings_profile,
        token_file = excluded.token_file,
        last_run_id = excluded.last_run_id,
        last_used_at = excluded.last_used_at";

const SELECT_BINDING_SQL: &str = "SELECT logical_env_id, logical_id, sandbox_id, workspace_path,
    sandbox_root, settings_profile, token_file, last_run_id, last_used_at
    FROM swarm_bindings WHERE logical_env_id = ?1";

// ─── 行为 ─────────────────────────────────────────────────────────────────────

async fn upsert_session(store: &RemoteStore, row: &SwarmSessionRow) -> SessionResourceResult<()> {
    let counts = store
        .apply_batch(vec![StatementSpec::new(
            UPSERT_SESSION_SQL,
            vec![
                Value::Text(row.logical_id.clone()),
                Value::Text(row.root_session_id.clone()),
                Value::Text(row.actor_key.clone()),
                Value::Text(row.cwd.clone()),
                Value::Text(row.state.clone()),
                optional_param(row.model.clone()),
                optional_param(row.instructions.clone()),
                optional_param(row.mcp_json.clone()),
                Value::Text(row.environment_json.clone()),
                Value::Text(row.created_at.clone()),
                Value::Text(row.last_activity_at.clone()),
                Value::Integer(seq_param(row.last_item_seq)?),
            ],
        )])
        .await?;
    match counts.as_slice() {
        [1] => Ok(()),
        // 影响 0 行：同一 logical_id 的 root_session_id/cwd 与本次不同——身份冲突。
        [0] => Err(conflict(
            "swarm session identity conflict: cwd or rootSessionId differs",
        )),
        _ => Err(unexplained(
            "swarm session upsert did not report a single affected row",
        )),
    }
}

async fn get_session(
    store: &RemoteStore,
    logical_id: &str,
) -> SessionResourceResult<Option<SwarmSessionRow>> {
    let row = store
        .fetch_row(&StatementSpec::new(
            SELECT_SESSION_SQL,
            vec![Value::Text(logical_id.to_owned())],
        ))
        .await?;
    row.as_deref().map(decode_session).transpose()
}

async fn update_session(
    store: &RemoteStore,
    update: &SwarmSessionUpdate,
) -> SessionResourceResult<()> {
    let counts = store
        .apply_batch(vec![StatementSpec::new(
            UPDATE_SESSION_SQL,
            vec![
                Value::Text(update.logical_id.clone()),
                Value::Text(update.state.clone()),
                Value::Integer(seq_param(update.last_item_seq)?),
                Value::Text(update.last_activity_at.clone()),
            ],
        )])
        .await?;
    match counts.as_slice() {
        [1] => Ok(()),
        [0] => Err(not_found("swarm session does not exist")),
        _ => Err(unexplained(
            "swarm session update did not report a single affected row",
        )),
    }
}

async fn append_item(store: &RemoteStore, row: &SwarmItemRow) -> SessionResourceResult<()> {
    let counts = store
        .apply_batch(vec![StatementSpec::new(
            INSERT_ITEM_SQL,
            vec![
                Value::Text(row.logical_id.clone()),
                Value::Integer(seq_param(row.seq)?),
                Value::Text(row.item_id.clone()),
                Value::Text(row.kind.clone()),
                Value::Text(row.source_json.clone()),
                Value::Text(row.payload_json.clone()),
                Value::Text(row.created_at.clone()),
                Value::Integer(i64::from(row.truncated)),
            ],
        )])
        .await?;
    match counts.as_slice() {
        [1] => Ok(()),
        // 普通 INSERT 要么影响 1 行，要么以 `(logical_id, seq)` 主键冲突失败；0 行说明回复不完整。
        // 同一 `item_id` 出现在不同 seq 不是冲突：delta 与 complete 行共用稳定 id。
        _ => Err(unexplained(
            "swarm item append did not report a single affected row",
        )),
    }
}

async fn items_after(
    store: &RemoteStore,
    logical_id: &str,
    after: u64,
    limit: u32,
) -> SessionResourceResult<Vec<SwarmItemRow>> {
    let rows = store
        .fetch_rows(&StatementSpec::new(
            SELECT_ITEMS_AFTER_SQL,
            vec![
                Value::Text(logical_id.to_owned()),
                Value::Integer(seq_param(after)?),
                Value::Integer(i64::from(limit)),
            ],
        ))
        .await?;
    rows.iter().map(|row| decode_item(row)).collect()
}

async fn items_before(
    store: &RemoteStore,
    logical_id: &str,
    before: Option<u64>,
    limit: u32,
) -> SessionResourceResult<Vec<SwarmItemRow>> {
    let spec = match before {
        Some(before) => StatementSpec::new(
            SELECT_ITEMS_BEFORE_SQL,
            vec![
                Value::Text(logical_id.to_owned()),
                Value::Integer(seq_param(before)?),
                Value::Integer(i64::from(limit)),
            ],
        ),
        None => StatementSpec::new(
            SELECT_ITEMS_TAIL_SQL,
            vec![
                Value::Text(logical_id.to_owned()),
                Value::Integer(i64::from(limit)),
            ],
        ),
    };
    let rows = store.fetch_rows(&spec).await?;
    rows.iter().map(|row| decode_item(row)).collect()
}

async fn last_seq(store: &RemoteStore, logical_id: &str) -> SessionResourceResult<u64> {
    let row = store
        .fetch_row(&StatementSpec::new(
            SELECT_LAST_SEQ_SQL,
            vec![Value::Text(logical_id.to_owned())],
        ))
        .await?
        .ok_or_else(|| unexplained("swarm last-seq read returned no row"))?;
    let value = int_at(&row, 0)
        .ok_or_else(|| unreadable("last item seq is not an integer"))?
        .try_into()
        .map_err(|_| unreadable("last item seq is negative"))?;
    Ok(value)
}

async fn put_input(
    store: &RemoteStore,
    row: &SwarmInputRow,
) -> SessionResourceResult<SwarmInputPut> {
    let counts = store
        .apply_batch(vec![StatementSpec::new(
            INSERT_INPUT_SQL,
            vec![
                Value::Text(row.logical_id.clone()),
                Value::Text(row.input_id.clone()),
                Value::Text(row.fingerprint.clone()),
                Value::Text(row.state.clone()),
                Value::Text(row.created_at.clone()),
            ],
        )])
        .await?;
    match counts.as_slice() {
        [1] => Ok(SwarmInputPut::Inserted),
        [0] => {
            let existing = get_input(store, &row.logical_id, &row.input_id)
                .await?
                .ok_or_else(|| unexplained("conflicting input receipt is not readable"))?;
            Ok(SwarmInputPut::Existing(existing))
        }
        _ => Err(unexplained(
            "swarm input insert did not report a single affected row",
        )),
    }
}

async fn get_input(
    store: &RemoteStore,
    logical_id: &str,
    input_id: &str,
) -> SessionResourceResult<Option<SwarmInputRow>> {
    let row = store
        .fetch_row(&StatementSpec::new(
            SELECT_INPUT_SQL,
            vec![
                Value::Text(logical_id.to_owned()),
                Value::Text(input_id.to_owned()),
            ],
        ))
        .await?;
    row.as_deref().map(decode_input).transpose()
}

async fn put_binding(store: &RemoteStore, row: &SwarmBindingRow) -> SessionResourceResult<()> {
    let counts = store
        .apply_batch(vec![StatementSpec::new(
            UPSERT_BINDING_SQL,
            vec![
                Value::Text(row.logical_env_id.clone()),
                Value::Text(row.logical_id.clone()),
                Value::Text(row.sandbox_id.clone()),
                Value::Text(row.workspace_path.clone()),
                Value::Text(row.sandbox_root.clone()),
                optional_param(row.settings_profile.clone()),
                optional_param(row.token_file.clone()),
                optional_param(row.last_run_id.clone()),
                Value::Text(row.last_used_at.clone()),
            ],
        )])
        .await?;
    match counts.as_slice() {
        [1] => Ok(()),
        _ => Err(unexplained(
            "swarm binding upsert did not report a single affected row",
        )),
    }
}

async fn get_binding(
    store: &RemoteStore,
    logical_env_id: &str,
) -> SessionResourceResult<Option<SwarmBindingRow>> {
    let row = store
        .fetch_row(&StatementSpec::new(
            SELECT_BINDING_SQL,
            vec![Value::Text(logical_env_id.to_owned())],
        ))
        .await?;
    row.as_deref().map(decode_binding).transpose()
}

// ─── 编解码 ───────────────────────────────────────────────────────────────────

fn decode_session(values: &[Value]) -> SessionResourceResult<SwarmSessionRow> {
    Ok(SwarmSessionRow {
        logical_id: text_field(values, 0, "logical id")?,
        root_session_id: text_field(values, 1, "root session id")?,
        actor_key: text_field(values, 2, "actor key")?,
        cwd: text_field(values, 3, "cwd")?,
        state: text_field(values, 4, "state")?,
        model: optional_text_field(values, 5, "model")?,
        instructions: optional_text_field(values, 6, "instructions")?,
        mcp_json: optional_text_field(values, 7, "mcp")?,
        environment_json: text_field(values, 8, "environment")?,
        created_at: text_field(values, 9, "created at")?,
        last_activity_at: text_field(values, 10, "last activity at")?,
        last_item_seq: seq_field(values, 11, "last item seq")?,
    })
}

fn decode_item(values: &[Value]) -> SessionResourceResult<SwarmItemRow> {
    Ok(SwarmItemRow {
        logical_id: text_field(values, 0, "logical id")?,
        seq: seq_field(values, 1, "seq")?,
        item_id: text_field(values, 2, "item id")?,
        kind: text_field(values, 3, "kind")?,
        source_json: text_field(values, 4, "source")?,
        payload_json: text_field(values, 5, "payload")?,
        created_at: text_field(values, 6, "created at")?,
        truncated: int_at(values, 7)
            .ok_or_else(|| unreadable("item truncated flag is not an integer"))?
            != 0,
    })
}

fn decode_input(values: &[Value]) -> SessionResourceResult<SwarmInputRow> {
    Ok(SwarmInputRow {
        logical_id: text_field(values, 0, "logical id")?,
        input_id: text_field(values, 1, "input id")?,
        fingerprint: text_field(values, 2, "fingerprint")?,
        state: text_field(values, 3, "state")?,
        created_at: text_field(values, 4, "created at")?,
    })
}

fn decode_binding(values: &[Value]) -> SessionResourceResult<SwarmBindingRow> {
    Ok(SwarmBindingRow {
        logical_env_id: text_field(values, 0, "logical environment id")?,
        logical_id: text_field(values, 1, "logical id")?,
        sandbox_id: text_field(values, 2, "sandbox id")?,
        workspace_path: text_field(values, 3, "workspace path")?,
        sandbox_root: text_field(values, 4, "sandbox root")?,
        settings_profile: optional_text_field(values, 5, "settings profile")?,
        token_file: optional_text_field(values, 6, "token file")?,
        last_run_id: optional_text_field(values, 7, "last run id")?,
        last_used_at: text_field(values, 8, "last used at")?,
    })
}

fn text_field(values: &[Value], index: usize, what: &'static str) -> SessionResourceResult<String> {
    text_at(values, index)
        .map(str::to_owned)
        .ok_or_else(|| unreadable(&format!("swarm row {what} is not text")))
}

fn optional_text_field(
    values: &[Value],
    index: usize,
    what: &'static str,
) -> SessionResourceResult<Option<String>> {
    match values.get(index) {
        Some(Value::Null) => Ok(None),
        Some(Value::Text(text)) => Ok(Some(text.clone())),
        _ => Err(unreadable(&format!("swarm row {what} is not text"))),
    }
}

fn seq_field(values: &[Value], index: usize, what: &'static str) -> SessionResourceResult<u64> {
    int_at(values, index)
        .ok_or_else(|| unreadable(&format!("swarm row {what} is not an integer")))?
        .try_into()
        .map_err(|_| unreadable(&format!("swarm row {what} is negative")))
}

fn seq_param(value: u64) -> SessionResourceResult<i64> {
    i64::try_from(value).map_err(|_| {
        SessionResourceError::new(SessionResourceErrorKind::InvalidInput {
            detail: "swarm seq exceeds the store's integer range".to_owned(),
        })
    })
}

fn optional_param(value: Option<String>) -> Value {
    value.map_or(Value::Null, Value::Text)
}

fn conflict(detail: &str) -> SessionResourceError {
    SessionResourceError::conflict(detail.to_owned())
}

fn not_found(_detail: &str) -> SessionResourceError {
    SessionResourceError::new(SessionResourceErrorKind::NotFound)
}

/// 存储行读出来但无法解释：不猜内容，也不把它当成「没有这一行」。
fn unreadable(detail: &str) -> SessionResourceError {
    SessionResourceError::new(SessionResourceErrorKind::Corrupt {
        detail: detail.to_owned(),
    })
}

/// 回复/结果不符合语句语义（影响行数异常、读回缺行）：按不完整处理，不静默成功。
fn unexplained(detail: &str) -> SessionResourceError {
    SessionResourceError::new(SessionResourceErrorKind::Internal {
        detail: detail.to_owned(),
    })
}
