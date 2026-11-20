// ---------------------------------------------------------------------------
// 折叠状态机——折叠策略的**唯一**定义点（契约见 docs/design/tui-chat-workbench.md）
//
// 正文空档折叠组是唯一的过程区折叠层级：组行默认 `Collapsed`（由分组 pass 构造），
// 组内成员强制 `Expanded`；本表的工具 / reasoning 默认值只服务非分组语境。
// ---------------------------------------------------------------------------

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
    Tool(String),
    SubAgent(String),
    /// 正文空档折叠组——键为组首成员的稳定身份（`tool:<id>` / `reasoning:<id>` 等）。
    /// 流式追加只在组尾增员，键不变，用户展开选择跨重建保持。
    Group(String),
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
        // reasoning / tool：收束为单行。正文空档分组保留这里给出的 fold——
        // 组行折叠一层，展开组后成员仍是单行、可各自点开。
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

/// 无 `message_id` 的 reasoning 折叠策略：没有稳定身份就无法写覆盖键，
/// 也不能安全折叠——流式段保留 `Preview`（有界 tail），完成后 `Expanded`
/// （正文完整可见）。
///
/// [Why] 默认单行折叠依赖「点击可展开」；键缺失时折叠等于永久隐藏正文。
/// 正常路径（live / 回放 / 历史页）都应携带 message_id；此分支只服务
/// 缺失该字段的兼容数据。
pub fn fold_for_unkeyed_reasoning(status: EntryStatus) -> FoldState {
    match status {
        EntryStatus::Running => FoldState::Preview,
        _ => FoldState::Expanded,
    }
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
