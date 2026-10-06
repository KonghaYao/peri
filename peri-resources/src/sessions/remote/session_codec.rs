//! 远程行值 ↔ 领域值的编解码：纯函数，可离线断言。
//!
//! 三条规则：
//!
//! - **只做形状转换**，不改写领域语义：payload 走 `peri_acp_types::store` 的 JSON envelope
//!   （`serialize_persisted_payload` / `deserialize_persisted_payload`）、继承区走
//!   `InheritedContext::to_json/from_json`，远端不另写一份编码格式。
//! - **读不出来就是损坏**：形状不符（缺列、负数计数、非法枚举、时间戳不可解析、行内 ID 与
//!   主键不一致）一律 `Corrupt`，不猜、不用默认值顶替，也不静默丢行。
//! - **写之前先定型**：不可序列化的 payload、越界的 flags 引用在发请求前拒绝。

use std::path::PathBuf;
use std::str::FromStr;

use chrono::{DateTime, Utc};
use peri_acp_types::session_resources::{
    SessionResourceError, SessionResourceErrorKind, SessionResourceResult,
};
use peri_acp_types::store::{
    deserialize_persisted_payload, InheritedContext, MessageFlags, PersistedPayload,
};
use peri_acp_types::thread::{AgentStatus, CancelPolicy, ThreadMeta};
use peri_acp_types::workspace::{ProjectId, SessionBinding, WorkspaceId};
use turso_serverless::Value;

use super::session_sql::{
    META_AGENT_STATUS, META_CANCEL_POLICY, META_CONFIG, META_CONTENT_SIZE, META_CREATED_AT,
    META_CWD, META_HIDDEN, META_ID, META_MESSAGE_COUNT, META_PARENT, META_SNAPSHOT_AT, META_TITLE,
    META_UPDATED_AT,
};
use super::sql::{int_at, text_at};
use crate::sessions::canonical;

// ─── 写侧编码 ─────────────────────────────────────────────────────────────────

pub(super) fn int_value(value: i64) -> Value {
    Value::Integer(value)
}

/// `Some` 写文本、`None` 写 NULL（NULL 在这套 schema 里是「没有这个事实」，不是空字符串）。
pub(super) fn optional_text(value: Option<&str>) -> Value {
    match value {
        Some(text) => Value::Text(text.to_owned()),
        None => Value::Null,
    }
}

/// 一条历史行的插入参数：列顺序即 `messages` 的 canonical 列序
/// （`message_id, thread_id, role, content, truncated, excluded, projection`）。
///
/// `role` 走 [`canonical::payload_role`]：两端写同一列时用同一份领域派生，不各写一份。
pub(super) fn payload_params(
    thread_id: &str,
    payload: &PersistedPayload,
    flags: Option<&MessageFlags>,
    reference: &peri_acp_types::session_resources::work::PayloadRef,
) -> SessionResourceResult<Vec<Value>> {
    let flags = flags.cloned().unwrap_or_default();
    let projection = flags
        .projection
        .as_ref()
        .map(serde_json::to_string)
        .transpose()
        .map_err(|_| corrupt("message projection is not serializable"))?;
    Ok(vec![
        Value::Text(payload.id().as_uuid().to_string()),
        Value::Text(thread_id.to_owned()),
        Value::Text(canonical::payload_role(payload).to_owned()),
        Value::Text(crate::sessions::work::encode(reference)?),
        int_value(i64::from(flags.truncated)),
        int_value(i64::from(flags.excluded)),
        optional_text(projection.as_deref()),
    ])
}

/// 继承区 JSON：序列化后立即按同一套规则读回，引用边界不成立就不落库。
pub(super) fn inherited_json(context: &InheritedContext) -> SessionResourceResult<String> {
    let json = context
        .to_json()
        .map_err(|_| corrupt("inherited context is not serializable"))?;
    InheritedContext::from_json(&json)
        .map_err(|_| corrupt("inherited context has invalid message references"))?;
    Ok(json)
}

// ─── 读侧解码 ─────────────────────────────────────────────────────────────────

