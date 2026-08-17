// ---------------------------------------------------------------------------
// 折叠状态机（spec §7）——折叠策略的**唯一**定义点
// ---------------------------------------------------------------------------

/// 折叠三态（spec §7）——`Collapsed` 单行 / `Preview` 有界 tail / `Expanded` 完整 body。
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

/// §7 折叠表的 entry 类型维度。
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

/// 折叠覆盖键——用户手动操作过的 entry 身份（spec §7「用户手动改变 fold state
/// 后，本 turn 内不再被自动策略覆盖」）。按 ACP 身份字段键控：
/// `Reasoning(message_id)` / `Tool(tool_id)` / `SubAgent(agent_id)` /
/// `Interaction(request_id)`。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum FoldKey {
    Reasoning(String),
    Tool(String),
    SubAgent(String),
    /// Interaction block 按本地 request_id 键控（生产创建点从
    /// HITL_REQUEST_ID / ASK_USER_REQUEST_ID atom 克隆；测试构造为 None 时
    /// `fold_key_of` 返回 None——与 reasoning 的 message_id 先例一致）。
    Interaction(String),
}

/// [G2] spec §7 折叠表——每个 entry 类型 × 状态的默认折叠目标。
///
/// 唯一折叠策略单点：`push_view_models` 的折叠 pass（以及未来所有消费者）
/// 只能从这里取值，禁止在别处内联折叠决策。
pub fn fold_for_status(target: FoldTarget, status: EntryStatus) -> FoldState {
    use EntryStatus::*;
    use FoldTarget::*;
    match (target, status) {
        // user / assistant 正文永远展开（user 长文折叠归 Slice 3 截断层）
        (User, _) | (Assistant, _) => FoldState::Expanded,
        // reasoning：running = tail preview，completed 自动收束为单行
        (Reasoning, Running) | (Reasoning, Error) => FoldState::Preview,
        (Reasoning, Completed) => FoldState::Collapsed,
        // tool：running = tail preview，success 默认折叠，error 展开错误摘要
        (Tool, Running) => FoldState::Preview,
        (Tool, Completed) => FoldState::Collapsed,
        (Tool, Error) => FoldState::Expanded,
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

/// `FoldState` 的确定性 hash 代码——纳入 content_hash 公式（G1）。
pub fn fold_state_code(f: FoldState) -> u64 {
    match f {
        FoldState::Collapsed => 1,
        FoldState::Preview => 2,
        FoldState::Expanded => 3,
    }
}

/// `EntryStatus` 的确定性 hash 代码——纳入 content_hash 公式（G1）。
pub fn entry_status_code(s: EntryStatus) -> u64 {
    match s {
        EntryStatus::Running => 1,
        EntryStatus::Completed => 2,
        EntryStatus::Error => 3,
    }
}
