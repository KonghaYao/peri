//! ACP session/load history replay via `session/update` notifications.
//!
//! Per ACP v1 spec, `session/load` MUST replay the entire conversation to the
//! client via `session/update` notifications (`user_message_chunk` +
//! `agent_message_chunk`) BEFORE responding to the request.
//!
//! Tool interactions (`ToolUse` / `ToolResult`) are replayed via standard
//! `tool_call` / `tool_call_update` events so the TUI can render tool cards.
//!
//! ## 交付来源
//!
//! 标准 ACP 连接从存储读取 **canonical 完整历史**（[`replay_canonical_history_streamed`]），
//! 逐段流式投影成标准 `session/update`，不携带任何 Peri 私有历史字段。
//!
//! 历史 V2 连接的首屏由 `_meta["peri.history"]` 交付，本模块只负责普通 ACP 客户端的
//! 完整回放；两者共用 [`project_payload_updates`]，不允许在别处再写第二种投影。
//!
//! Reference: <https://agentclientprotocol.com/protocol/v1/session-setup#loading-a-session>

use agent_client_protocol_schema::v1::{
    ContentBlock, ContentChunk, SessionId, SessionNotification, SessionUpdate, TextContent,
    ToolCall, ToolCallUpdate,
};
use peri_acp_types::history_page::HistoryForwardCursor;
use peri_acp_types::messages::{
    BaseMessage, ContentBlock as PeriContentBlock, MessageContent as PeriMessageContent, MessageId,
};
use peri_acp_types::session_resources::SessionResourceErrorKind;
use peri_acp_types::store::PersistedPayload;
use peri_acp_types::system_reminder::{reminder_egress_allowed, ReminderAudience, SystemReminder};
use peri_acp_types::thread::ThreadId;
use peri_acp_types::tools::{ToolOutcome, ToolOutput};
use peri_acp_types::PeriCaps;
use peri_controller::Controller;

use crate::event::tool_projection::{project_tool_completion, project_tool_start};

/// 一条持久化 payload 的一个展示子项。
///
/// 与回放路径同源：`Session` 就是回放要发的那条 `session/update`，`SystemReminder`
/// 就是回放要发的结构化 reminder 事件（两者的受众规则也共用，见
/// [`reminder_visible_to_client`]）。
#[derive(Clone, Debug)]
pub enum ProjectedUpdate {
    /// `session/update` 载荷（user/agent chunk、thought chunk、tool call 及其更新）。
    Session(Box<SessionUpdate>),
    /// `peri/unstable_event` 的结构化 system-reminder 载荷（legacy compact 提示）。
    SystemReminder(SystemReminder),
}

impl ProjectedUpdate {
    fn session(update: SessionUpdate) -> Self {
        Self::Session(Box::new(update))
    }
}

/// 条目身份（`entryId`）的**唯一**格式：`{会话 id}:{payload id}`。
///
/// 回放出口与历史页出口共用它：同一个 payload 在两条出口上必须是同一个字符串，客户端的
/// 去重与区间合并才成立（REQ-H01-3、验收 A20 的合并契约）。
pub fn entry_id(session_id: &str, message_id: MessageId) -> String {
    format!("{session_id}:{}", message_id.as_uuid())
}

/// 把一条持久化 payload 投影成客户端可见的更新序列（回放与 V2 共用的唯一实现）。
///
/// 不可展示的条目（System 消息）不产生子项：过滤在这里完成，调用方不需要再判一次。
/// 受众规则**不**在这里套用：回放出口与 V2 出口各自在发送/装配时用
/// [`reminder_visible_to_client`] 过滤（同一份判据）。
pub fn project_payload_updates(
    payload: &PersistedPayload,
    caps: &PeriCaps,
) -> Vec<ProjectedUpdate> {
    match payload {
        PersistedPayload::Message(message) => project_message_updates(message, caps),
        PersistedPayload::SystemReminder { reminder, .. } => {
            vec![ProjectedUpdate::SystemReminder(
                reminder.as_reminder().clone(),
            )]
        }
    }
}

