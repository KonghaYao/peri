//! Thread 列投影、强类型元数据解码和消息展示字段。

pub(crate) use crate::sessions::canonical::extract_title;
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use peri_acp_types::thread::{AgentStatus, CancelPolicy, ThreadMeta};
use std::str::FromStr;

pub(super) const THREAD_META_COLUMNS: &str = "t.id, t.title, t.cwd, t.created_at, t.updated_at, t.message_count,
    (SELECT COALESCE(SUM(json_extract(m.content_ref,'$.byteLength')), 0) FROM messages m WHERE m.thread_id = t.id) as content_size,
    t.parent_thread_id, t.snapshot_at_message_id, t.hidden, t.cancel_policy, t.config, t.agent_status";

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
) -> Result<ThreadMeta> {
    let message_count = usize::try_from(message_count).context("message_count is negative")?;
    let content_size = u64::try_from(content_size).context("content_size is negative")?;
    // 关键约束：DB 字符串必须经 FromStr 解析为强类型枚举；非法值不静默 fallback
    let cancel_policy = CancelPolicy::from_str(&cancel_policy)
        .with_context(|| format!("解析 cancel_policy 失败（thread_id={}）", id))?;
    let agent_status = AgentStatus::from_str(&agent_status)
        .with_context(|| format!("解析 agent_status 失败（thread_id={}）", id))?;
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
    })
}
