//! ACP session/load history replay via `session/update` notifications.
//!
//! Per ACP v1 spec, `session/load` MUST replay the entire conversation to the
//! client via `session/update` notifications (`user_message_chunk` +
//! `agent_message_chunk`) BEFORE responding to the request.
//!
//! Tool interactions (`ToolUse` / `ToolResult`) are replayed via standard
//! `tool_call` / `tool_call_update` events so the TUI can render tool cards.
//!
//! ## 投影只有一份实现
//!
//! [`project_payload_updates`] 把**一条**持久化 payload 投影成客户端可见的更新序列
//! （一个持久信封 = 若干子项：reasoning / text / tool / 结构化 reminder）。回放路径
//! 把同一序列逐条送出，历史页路径把同一序列放进响应——两条出口共用这一份实现，
//! 不允许在别处再写第三种投影。
//!
//! ## 两条交付来源，判据是连接级事实
//!
//! | 连接 | 交付来源 | 出口 |
//! | --- | --- | --- |
//! | 协商过 `peri.historyPagination` | 内存热态的**有效集合**（[`replay_persisted_session_history`]） | 回放 + 条目身份 / 位置戳（`entryId`/`position`）+ 代次 |
//! | 未协商 | 存储里的 **canonical 完整历史**（[`replay_canonical_history_streamed`]） | 逐段流式投影成标准 `session/update`，不带任何分页私有字段 |
//!
//! 未协商路径**不得**改用热态有效集合：那会丢掉 Full Compact 归档的原文，完整回放就不再
//! 完整（REQ §5.3）。判据必须来自**当前连接**的协商事实，不得回读进程 / 会话级 caps 注册表。
//!
//! Reference: <https://agentclientprotocol.com/protocol/v1/session-setup#loading-a-session>

use std::collections::HashMap;

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

/// 回放出口的位置戳来源：本次 load 的代次 + 本会话自有条目的存储序位置。
///
/// 条目身份 / 位置戳（`periEntryId` / `periPosition`）只对**当前连接**协商过分页能力的
/// 初始 load 出现（REQ §5.3）：未协商的连接拿到的仍是标准完整回放，一个 Peri 私有字段都
/// 不引入。位置表来自存储的 `load_entry_positions`（只读条目 id 与存储序键，不读正文），
/// 因此回放不必为了打戳再读一遍历史。
///
/// 两个构造口不是可选项而是**准入事实的两种取值**：`new` = 本连接协商过分页（打戳），
/// [`Self::without_entry_marks`] = 没协商（只带代次，不打戳）。调用方不得为了省一次读取
/// 而用空位置表冒充「协商过」——那会漏发位置戳，客户端按 `null` 处理会把自有条目排到
/// 继承条目之间。
#[derive(Clone, Copy, Debug)]
pub struct ReplayMarks<'a> {
    generation: &'a LoadGeneration,
    session_id: &'a str,
    /// `None` = 未协商分页：不打条目身份 / 位置戳。
    positions: Option<&'a HashMap<MessageId, u64>>,
}

impl<'a> ReplayMarks<'a> {
    /// 协商过分页的连接：位置表是存储给出的自有有效条目序键。
    pub fn new(
        generation: &'a LoadGeneration,
        session_id: &'a str,
        positions: &'a HashMap<MessageId, u64>,
    ) -> Self {
        Self {
            generation,
            session_id,
            positions: Some(positions),
        }
    }

    /// 未协商分页的连接：回放仍带本次 load 的代次，但不带任何条目身份 / 位置戳。
    pub fn without_entry_marks(generation: &'a LoadGeneration, session_id: &'a str) -> Self {
        Self {
            generation,
            session_id,
            positions: None,
        }
    }

    /// 一条 payload 的戳。继承区条目（本线程没有行）不在位置表里：它们拿得到身份，
    /// 拿不到位置——位置只在同一个存储序内可比。
    fn entry(&self, message_id: MessageId) -> EntryMarks<'a> {
        EntryMarks {
            generation: self.generation,
            session_id: self.session_id,
            message_id,
            position: self
                .positions
                .and_then(|positions| positions.get(&message_id).copied()),
            merge: self.positions.is_some(),
        }
    }
}

/// 一个条目（一个持久 payload）在回放出口的身份与位置戳。
///
/// 它是「本次 load 的代次」与「条目身份 / 存储序位置」的合体，因为三者必须**同进同出**：
/// 客户端按代次把通知与那次响应配对，按身份去重、按位置把页并进稀疏首屏。
#[derive(Clone, Copy, Debug)]
pub struct EntryMarks<'a> {
    generation: &'a LoadGeneration,
    session_id: &'a str,
    message_id: MessageId,
    position: Option<u64>,
    /// 本连接是否协商过分页：`false` 时 [`write_entry_marks`] 一个字段都不写。
    merge: bool,
}

