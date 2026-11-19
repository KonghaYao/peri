// ---------------------------------------------------------------------------
// 折叠状态机——折叠策略的**唯一**定义点（契约见 docs/design/tui-chat-workbench.md）
// ---------------------------------------------------------------------------

use super::bubble::TuiAssistantBubble;
use super::hash::{tui_hash_combine, tui_hash_roll};

/// 折叠三态——`Collapsed` 单行 / `Preview` 有界 tail / `Expanded` 完整 body。
/// 默认策略只产出 `Collapsed` / `Expanded`；`Preview` 是用户 Space 手动覆盖的中态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FoldState {
    #[default]
    Collapsed,
    Preview,
    Expanded,
}

/// Entry 生命周期状态——折叠表按此选择默认 fold。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EntryStatus {
    Running,
    #[default]
    Completed,
    Error,
}

/// 折叠策略表的 entry 类型维度。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FoldTarget {
    User,
    Assistant,
    Reasoning,
    Tool,
    SubAgent,
    System,
    Interaction,
}

/// 折叠覆盖键——用户手动操作过的 entry 身份（手动改变 fold state 后，本 turn 内
/// 不再被自动策略覆盖）。按 ACP 身份字段键控：
/// `Reasoning(message_id)` / `Tool(tool_id)` / `SubAgent(agent_id)` /
/// `Interaction(request_id)` / `SystemReminder(reminder_id)`。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum FoldKey {
    Reasoning(String),
    /// 无 ACP `message_id` 的已完成 reasoning：按内容身份键控
    /// （见 [`reasoning_fold_key`]）。历史回放/历史页不携带 messageId，
    /// 仍需保留逐项展开能力。
    ReasoningLocal(u64),
    Tool(String),
    SubAgent(String),
    /// Interaction block 按事件携带的本地 request_id 键控；测试构造为 None 时
    /// `fold_key_of` 返回 None——与 reasoning 的 message_id 先例一致）。
    Interaction(String),
    /// System reminder 使用 TUI session 内分配的稳定身份键控。
    SystemReminder(u64),
}

/// \[G2\] 折叠策略表——每个 entry 类型 × 状态的默认折叠目标。
///
/// 唯一折叠策略单点：`push_view_models` 的折叠 pass（以及未来所有消费者）
/// 只能从这里取值，禁止在别处内联折叠决策。
pub fn fold_for_status(target: FoldTarget, status: EntryStatus) -> FoldState {
    use EntryStatus::*;
    use FoldTarget::*;
    match (target, status) {
        // user / assistant 正文永远展开（user 长文折叠归 Slice 3 截断层）
        (User, _) | (Assistant, _) => FoldState::Expanded,
        // reasoning / tool：正文与正文之间的过程 entry 一律单行摘要，点击该项
        // 才展开（用户需求：提升纵向阅读效率；不再按状态做 tail preview）。
        // running 的活动感由状态符号、elapsed 与摘要文本承担；失败仍由状态符号
        // 与 Failed 文案保持可见。
        (Reasoning, _) => FoldState::Collapsed,
        (Tool, _) => FoldState::Collapsed,
        // subagent：running = Collapsed + live summary（裁决 C4：按 spec §7 表）
        (SubAgent, Running) | (SubAgent, Completed) => FoldState::Collapsed,
        (SubAgent, Error) => FoldState::Expanded,
        // system：普通事件单行 divider，error 展开摘要
        (System, Running) | (System, Completed) => FoldState::Collapsed,
        (System, Error) => FoldState::Expanded,
        // interaction：等待时 expanded 可聚焦，答毕保持完整展示（问题 + 选项
        // + 结果行始终可见，不自动收束；用户 Space 手动折叠仍生效）
        (Interaction, Running) => FoldState::Expanded,
        (Interaction, Completed) => FoldState::Expanded,
        (Interaction, Error) => FoldState::Expanded,
    }
}

/// 无 `message_id` 的 reasoning 折叠策略：这类 entry 无法写稳定覆盖键，因此
/// 不能安全折叠——流式段保留 `Preview`（正文可见），完成后走常规表
/// （`Collapsed`，由内容身份键展开）。
///
/// [Why] 默认单行折叠依赖「点击可展开」；键缺失时折叠等于永久隐藏正文。
pub fn fold_for_unkeyed_reasoning(status: EntryStatus) -> FoldState {
    match status {
        EntryStatus::Running => FoldState::Preview,
        _ => fold_for_status(FoldTarget::Reasoning, status),
    }
}

/// reasoning 的折叠身份——导航、折叠 pass 与详情面板共用同一口径。
///
/// 有 ACP `message_id` → `Reasoning(id)`；缺失时仅已完成块可按内容键控
/// （历史回放/历史页的正文不再变化）。流式块逐 chunk 变化、没有稳定身份，
/// 返回 `None`：Enter 不切换，渲染侧由 [`fold_for_unkeyed_reasoning`] 保持可见。
#[must_use]
pub fn reasoning_fold_key(bubble: &TuiAssistantBubble) -> Option<FoldKey> {
    let reasoning = bubble.reasoning.as_ref()?;
    if let Some(id) = &bubble.message_id {
        return Some(FoldKey::Reasoning(id.clone()));
    }
    if reasoning.is_running || reasoning.status == EntryStatus::Running {
        return None;
    }
    let mut hash = tui_hash_roll(&bubble.text);
    hash = tui_hash_combine(hash, tui_hash_roll(&reasoning.text));
    Some(FoldKey::ReasoningLocal(hash))
}

/// `FoldState` 的确定性 hash 代码——纳入 content_hash 公式（G1）。
#[must_use]
pub fn fold_state_code(f: FoldState) -> u64 {
    match f {
        FoldState::Collapsed => 1,
        FoldState::Preview => 2,
        FoldState::Expanded => 3,
    }
}

/// `EntryStatus` 的确定性 hash 代码——纳入 content_hash 公式（G1）。
#[must_use]
pub fn entry_status_code(s: EntryStatus) -> u64 {
    match s {
        EntryStatus::Running => 1,
        EntryStatus::Completed => 2,
        EntryStatus::Error => 3,
    }
}