/// H8 受众规则：该 reminder 是否可以出现在客户端出口。
///
/// 回放发送与 V2 装配共用这一份判据；未声明客户端受众的 reminder（例如 Model-only
/// recall）不得因为回放或翻页而出现在 wire 上。
pub fn reminder_visible_to_client(reminder: &SystemReminder) -> bool {
    reminder_egress_allowed(reminder, ReminderAudience::Tui)
}

pub async fn replay_persisted_session_history(
    session_id: &str,
    history: &[PersistedPayload],
    sender: &dyn ReplaySender,
    caps: &PeriCaps,
) -> Result<(), ReplayError> {
    for payload in history {
        for update in project_payload_updates(payload, caps) {
            send_projected(session_id, update, sender, caps).await?;
        }
    }
    Ok(())
}

/// 一段 canonical 完整回放的读取规模。
///
/// 取值与历史窗口存储读取的两个默认值同量级（50 行 / 512 KiB），保证同一会话的
/// 首屏回放与 V2 窗口读取都不会把内存上限放大。
pub const REPLAY_SEGMENT_ROWS: usize = 50;

/// 一段回放的正文编码字节上限，见 [`REPLAY_SEGMENT_ROWS`]。
pub const REPLAY_SEGMENT_BYTE_BUDGET: usize = 512 * 1024;

/// 标准 ACP 客户端的 canonical 完整回放：**流式**逐段读取并投影。
///
/// ## 为什么不是热态有效集合
///
/// 内存热态只持有**有效集合**：Full Compact 释放掉的原文不在其中。拿它当回放源会让「完整
/// 回放」少掉归档原文（REQ §5.3）。完整历史在存储侧，这里按向前段读交付，核心不必重新持有
/// 整份历史——循环只留当前一段，下一段的读取覆盖它（不先取全量再切段）。
///
/// ## 交付边界
///
/// - **固定 `tail` 上界**：首段读取那一刻的最大存储序键固定本次回放的区间上界；回放期间新
///   落盘的行不进入本次交付，避免与实时通知重复。**已完成的 turn 不会因此丢失**：写屏障先于
///   热态发布（`host::prompt::finish_prompt_turn` 只在 `PromptResult` 建好之后采用
///   `persisted_payloads`，而该结果在屏障完成之后才产生，失败/取消同理），因此进程内热态
///   **永远不领先于** disk prefix——凡是进入热态（因而属于既有交付）的内容必然已在 `tail`
///   之内。晚于 `tail` 落盘的内容属于「本次交付之后的新增」，由实时通知通道负责。
///   已知边界（本切片不改变）：仍在进行中的 turn 已流式发给客户端的正文既不在 `tail` 之内、
///   也不在热态有效集合之内（热态只在 turn 结束时刷新），两条来源在这一点上一致。
/// - **继承区不分页**：首段一次性返回全部继承区 payload（S9a 的段契约），本切片不切分它。
///   这是已知边界，不是遗漏。
/// - 单条超预算的正文独占一段并**完整交付**（完整回放不允许静默丢内容）。
///
/// ## 回退与失败
///
/// - `Unsupported`（后端没有这条读取）：**显式**回退到
///   [`peri_acp_types::session_resources::SessionResources::load_session_history`] 的全量读取，
///   按同一份投影回放，并留下一条可区分的日志。回退只发生在**还没送出任何通知**时：半份段
///   之后再来一次全量回放会把同一段内容交付两遍——那种情况按读失败处理（支持与否是后端属性，
///   正常不会中途改变），宁可让本次 load 可见地失败。
/// - 其他错误（`Corrupt` / `Unavailable` / 锚点消失 / 读失败）：整次 load 失败，错误交给
///   调用方映射成 `-32603`。不得用空段冒充「回放结束」，也不得留下「半份首屏 + 成功响应」。
pub async fn replay_canonical_history_streamed(
    controller: &Controller,
    session_id: &str,
    sender: &dyn ReplaySender,
    caps: &PeriCaps,
) -> Result<(), ReplayError> {
    let thread_id = ThreadId::from(session_id.to_owned());
    let resources = controller.sessions();
    // 首段之后游标由存储铸造：区间上界（`tail`）只在首段那一刻确定。
    let mut cursor: Option<HistoryForwardCursor> = None;
    // 已交付的 payload 条数：判据是「本次回放是否已经交付过东西」，不是条数本身。
    let mut delivered = 0usize;
    loop {
        let segment = match resources
            .load_history_segment(
                &thread_id,
                cursor.as_ref(),
                REPLAY_SEGMENT_ROWS,
                REPLAY_SEGMENT_BYTE_BUDGET,
            )
            .await
        {
            Ok(segment) => segment,
            // 能力缺失 vs「没有更多历史」必须分开：空段是「回放结束」的合法含义，后端没有
            // 这条读取时不能拿它冒充结束。
            Err(error)
                if matches!(error.kind(), SessionResourceErrorKind::Unsupported)
                    && delivered == 0 =>
            {
                tracing::warn!(
                    session_id,
                    "session/load: canonical history segment read is unsupported by this store; \
                     falling back to the full canonical read"
                );
                return replay_canonical_history_fallback(controller, session_id, sender, caps)
                    .await;
            }
            Err(error) => {
                return Err(ReplayError::HistoryReadFailed(format!(
                    "canonical history segment load failed: {error}"
                )))
            }
        };
        for payload in &segment.payloads {
            delivered += 1;
            for update in project_payload_updates(payload, caps) {
                send_projected(session_id, update, sender, caps).await?;
            }
        }
        tracing::debug!(
            session_id,
            rows = segment.payloads.len(),
            has_more = segment.next_cursor.is_some(),
            "session/load: canonical history segment delivered"
        );
        match segment.next_cursor {
            Some(next) => cursor = Some(next),
            // `None` 只表示「没有更多自有行了」，两种失败（不支持 / 读失败）都不走这里。
            None => return Ok(()),
        }
    }
}