impl<'a> EntryMarks<'a> {
    /// 本次 load 的代次（REQ-H04）。
    pub fn generation(&self) -> &'a LoadGeneration {
        self.generation
    }

    /// 本连接是否协商过分页（决定要不要打条目身份 / 位置戳）。
    pub fn merge(&self) -> bool {
        self.merge
    }

    /// 条目身份（见 [`entry_id`]）。
    pub fn entry_id(&self) -> String {
        entry_id(self.session_id, self.message_id)
    }

    /// 存储序键；`None` = 该条目不在本会话存储序里（继承条目）。
    pub fn position(&self) -> Option<u64> {
        self.position
    }
}

/// 把位置戳写进 meta / 事件 data（**唯一**一处拼装 `periEntryId` / `periPosition`）。
///
/// 未协商分页的连接（[`EntryMarks::merge`] 为 `false`）一个字段都不写：分页私有语义不得
/// 出现在标准完整回放的 wire 上（REQ §5.3）。已协商的连接则必须两条都带（`periPosition`
/// 只在继承条目上缺席——它们在本线程没有行）。
pub fn write_entry_marks(
    marks: &EntryMarks<'_>,
    target: &mut serde_json::Map<String, serde_json::Value>,
) {
    if !marks.merge() {
        return;
    }
    target.insert(
        "periEntryId".to_owned(),
        serde_json::Value::String(marks.entry_id()),
    );
    if let Some(position) = marks.position() {
        target.insert("periPosition".to_owned(), serde_json::Value::from(position));
    }
}

/// 把一条持久化 payload 投影成客户端可见的更新序列（回放与历史页的唯一实现）。
///
/// 不可展示的条目（System 消息）不产生子项：过滤在这里完成，调用方不需要再判一次。
/// 受众规则**不**在这里套用：回放出口与历史页出口各自在发送/装配时用
/// [`reminder_visible_to_client`] 过滤（同一份判据）。
///
/// `marks` 是**回放出口**的本次 load 代次与条目位置戳（REQ-H04、REQ-H01-3）：只有回放路径
/// 传 `Some`，历史页传 `None`——历史页的条目身份由响应装配给出，也不属于任何 load 代次。
pub fn project_payload_updates(
    payload: &PersistedPayload,
    caps: &PeriCaps,
    marks: Option<&EntryMarks<'_>>,
) -> Vec<ProjectedUpdate> {
    match payload {
        PersistedPayload::Message(message) => project_message_updates(message, caps, marks),
        PersistedPayload::SystemReminder { reminder, .. } => {
            vec![ProjectedUpdate::SystemReminder(
                reminder.as_reminder().clone(),
            )]
        }
    }
}

/// 本次初始 load 的代次（REQ-H04、REQ §5.2 `loadGeneration`）。
///
/// 服务端只承担两件事实：**回放通知**与**成功响应**带同一个代次，客户端才能把
/// 「属于哪一次 load」的通知与那次响应配对。按代次暂存/安装/丢弃（失败代次不得安装、
/// 迟到通知不污染新快照、重试不重复）属于 TUI / SDK 切片，不在这里。
///
/// 只在协商过回放标记的连接上出现（见 [`replay_chunk`]）：未协商的标准客户端收到的仍是
/// 纯 ACP 完整回放，不引入 Peri 私有语义（REQ §5.3）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoadGeneration(String);

impl LoadGeneration {
    /// 采用客户端给的代次（重试可以显式复用同一代次，服务端不解释其内容）。
    pub fn from_client(value: &str) -> Self {
        Self(value.to_owned())
    }

