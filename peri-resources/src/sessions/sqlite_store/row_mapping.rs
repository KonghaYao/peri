//! Thread 列投影、强类型元数据解码和消息展示字段。

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use peri_acp_types::{
    messages::BaseMessage,
    thread::{AgentStatus, CancelPolicy, ThreadMeta},
};
use std::str::FromStr;

/// SELECT 所有 thread 列的统一常量（含 cached_context，仅 load_context 等需要完整数据的场景使用）
pub(super) const THREAD_COLUMNS: &str = "t.id, t.title, t.cwd, t.created_at, t.updated_at, t.message_count,
    (SELECT COALESCE(SUM(LENGTH(m.content)), 0) FROM messages m WHERE m.thread_id = t.id) as content_size,
    t.parent_thread_id, t.snapshot_at_message_id, t.hidden, t.cancel_policy, t.config, t.cached_context, t.agent_status";

/// SELECT thread 元数据列（不含 cached_context），用于 list_threads 等列表场景。
/// cached_context 包含完整消息历史 JSON，加载所有线程时会占用大量内存（~1MB/线程）。
pub(super) const THREAD_META_COLUMNS: &str = "t.id, t.title, t.cwd, t.created_at, t.updated_at, t.message_count,
    (SELECT COALESCE(SUM(LENGTH(m.content)), 0) FROM messages m WHERE m.thread_id = t.id) as content_size,
    t.parent_thread_id, t.snapshot_at_message_id, t.hidden, t.cancel_policy, t.config, NULL as cached_context, t.agent_status";

/// 两种 thread SELECT 投影共享的行形状；字段顺序与上方列常量一致。
/// 列表投影用 NULL 填充 cached_context，保留相同的可空字段位置。
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
    Option<String>,
    String,
);

// ── 辅助函数 ──────────────────────────────────────────────────────────────────

pub(super) fn role_of(msg: &BaseMessage) -> &'static str {
    match msg {
        BaseMessage::Human { .. } => "user",
        BaseMessage::Ai { .. } => "assistant",
        BaseMessage::System { .. } => "system",
        BaseMessage::Tool { .. } => "tool",
    }
}

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
    cached_context: Option<String>,
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
        cached_context,
        agent_status,
    })
}

/// 从消息列表中提取标题（取第一条 Human 消息的前 50 字符）
pub(super) fn extract_title(msgs: &[BaseMessage]) -> Option<String> {
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