/// [`replay_canonical_history_streamed`] 的回退：后端没有向前段读取，改用全量 canonical 读取。
///
/// 与流式路径**同一份投影**，因此两条路的 wire 形状一致，客户端不需要知道走了哪一条。
/// 调用方保证只在「还没交付过任何东西」时进来。
async fn replay_canonical_history_fallback(
    controller: &Controller,
    session_id: &str,
    sender: &dyn ReplaySender,
    caps: &PeriCaps,
) -> Result<(), ReplayError> {
    let history = controller
        .sessions()
        .load_session_history(&ThreadId::from(session_id.to_owned()))
        .await
        .map_err(|error| {
            ReplayError::HistoryReadFailed(format!("canonical history load failed: {error}"))
        })?;
    replay_persisted_session_history(session_id, &history, sender, caps).await
}

/// Replay session history via `session/update` notifications.
///
/// Iterates `history`, converting each `BaseMessage` into one or more
/// `SessionUpdate` variants, then calls `sender` for each notification.
///
/// - `BaseMessage::Human`  → `SessionUpdate::UserMessageChunk`
/// - `BaseMessage::Ai`     → `Reasoning` blocks as `AgentThoughtChunk`,
///   `Text` blocks as `AgentMessageChunk`,
///   `ToolUse` blocks as `ToolCall` (periReplay=true)
/// - `BaseMessage::Tool`   → `ToolResult` blocks as `ToolCallUpdate` (periReplay=true)
/// - Other variants         → silently skipped
pub async fn replay_session_history(
    session_id: &str,
    history: &[BaseMessage],
    sender: &dyn ReplaySender,
    caps: &PeriCaps,
) -> Result<(), ReplayError> {
    for msg in history.iter() {
        for update in project_message_updates(msg, caps) {
            send_projected(session_id, update, sender, caps).await?;
        }
    }
    Ok(())
}

/// 送出一次投影（回放出口）：`session/update` 走 `send`，结构化 reminder 走
/// `send_system_reminder`（受众过滤在实现里，见 [`reminder_visible_to_client`]）。
async fn send_projected(
    session_id: &str,
    update: ProjectedUpdate,
    sender: &dyn ReplaySender,
    caps: &PeriCaps,
) -> Result<(), ReplayError> {
    match update {
        ProjectedUpdate::Session(update) => {
            let notif = SessionNotification::new(SessionId::new(session_id.to_string()), *update);
            sender.send(notif).await
        }
        ProjectedUpdate::SystemReminder(reminder) => {
            sender
                .send_system_reminder(session_id, &reminder, caps)
                .await
        }
    }
}