    /// 客户端没给代次时服务端生成一个：每次初始 load 一个，不跨 load 复用。
    pub fn generate() -> Self {
        Self(uuid::Uuid::now_v7().to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// H8 受众规则：该 reminder 是否可以出现在客户端出口。
///
/// 回放发送与历史页装配共用这一份判据；未声明客户端受众的 reminder（例如 Model-only
/// recall）不得因为回放或翻页而出现在 wire 上。
pub fn reminder_visible_to_client(reminder: &SystemReminder) -> bool {
    reminder_egress_allowed(reminder, ReminderAudience::Tui)
}

pub async fn replay_persisted_session_history(
    session_id: &str,
    history: &[PersistedPayload],
    sender: &dyn ReplaySender,
    caps: &PeriCaps,
    marks: &ReplayMarks<'_>,
) -> Result<(), ReplayError> {
    for payload in history {
        let marks = marks.entry(payload.id());
        for update in project_payload_updates(payload, caps, Some(&marks)) {
            send_projected(session_id, update, sender, caps, &marks).await?;
        }
    }
    Ok(())
}

/// 一段 canonical 完整回放的读取规模（未协商 `peri.historyPagination` 的标准客户端路径）。
///
/// 取值来源与理由：与历史页出口的两个默认值（`host::history_page` 的 `DEFAULT_PAGE_LIMIT`
/// = 50、`PAGE_BYTE_BUDGET` = 512 KiB）**同量级且当前取值相同**。同量级是刻意的：同一个
/// 客户端可以同时走两条出口（首屏流式回放 + 向更早翻页），任一侧把内存上限放大都会让另一侧
/// 的约束失效。两者各自持有命名常量、**不互相引用**：页的那两个是协议面默认值（可被请求里的
/// `limit` 覆盖），这两个是回放自己的固定规模，不出现在 wire 上（回放游标由存储铸造，这里
/// 不拼装）。
pub const REPLAY_SEGMENT_ROWS: usize = 50;

/// 一段回放的正文编码字节上限，见 [`REPLAY_SEGMENT_ROWS`]。
pub const REPLAY_SEGMENT_BYTE_BUDGET: usize = 512 * 1024;

/// 标准客户端（未协商 `peri.historyPagination`）的 canonical 完整回放：**流式**逐段读取并投影。
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
///
/// 带次 `generation` 是调用方（初始 load）算出的那一个：它与响应里的
/// `_meta.periLoadGeneration` 必须同源（REQ-H04）。条目身份 / 位置戳在这里**构造不出来**
/// ——本函数只造不打戳的 [`ReplayMarks`]，未协商的连接不可能因为调用顺序拿到分页私有字段。
pub async fn replay_canonical_history_streamed(
    controller: &Controller,
    session_id: &str,
    sender: &dyn ReplaySender,
    caps: &PeriCaps,
    generation: &LoadGeneration,
) -> Result<(), ReplayError> {
    // 未协商分页：一个条目身份 / 位置戳都不打（`merge = false`，见 [`EntryMarks`]）。代次仍随
    // `caps.replay` 出现——那是回放扩展的字段，门控见 [`replay_meta`]。
    let marks = ReplayMarks::without_entry_marks(generation, session_id);
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
                return replay_canonical_history_fallback(
                    controller, session_id, sender, caps, &marks,
                )
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
            let payload_marks = marks.entry(payload.id());
            for update in project_payload_updates(payload, caps, Some(&payload_marks)) {
                send_projected(session_id, update, sender, caps, &payload_marks).await?;
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
/// 与流式路径**同一份投影**（[`replay_persisted_session_history`]）与同一份不打戳标记，因此
/// 两条路的 wire 形状一致，客户端不需要知道走了哪一条。调用方保证只在「还没交付过任何东西」
/// 时进来。
async fn replay_canonical_history_fallback(
    controller: &Controller,
    session_id: &str,
    sender: &dyn ReplaySender,
    caps: &PeriCaps,
    marks: &ReplayMarks<'_>,
) -> Result<(), ReplayError> {
    let history = controller
        .sessions()
        .load_session_history(&ThreadId::from(session_id.to_owned()))
        .await
        .map_err(|error| {
            ReplayError::HistoryReadFailed(format!("canonical history load failed: {error}"))
        })?;
    replay_persisted_session_history(session_id, &history, sender, caps, marks).await
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
    marks: &ReplayMarks<'_>,
) -> Result<(), ReplayError> {
    for msg in history.iter() {
        let marks = marks.entry(msg.id());
        for update in project_message_updates(msg, caps, Some(&marks)) {
            send_projected(session_id, update, sender, caps, &marks).await?;
        }
    }
    Ok(())
}

/// 送出一次投影（回放出口）：`session/update` 走 `send`，结构化 reminder 走
/// `send_system_reminder`（受众过滤在实现里，见 [`reminder_visible_to_client`]）。
///
/// `marks` 是本条 payload 的戳，随投影一起交给出口：两条通道打出的字段必须同源。
async fn send_projected(
    session_id: &str,
    update: ProjectedUpdate,
    sender: &dyn ReplaySender,
    caps: &PeriCaps,
    marks: &EntryMarks<'_>,
) -> Result<(), ReplayError> {
    match update {
        ProjectedUpdate::Session(update) => {
            let notif = SessionNotification::new(SessionId::new(session_id.to_string()), *update);
            sender.send(notif).await
        }
        ProjectedUpdate::SystemReminder(reminder) => {
            sender
                .send_system_reminder(session_id, &reminder, caps, marks)
                .await
        }
    }
}

/// 一条消息的展示投影（回放与历史页共用的唯一实现）。
///
/// `marks` 见 [`project_payload_updates`]：回放出口传本条 payload 的戳，历史页传 `None`。
pub fn project_message_updates(
    msg: &BaseMessage,
    caps: &PeriCaps,
    marks: Option<&EntryMarks<'_>>,
) -> Vec<ProjectedUpdate> {
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
                    marks,
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
                            marks,
                            msg.id(),
                        ),
                    )));
                    // 纯文本 AI 消息无 blocks，tool_calls 由下方单独处理
                    for tc in tool_calls {
                        let tool_call = project_tool_start(&tc.id, &tc.name, &tc.arguments);
                        updates.push(ProjectedUpdate::session(SessionUpdate::ToolCall(
                            replay_tool(tool_call, caps, marks),
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
                                marks,
                                msg.id(),
                            ),
                        )));
                    }
                    PeriContentBlock::Text { text } => {
                        updates.push(ProjectedUpdate::session(SessionUpdate::AgentMessageChunk(
                            replay_chunk(
                                ContentBlock::Text(TextContent::new(text.clone())),
                                caps,
                                marks,
                                msg.id(),
                            ),
                        )));
                    }
                    PeriContentBlock::ToolUse { id, name, input } => {
                        emitted_ids.insert(id.clone());
                        let tc = project_tool_start(id, name, input);
                        updates.push(ProjectedUpdate::session(SessionUpdate::ToolCall(
                            replay_tool(tc, caps, marks),
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
                        replay_tool(tool_call, caps, marks),
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
                replay_tool_update(update, caps, marks),
            )));
        }
        _ => {}
    }
    updates
}