/// 单条会话 metadata（事实行或列表行的前 [`super::session_sql::META_COLUMN_COUNT`] 列）。
pub(super) fn decode_meta(values: &[Value]) -> SessionResourceResult<ThreadMeta> {
    let id = text_field(values, META_ID, "session id")?;
    let message_count = int_field(values, META_MESSAGE_COUNT, "message_count")?;
    let content_size = int_field(values, META_CONTENT_SIZE, "content_size")?;
    let cancel_policy = text_field(values, META_CANCEL_POLICY, "cancel_policy")?;
    let agent_status = text_field(values, META_AGENT_STATUS, "agent_status")?;
    Ok(ThreadMeta {
        id,
        title: optional_text_field(values, META_TITLE),
        cwd: text_field(values, META_CWD, "cwd")?,
        created_at: timestamp_field(values, META_CREATED_AT, "created_at")?,
        updated_at: timestamp_field(values, META_UPDATED_AT, "updated_at")?,
        message_count: usize::try_from(message_count)
            .map_err(|_| corrupt("message_count is negative"))?,
        content_size: u64::try_from(content_size)
            .map_err(|_| corrupt("content_size is negative"))?,
        parent_thread_id: optional_text_field(values, META_PARENT),
        snapshot_at_message_id: optional_text_field(values, META_SNAPSHOT_AT),
        hidden: bool_field(values, META_HIDDEN, "hidden")?,
        // 关键约束：列值必须经 FromStr 解析为强类型枚举；非法值不静默 fallback。
        cancel_policy: CancelPolicy::from_str(&cancel_policy)
            .map_err(|_| corrupt("cancel_policy is not a known value"))?,
        config: optional_text_field(values, META_CONFIG),
        // 远端不保存物化缓存：这里没有第二份真相可返回。
        agent_status: AgentStatus::from_str(&agent_status)
            .map_err(|_| corrupt("agent_status is not a known value"))?,
    })
}

/// 绑定列：四列全 NULL = 无绑定行；部分 NULL 或形状不可解释 = 损坏（不猜「有一半绑定」）。
///
/// 版本列是 INTEGER，其余三列是 TEXT：类型不符即形状不符。
pub(super) fn decode_binding(
    values: &[Value],
    base: usize,
) -> SessionResourceResult<Option<SessionBinding>> {
    let present = (0..4).any(|offset| {
        values
            .get(base + offset)
            .is_some_and(|value| *value != Value::Null)
    });
    if !present {
        return Ok(None);
    }
    let version = int_at(values, base)
        .and_then(|value| u16::try_from(value).ok())
        .filter(|value| *value > 0)
        .ok_or_else(|| corrupt("session binding version is not a positive integer"))?;
    let project =
        text_at(values, base + 1).ok_or_else(|| corrupt("session binding row is incomplete"))?;
    let workspace =
        text_at(values, base + 2).ok_or_else(|| corrupt("session binding row is incomplete"))?;
    let relative =
        text_at(values, base + 3).ok_or_else(|| corrupt("session binding row is incomplete"))?;
    let project = ProjectId::from_str(project)
        .map_err(|_| corrupt("session binding project id is unreadable"))?;
    let workspace = WorkspaceId::from_str(workspace)
        .map_err(|_| corrupt("session binding workspace id is unreadable"))?;
    let relative_cwd = PathBuf::from(relative);
    if relative_cwd.is_absolute() {
        return Err(corrupt("session binding cwd is not relative"));
    }
    Ok(Some(SessionBinding {
        schema_version: version,
        // 不可变绑定的协议字段恒为 1；它不是持久化事实，因此不从列里读。
        revision: 1,
        project_id: project,
        workspace_id: workspace,
        cwd_relative_to_workspace: relative_cwd,
    }))
}