/// 一条消息的展示投影（回放与 V2 共用的唯一实现）。
pub fn project_message_updates(msg: &BaseMessage, caps: &PeriCaps) -> Vec<ProjectedUpdate> {
    let mut updates = Vec::new();
    if msg.is_system() {
        return updates;
    }
    match msg {
        BaseMessage::Human { content, .. } => {
            let reminders = peri_acp_types::compact_reminder::legacy_compact_reminders(msg);
            if !reminders.is_empty() {
                for reminder in reminders {
                    updates.push(ProjectedUpdate::SystemReminder(reminder));
                }
                return updates;
            }
            updates.push(ProjectedUpdate::session(SessionUpdate::UserMessageChunk(
                replay_chunk(
                    ContentBlock::Text(TextContent::new(extract_text(content))),
                    caps,
                    msg.id(),
                ),
            )));
        }
        BaseMessage::Ai {
            content,
            tool_calls,
            ..
        } => {
            // 收集 ContentBlock::ToolUse 的 id，避免与 tool_calls 重复发射
            let mut emitted_ids: std::collections::HashSet<String> =
                std::collections::HashSet::new();

            let blocks = match content {
                PeriMessageContent::Text(s) => {
                    updates.push(ProjectedUpdate::session(SessionUpdate::AgentMessageChunk(
                        replay_chunk(
                            ContentBlock::Text(TextContent::new(s.clone())),
                            caps,
                            msg.id(),
                        ),
                    )));
                    // 纯文本 AI 消息无 blocks，tool_calls 由下方单独处理
                    for tc in tool_calls {
                        let tool_call = project_tool_start(&tc.id, &tc.name, &tc.arguments);
                        updates.push(ProjectedUpdate::session(SessionUpdate::ToolCall(
                            replay_tool(tool_call, caps),
                        )));
                    }
                    return updates;
                }
                PeriMessageContent::Blocks(blocks) => blocks,
                PeriMessageContent::Raw(_) => return updates,
            };

            for block in blocks {
                match block {
                    PeriContentBlock::Reasoning { text, .. } => {
                        updates.push(ProjectedUpdate::session(SessionUpdate::AgentThoughtChunk(
                            replay_chunk(
                                ContentBlock::Text(TextContent::new(text.clone())),
                                caps,
                                msg.id(),
                            ),
                        )));
                    }
                    PeriContentBlock::Text { text } => {
                        updates.push(ProjectedUpdate::session(SessionUpdate::AgentMessageChunk(
                            replay_chunk(
                                ContentBlock::Text(TextContent::new(text.clone())),
                                caps,
                                msg.id(),
                            ),
                        )));
                    }
                    PeriContentBlock::ToolUse { id, name, input } => {
                        emitted_ids.insert(id.clone());
                        let tc = project_tool_start(id, name, input);
                        updates.push(ProjectedUpdate::session(SessionUpdate::ToolCall(
                            replay_tool(tc, caps),
                        )));
                    }
                    // Image / Document / Unknown → 跳过
                    _ => {}
                }
            }

            // 发射 tool_calls 中未被 ContentBlock::ToolUse 覆盖的条目
            for tc in tool_calls {
                if !emitted_ids.contains(&tc.id) {
                    let tool_call = project_tool_start(&tc.id, &tc.name, &tc.arguments);
                    updates.push(ProjectedUpdate::session(SessionUpdate::ToolCall(
                        replay_tool(tool_call, caps),
                    )));
                }
            }
        }
        BaseMessage::Tool {
            content,
            is_error,
            tool_call_id,
            execution,
            subagent_failure,
            ..
        } => {
            let result_text = ToolOutput {
                text: extract_text(content),
                execution: execution.clone(),
                outcome: ToolOutcome::Unreported,
            }
            .projected_text(None);
            let mut update = project_tool_completion(
                tool_call_id,
                &result_text,
                *is_error,
                subagent_failure.as_ref(),
            );
            // 历史 rawOutput 保持字符串；live adapter 保留 JSON 解析兼容行为。
            update.fields.raw_output = Some(serde_json::Value::String(result_text));
            updates.push(ProjectedUpdate::session(SessionUpdate::ToolCallUpdate(
                replay_tool_update(update, caps),
            )));
        }
        _ => {}
    }
    updates
}

