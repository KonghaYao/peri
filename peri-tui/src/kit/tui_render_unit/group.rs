use super::fold::{FoldState, fold_state_code};
use super::hash::{tui_hash_combine, tui_hash_str};
use super::unit::TuiRenderUnit;

/// System notification -- centered banner for model switches, compact, etc.
#[derive(Debug, Clone)]
pub struct TuiSystemNote {
    pub text: String,
    pub level: TuiNoteLevel,
    /// 内容哈希——rebuild 时用于检测是否需重新渲染
    pub content_hash: u64,
}

tui_impl_partial_eq!(TuiSystemNote: text, level);

/// Severity of a system note.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TuiNoteLevel {
    Info,
    Warning,
    Error,
}

/// Sub-agent message group -- bounded by start/stop events.
///
/// Nested `view_models` render inside a collapsible container.
#[derive(Debug, Clone)]
pub struct TuiSubAgentGroup {
    pub agent_id: String,
    pub agent_name: String,
    /// Nested view models produced by the sub-agent.
    pub view_models: im::Vector<TuiRenderUnit>,
    /// Whether the group is currently collapsed（详情面板语义；消息区折叠由 fold 驱动）。
    pub collapsed: bool,
    /// Whether the sub-agent is still streaming.
    pub is_running: bool,
    /// Parent 终态唯一事实源——`SubagentStopped.is_error`；nested child tool
    /// error 不提升 block error。参与 hash/PartialEq（终态变化必须刷新缓存）。
    pub is_error: bool,
    /// Genuine parent error 的可见原因（`SubagentStopped.result` 非空时保存）；
    /// 不覆盖 header 的 result 摘要。参与 hash/PartialEq。
    pub error_reason: Option<String>,
    /// 折叠状态——折叠 pass（spec §7 表）与用户覆盖（FOLD_OVERRIDES）驱动。
    pub fold: FoldState,
    /// 用户手动操作过折叠状态——自动策略免疫（spec §7）。
    pub user_modified: bool,
    /// 内容哈希——rebuild 时用于检测是否需重新渲染
    pub content_hash: u64,
}

impl TuiSubAgentGroup {
    /// [G1] 内容哈希公式单点——`SubAgentAccumulator::view_model` 构造与折叠 pass
    /// 共用（含 fold + user_modified：状态变化必须触发分片渲染缓存重建）。
    pub fn recompute_hash(&mut self) {
        // 用 u64 组合累加每个 child VM 的 content_hash，确保 child 文本
        // 变化时（即使 view_models.len() 不变）也能触发按 hash 分片的渲染缓存重建。
        let mut child_hash_total: u64 = 0;
        for vm in self.view_models.iter() {
            child_hash_total = tui_hash_combine(child_hash_total, vm.content_hash());
        }
        let mut h = tui_hash_combine(0, tui_hash_str(&self.agent_id));
        h = tui_hash_combine(h, tui_hash_str(&self.agent_name));
        h = tui_hash_combine(h, self.view_models.len() as u64);
        h = tui_hash_combine(h, 0); // collapsed 恒为 false（详情面板保持展开；消息区折叠由 fold 驱动）
        h = tui_hash_combine(h, u64::from(self.is_running));
        h = tui_hash_combine(h, u64::from(self.is_error));
        h = tui_hash_combine(
            h,
            self.error_reason.as_deref().map(tui_hash_str).unwrap_or(0),
        );
        h = tui_hash_combine(h, fold_state_code(self.fold));
        h = tui_hash_combine(h, u64::from(self.user_modified));
        h = tui_hash_combine(h, child_hash_total);
        self.content_hash = h;
    }
}

tui_impl_partial_eq!(TuiSubAgentGroup: agent_id, agent_name, view_models, collapsed, is_running, is_error, error_reason, fold, user_modified);

/// Generic collapsible group -- e.g. batched tool calls.
#[derive(Debug, Clone)]
pub struct TuiCollapsedGroup {
    pub title: String,
    /// Number of items hidden when collapsed.
    pub count: u32,
    /// 组后**连续相邻**的 error 工具数（D2：error 不入组、不删除、保持展开，
    /// 标题追加 `· N failed`）。由 `group_successful_tools` 从 run 结束位置
    /// 向后扫描连续相邻 error `TuiToolCard` 计入。
    pub failed_count: u32,
    /// The view models inside the group (visible when expanded).
    pub view_models: Vec<TuiRenderUnit>,
    /// 内容哈希——rebuild 时用于检测是否需重新渲染
    pub content_hash: u64,
}

impl TuiCollapsedGroup {
    /// [G1] 内容哈希公式单点——`group_successful_tools` 构造时调用。
    /// 成员变化（标题/数量/失败数/隐藏 VM）必须触发按 hash 分片的渲染缓存重建。
    pub fn recompute_hash(&mut self) {
        let mut child_hash_total: u64 = 0;
        for vm in self.view_models.iter() {
            child_hash_total = tui_hash_combine(child_hash_total, vm.content_hash());
        }
        let mut h = tui_hash_combine(0, tui_hash_str(&self.title));
        h = tui_hash_combine(h, u64::from(self.count));
        h = tui_hash_combine(h, u64::from(self.failed_count));
        h = tui_hash_combine(h, self.view_models.len() as u64);
        h = tui_hash_combine(h, child_hash_total);
        self.content_hash = h;
    }
}

tui_impl_partial_eq!(TuiCollapsedGroup: title, count, failed_count, view_models);

/// Visual separator between iteration rounds.
#[derive(Debug, Clone)]
pub struct TuiDivider {
    /// Optional label rendered next to the line (e.g. "Round 3").
    pub label: Option<String>,
    /// 内容哈希——rebuild 时用于检测是否需重新渲染
    pub content_hash: u64,
}

tui_impl_partial_eq!(TuiDivider: label);

/// §6.9 活动 turn 的 todo 进度摘要——`3/7 tasks · Running tests`。
///
/// 由 `push_view_models` 从 `TODO_ITEMS` 派生（快照后处理，非 segment 缓存），
/// 插在当前 turn 的最终回答之前；turn 结束后随 current_turn 一起消失。
#[derive(Debug, Clone)]
pub struct TuiTodoSummary {
    /// 摘要文本（含完成数/总数与 in-progress 项内容）。
    pub text: String,
    /// 内容哈希——todo 内容变化时触发按 hash 分片的渲染缓存重建。
    pub content_hash: u64,
}

impl TuiTodoSummary {
    /// 从摘要文本构造（hash = f(text)）。
    pub fn new(text: String) -> Self {
        let content_hash = tui_hash_str(&text);
        Self { text, content_hash }
    }
}

tui_impl_partial_eq!(TuiTodoSummary: text);