/// 自有 payload 行：`message_id, content, truncated, excluded, projection`。
pub(super) fn decode_message_row(values: &[Value]) -> SessionResourceResult<PersistedPayload> {
    let row_id = text_field(values, 0, "message id")?;
    let reference: peri_acp_types::session_resources::work::PayloadRef =
        crate::sessions::work::decode(&text_field(values, 1, "message content reference")?)?;
    let columns = values
        .get(6..10)
        .ok_or_else(|| corrupt("message payload projection is missing"))?;
    let batches = super::session_work::rows(vec![vec![columns.to_vec()]])?;
    let evidence = crate::sessions::work_store::payload::decode_evidence(&reference, &batches[0])?;
    let content = std::str::from_utf8(&evidence.bytes)
        .map_err(|_| corrupt("message payload encoding is invalid"))?;
    let payload = decode_payload_text(content, &row_id)?;
    if text_field(values, 5, "message role")? != canonical::payload_role(&payload) {
        return Err(corrupt("message payload role conflicts"));
    }
    Ok(payload)
}

/// 历史行里的 flags 部分：`message_id, content, truncated, excluded, projection`。
pub(super) fn decode_message_flags_row(values: &[Value]) -> SessionResourceResult<MessageFlags> {
    decode_flags(values, 2, 3, 4)
}

/// 单条 payload 文本 → 领域值；行内 ID 必须与主键一致（与本地读取同一复核）。
pub(super) fn decode_payload_text(
    content: &str,
    row_id: &str,
) -> SessionResourceResult<PersistedPayload> {
    let payload = deserialize_persisted_payload(content)
        .map_err(|_| corrupt("history entry is not a readable persisted payload"))?;
    if payload.id().as_uuid().to_string() != row_id {
        return Err(corrupt(
            "persisted payload message id does not match its row",
        ));
    }
    Ok(payload)
}

/// 继承区 JSON → 领域值。
pub(super) fn decode_inherited(text: Option<&str>) -> SessionResourceResult<InheritedContext> {
    match text {
        Some(json) => InheritedContext::from_json(json)
            .map_err(|_| corrupt("inherited context is not readable")),
        None => Ok(InheritedContext::default()),
    }
}

/// flags 是否为默认值（派生视图只返回非默认标记）。
pub(super) fn flags_are_default(flags: &MessageFlags) -> bool {
    !flags.truncated && !flags.excluded && flags.projection.is_none()
}

fn decode_flags(
    values: &[Value],
    truncated: usize,
    excluded: usize,
    projection: usize,
) -> SessionResourceResult<MessageFlags> {
    let projection = match text_at(values, projection) {
        Some(json) => Some(
            serde_json::from_str(json)
                .map_err(|_| corrupt("message projection is not a readable directive"))?,
        ),
        None => None,
    };
    Ok(MessageFlags {
        truncated: bool_field(values, truncated, "truncated")?,
        excluded: bool_field(values, excluded, "excluded")?,
        projection,
    })
}

pub(super) fn corrupt(detail: &str) -> SessionResourceError {
    SessionResourceError::new(SessionResourceErrorKind::Corrupt {
        detail: detail.to_owned(),
    })
}

fn text_field(values: &[Value], index: usize, what: &str) -> SessionResourceResult<String> {
    text_at(values, index)
        .map(str::to_owned)
        .ok_or_else(|| corrupt(&format!("{what} is missing or not text")))
}

fn optional_text_field(values: &[Value], index: usize) -> Option<String> {
    text_at(values, index).map(str::to_owned)
}

fn int_field(values: &[Value], index: usize, what: &str) -> SessionResourceResult<i64> {
    int_at(values, index).ok_or_else(|| corrupt(&format!("{what} is missing or not an integer")))
}

fn bool_field(values: &[Value], index: usize, what: &str) -> SessionResourceResult<bool> {
    match int_at(values, index) {
        Some(0) => Ok(false),
        Some(1) => Ok(true),
        _ => Err(corrupt(&format!("{what} is not a boolean flag"))),
    }
}

fn timestamp_field(
    values: &[Value],
    index: usize,
    what: &str,
) -> SessionResourceResult<DateTime<Utc>> {
    text_at(values, index)
        .and_then(|text| text.parse::<DateTime<Utc>>().ok())
        .ok_or_else(|| corrupt(&format!("{what} is not an RFC3339 timestamp")))
}
