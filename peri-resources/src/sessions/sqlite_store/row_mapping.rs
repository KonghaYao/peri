//! Thread 列投影、强类型元数据解码和消息展示字段。

pub(crate) use crate::sessions::canonical::extract_title;
pub(in crate::sessions) use crate::sessions::canonical::role_of;
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use peri_acp_types::thread::{AgentStatus, CancelPolicy, ThreadMeta, WorkState};
use std::str::FromStr;

/// 元数据行投影；字段顺序与 [`ThreadRow`] 一致。
///
/// `content_size` 没有落列（`threads` 表里没有这一列），也没有生产消费者：此前的
/// `SUM(LENGTH(content))` 相关子查询每次读取都要聚合整份历史，恢复热路径与列表都随历史
/// 增长而变慢。这里保留列位（别名 + 常量 0）以维持行形状与解码下标，**不做聚合**；
/// 需要真实字节数时应另设按需读取，不把全历史聚合塞回这条投影。
pub(super) const THREAD_META_COLUMNS: &str = "t.id, t.title, t.cwd, t.created_at, t.updated_at, t.message_count,
    0 as content_size,
    t.parent_thread_id, t.snapshot_at_message_id, t.hidden, t.cancel_policy, t.config, t.agent_status,
    t.context_usage_tokens, t.work_state";

/// 旧代际（schema ≤ 13）的同一投影：`threads` 还没有两列工作快照，用本代默认值占位。
///
/// 只读打开不迁移，必须能读旧库；列数与顺序与本代一致，[`ThreadRow`] 的解码下标因此不变。
pub(super) const THREAD_META_COLUMNS_LEGACY: &str = "t.id, t.title, t.cwd, t.created_at, t.updated_at, t.message_count,
    0 as content_size,
    t.parent_thread_id, t.snapshot_at_message_id, t.hidden, t.cancel_policy, t.config, t.agent_status,
    NULL as context_usage_tokens, 'idle' as work_state";

/// 恢复范围查询的工作快照投影与候选过滤。
///
/// 与 metadata 投影共用同一个 legacy 开关：两处读同一列，不能一处有、一处无。
pub(super) const RECOVERY_SCOPE_PROJECTION: &str = "t.context_usage_tokens, t.work_state";
pub(super) const RECOVERY_SCOPE_FILTER: &str = "t.work_state = 'working'";

/// 旧代际的恢复范围片段：工作意图读作 `idle`，过滤恒假 ⇒ 自然零候选。
///
/// 只读旧库必须零候选而不是报错，也不能把「读不到工作意图」当成 `working`。
pub(super) const RECOVERY_SCOPE_PROJECTION_LEGACY: &str =
    "NULL as context_usage_tokens, 'idle' as work_state";
pub(super) const RECOVERY_SCOPE_FILTER_LEGACY: &str = "0";

/// 元数据行形状；字段顺序与上方列常量一致。
pub(super) type ThreadRow = (
    String,
    Option<String>,
    String,
    String,
    String,
    i64,
    i64,
    Option<String>,
    Option<String>,
    bool,
    String,
    Option<String>,
    String,
    Option<i64>,
    String,
);

// ── 辅助函数 ──────────────────────────────────────────────────────────────────

// meta_from_row 从行列提取 8+ 字段；拆分参数列表不具可读性优势，此处抑制 `too_many_arguments`
#[allow(clippy::too_many_arguments)]
pub(super) fn meta_from_row(
    id: String,
    title: Option<String>,
    cwd: String,
    created_at: String,
    updated_at: String,
    message_count: i64,
    content_size: i64,
    parent_thread_id: Option<String>,
    snapshot_at_message_id: Option<String>,
    hidden: bool,
    cancel_policy: String,
    config: Option<String>,
    agent_status: String,
    context_usage_tokens: Option<i64>,
    work_state: String,
) -> Result<ThreadMeta> {
    let message_count = usize::try_from(message_count).context("message_count is negative")?;
    let content_size = u64::try_from(content_size).context("content_size is negative")?;
    let context_usage_tokens = context_usage_tokens
        .map(|tokens| u64::try_from(tokens).context("context_usage_tokens is negative"))
        .transpose()?;
    // 关键约束：DB 字符串必须经 FromStr 解析为强类型枚举；非法值不静默 fallback
    let cancel_policy = CancelPolicy::from_str(&cancel_policy)
        .with_context(|| format!("解析 cancel_policy 失败（thread_id={}）", id))?;
    let agent_status = AgentStatus::from_str(&agent_status)
        .with_context(|| format!("解析 agent_status 失败（thread_id={}）", id))?;
    let work_state = WorkState::from_str(&work_state)
        .with_context(|| format!("解析 work_state 失败（thread_id={}）", id))?;
    Ok(ThreadMeta {
        id,
        title,
        cwd,
        created_at: created_at.parse::<DateTime<Utc>>()?,
        updated_at: updated_at.parse::<DateTime<Utc>>()?,
        message_count,
        content_size,
        parent_thread_id,
        snapshot_at_message_id,
        hidden,
        cancel_policy,
        config,
        agent_status,
        work_state,
        context_usage_tokens,
    })
}