/// 回放出口的 meta 戳：`periReplay` 标记 + 本次 load 代次 + 条目位置戳。
///
/// 回放出口的 meta 标记：`periReplay` 由**当前连接**的 `caps.replay` 门控。
///
/// 未协商的连接收到的是纯 ACP 完整回放，不引入 Peri 私有语义（REQ §5.3）。
fn replay_meta(caps: &PeriCaps) -> Option<serde_json::Map<String, serde_json::Value>> {
    caps.replay.then(|| {
        serde_json::Map::from_iter([("periReplay".to_string(), serde_json::Value::Bool(true))])
    })
}

fn replay_chunk(
    content: ContentBlock,
    caps: &PeriCaps,
    message_id: peri_acp_types::messages::MessageId,
) -> ContentChunk {
    let mut chunk = ContentChunk::new(content);
    // ACP 标准 messageId 语义：replay 时携带消息真实 ID，与流式路径一致
    // （同一消息的 reasoning/text chunk 共享 ID，客户端段边界行为一致）。
    // v1 wire 上的 messageId 是字符串（规范消息 ID 的 UUID 串）。
    chunk.message_id = Some(agent_client_protocol_schema::v1::MessageId::from(
        message_id.as_uuid().to_string(),
    ));
    chunk.meta = replay_meta(caps);
    chunk
}

/// 给 `ToolCall` 打上回放 meta 标记（见 [`replay_meta`]）。
fn replay_tool(mut tc: ToolCall, caps: &PeriCaps) -> ToolCall {
    tc.meta = replay_meta(caps);
    tc
}

/// 给 `ToolCallUpdate` 打上回放 meta 标记（与 [`replay_tool`] 同一份规则）。
fn replay_tool_update(mut tu: ToolCallUpdate, caps: &PeriCaps) -> ToolCallUpdate {
    if let Some(meta) = replay_meta(caps) {
        tu.meta
            .get_or_insert_with(serde_json::Map::new)
            .extend(meta);
    }
    tu
}

/// Extract plain text from a `MessageContent`.
fn extract_text(content: &PeriMessageContent) -> String {
    match content {
        PeriMessageContent::Text(s) => s.clone(),
        PeriMessageContent::Blocks(blocks) => blocks
            .iter()
            .filter_map(|b| match b {
                PeriContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
        PeriMessageContent::Raw(_) => String::new(),
    }
}

/// Abstraction over how to send a `SessionNotification`.
///
/// `send_system_reminder` 是**必需**方法：replay 出口不得继承默认 no-op 冒充
/// 成功。每个 sender 必须显式决定（按 H8 受众规则过滤后）结构化事件或客户端
/// 摘要怎么下发；不承载该通道的实现也必须写出显式理由而不是静默吞掉。
#[async_trait::async_trait]
pub trait ReplaySender: Send + Sync {
    async fn send(&self, notif: SessionNotification) -> Result<(), ReplayError>;

    async fn send_system_reminder(
        &self,
        session_id: &str,
        reminder: &peri_acp_types::system_reminder::SystemReminder,
        caps: &PeriCaps,
    ) -> Result<(), ReplayError>;
}

#[derive(Debug, thiserror::Error)]
pub enum ReplayError {
    #[error("transport send failed: {0}")]
    SendFailed(String),
    /// 回放源读取失败（段读取或 Unsupported 回退的全量读取）：整次 load 必须失败。
    ///
    /// 与 [`Self::SendFailed`] 分开：这里是「要回放的东西没读出来」，不是投递通道故障。
    /// 两者对调用方都是同一种处置（本次 load 以 `-32603` 结束），但日志与排查路径不同。
    #[error("canonical history read failed: {0}")]
    HistoryReadFailed(String),
}

#[cfg(test)]
#[path = "session_replay_test.rs"]
mod tests;