/// 回放出口的 meta 戳：`periReplay` 标记 + 本次 load 代次 + 条目位置戳。
///
/// `periReplay` / `periLoadGeneration` 共用同一个门（`caps.replay`）：未协商的连接收到的是
/// 纯 ACP 完整回放，不引入 Peri 私有语义（REQ §5.3）；协商过的连接才能按代次把回放通知与
/// 那次响应配对（REQ-H04）。
///
/// 位置戳（`periEntryId` / `periPosition`）只随调用方给出的 `marks` 出现——它由
/// **当前连接**是否协商过分页决定（见 [`ReplayMarks`]）。`marks` 为 `None` 是历史页出口：
/// 条目身份在那里由响应装配给出，且不属于任何一次 load 代次。
fn replay_meta(
    caps: &PeriCaps,
    marks: Option<&EntryMarks<'_>>,
) -> Option<serde_json::Map<String, serde_json::Value>> {
    let mut meta = serde_json::Map::new();
    if caps.replay {
        meta.insert("periReplay".to_string(), serde_json::Value::Bool(true));
        if let Some(marks) = marks {
            meta.insert(
                "periLoadGeneration".to_string(),
                serde_json::Value::String(marks.generation().as_str().to_owned()),
            );
        }
    }
    if let Some(marks) = marks {
        write_entry_marks(marks, &mut meta);
    }
    (!meta.is_empty()).then_some(meta)
}

fn replay_chunk(
    content: ContentBlock,
    caps: &PeriCaps,
    marks: Option<&EntryMarks<'_>>,
    message_id: peri_acp_types::messages::MessageId,
) -> ContentChunk {
    let mut chunk = ContentChunk::new(content);
    // ACP 标准 messageId 语义：replay 时携带消息真实 ID，与流式路径一致
    // （同一消息的 reasoning/text chunk 共享 ID，客户端段边界行为一致）。
    // v1 wire 上的 messageId 是字符串（规范消息 ID 的 UUID 串）。
    chunk.message_id = Some(agent_client_protocol_schema::v1::MessageId::from(
        message_id.as_uuid().to_string(),
    ));
    chunk.meta = replay_meta(caps, marks);
    chunk
}

/// 给 `ToolCall` 打上回放 meta 标记（`periReplay` + 代次 + 位置戳，见 [`replay_meta`]）。
fn replay_tool(mut tc: ToolCall, caps: &PeriCaps, marks: Option<&EntryMarks<'_>>) -> ToolCall {
    tc.meta = replay_meta(caps, marks);
    tc
}

/// 给 `ToolCallUpdate` 打上回放 meta 标记（与 [`replay_tool`] 同一份规则）。
fn replay_tool_update(
    mut tu: ToolCallUpdate,
    caps: &PeriCaps,
    marks: Option<&EntryMarks<'_>>,
) -> ToolCallUpdate {
    if let Some(meta) = replay_meta(caps, marks) {
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
///
/// `marks` 是本条 payload 的身份与位置戳（[`EntryMarks`]）：两条通道打出的
/// `periEntryId` / `periPosition` / `periLoadGeneration` 必须同源拼装
/// （[`write_entry_marks`] / [`replay_meta`]），不允许各自发明字段名。
#[async_trait::async_trait]
pub trait ReplaySender: Send + Sync {
    async fn send(&self, notif: SessionNotification) -> Result<(), ReplayError>;

    async fn send_system_reminder(
        &self,
        session_id: &str,
        reminder: &peri_acp_types::system_reminder::SystemReminder,
        caps: &PeriCaps,
        marks: &EntryMarks<'_>,
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
