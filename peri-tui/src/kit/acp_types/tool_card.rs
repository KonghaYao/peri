use super::current_turn::CurrentTurn;
use crate::kit::tool_semantics::{TodoSnapshot, presentation_for};
use crate::kit::tui_render_unit::{
    EntryStatus, FoldTarget, TuiRenderUnit, TuiToolCard, TuiToolPresentation, fold_for_status,
};
use serde_json::Value;
use std::time::Instant;

/// 从 `ToolCardAccumulator` 派生 `TuiToolCard`。
///
/// fold 按 spec §7 表取当前状态的目标值（running=Preview / completed/error=
/// Collapsed）；工具只有在所属 turn 仍 active 且尚无输出时才是 running，
/// 避免 turn 取消后缺失 `ToolEnded` 导致卡片永久显示 spinner。hash 由
/// [`TuiToolCard::recompute_hash`] 单点计算（含 fold + user_modified，duration
/// 按秒取整避免每毫秒 hash 抖动）。
pub(crate) fn build_tool_card(t: &ToolCardAccumulator, turn_active: bool) -> TuiToolCard {
    let is_running = turn_active && t.output_summary.is_none();
    let running_duration_ms = is_running.then(|| t.started_at.elapsed().as_millis() as u64);
    let status = if is_running {
        EntryStatus::Running
    } else if t.is_error {
        EntryStatus::Error
    } else {
        EntryStatus::Completed
    };
    let mut card = TuiToolCard {
        tool_id: t.tool_id.clone(),
        tool_name: t.tool_name.clone(),
        input_summary: t.input_summary.clone(),
        output_summary: t.output_summary.clone().unwrap_or_default(),
        is_error: t.is_error,
        is_running,
        running_duration_ms,
        // [G-started_at] completed 时长在 end_tool 时冻结（同源 started_at），
        // 与 running 时长同一口径；hash 按秒取整（recompute_hash）。
        completed_duration_ms: if is_running {
            None
        } else {
            t.completed_duration_ms
        },
        // [G-Diff] Edit/Write 完成且非 error 时解析输出中的 unified diff——
        // 解析失败（非法/二进制/超限）静默降级到 diff_change_summary 兜底；
        // path_hint 取自原始输入的 file_path（Edit/Write 摘要口径）。
        diff: parse_tool_diff(
            &t.tool_name,
            t.output_summary.as_deref().unwrap_or(""),
            t.output_summary.is_none() || t.is_error,
            tool_path_hint(&t.tool_name, &t.raw_input),
        ),
        presentation: t.presentation.clone(),
        fold: fold_for_status(FoldTarget::Tool, status),
        user_modified: false,
        tool_calls_count: 0,
        content_hash: 0,
    };
    card.recompute_hash();
    card
}

/// [G-Diff] 从工具原始输入提取 path hint（Edit/Write 的 `file_path`）。
fn tool_path_hint(tool_name: &str, raw_input: &serde_json::Value) -> Option<String> {
    if !matches!(tool_name, "Edit" | "Write") {
        return None;
    }
    raw_input
        .get("file_path")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .filter(|p| !p.is_empty())
}

/// [G-Diff] 生产路径的 diff 解析入口：仅 Edit/Write 完成态（非 running、
/// 非 error）尝试解析；其余场景恒 `None`（数据不可达省略，G-Tokens 同口径）。
pub(crate) fn parse_tool_diff(
    tool_name: &str,
    output: &str,
    skip: bool,
    path_hint: Option<String>,
) -> Option<crate::kit::tui_render_unit::TuiDiffBlock> {
    if skip || !matches!(tool_name, "Edit" | "Write") {
        return None;
    }
    // [Slice 5] 两段式：优先 unified diff（协议未来携带 diff 文本时自动接管），
    // 失败后回退真实摘要文本（"Added 3 lines to P" 等——事件流中 Edit/Write
    // 输出即摘要，unified diff 数据不可达）。
    crate::kit::diff_parser::parse_unified_diff(output, path_hint.as_deref())
        .or_else(|| crate::kit::diff_parser::parse_edit_write_summary(output, path_hint.as_deref()))
}

/// In-progress tool card accumulator.
#[derive(Debug, Clone)]
pub struct ToolCardAccumulator {
    /// Tool call identifier (matches `tool_id` from the protocol).
    pub tool_id: String,
    /// Human-readable tool name (e.g. `"Edit"`, `"Bash"`).
    pub tool_name: String,
    /// Short summary of the tool's input arguments.
    pub input_summary: String,
    /// 原始结构化输入；仅供专属语义展示生成，绝不从输出推导。
    pub raw_input: Value,
    /// 专属工具卡片的用户语义展示；未知工具保持通用卡片。
    pub presentation: TuiToolPresentation,
    /// Short summary of the tool's output (filled by `"tool-ended"`).
    pub output_summary: Option<String>,
    /// Whether the tool returned an error.
    pub is_error: bool,
    /// When the tool started on the TUI side.
    pub started_at: Instant,
    /// Completed 时长（毫秒）——`end_tool` 时由 `started_at` 冻结一次，
    /// 之后不再增长（§6.4 完成行的 `37ms`/`4.2s`；G-started_at）。
    /// Running 中为 `None`。
    pub completed_duration_ms: Option<u64>,
    /// 是否已有 SubAgent segment 声明与此 ToolCard 关联。
    /// 防止多 Agent 场景下 SubAgent 段错配到错误的 ToolCard。
    pub claimed_by_subagent: bool,
}

impl ToolCardAccumulator {
    /// Create a generic in-progress tool card from a replay or legacy event.
    pub fn new(tool_id: String, tool_name: String, input_summary: String) -> Self {
        Self::with_input(tool_id, tool_name, input_summary, Value::Null, None)
    }

    /// Create an in-progress card while retaining the structured input for semantic rendering.
    pub(crate) fn with_input(
        tool_id: String,
        tool_name: String,
        input_summary: String,
        raw_input: Value,
        previous_todos: Option<&TodoSnapshot>,
    ) -> Self {
        let presentation = presentation_for(&tool_name, &raw_input, previous_todos);
        Self {
            tool_id,
            tool_name,
            input_summary,
            raw_input,
            presentation,
            output_summary: None,
            is_error: false,
            started_at: Instant::now(),
            completed_duration_ms: None,
            claimed_by_subagent: false,
        }
    }
}

/// In-progress sub-agent accumulator.
#[derive(Debug, Clone)]
pub struct SubAgentAccumulator {
    pub agent_id: String,
    pub agent_name: String,
    pub is_running: bool,
    /// Parent 终态唯一事实源——由 `SubagentStopped.is_error` 写入；
    /// nested child tool error 不参与。
    pub is_error: bool,
    /// Genuine parent error 的可见原因（`SubagentStopped.result` 非空白时保存，
    /// 原始文本未 trim）。
    pub error_reason: Option<String>,
    pub child_turn: CurrentTurn,
    /// Cached view_model result, invalidated on any mutation.
    pub(super) cached_view_model: std::cell::RefCell<Option<TuiRenderUnit>>,
}

impl SubAgentAccumulator {
    pub fn new(agent_id: String, agent_name: String) -> Self {
        let child_turn = CurrentTurn::new();
        Self {
            agent_id,
            agent_name,
            is_running: true,
            is_error: false,
            error_reason: None,
            child_turn,
            cached_view_model: std::cell::RefCell::new(None),
        }
    }

    pub(super) fn append_text(&mut self, text: &str) {
        self.child_turn.append_text(text, None);
        self.cached_view_model.replace(None);
    }

    pub(super) fn append_reasoning(&mut self, text: &str) {
        self.child_turn.append_reasoning(text, None);
        self.cached_view_model.replace(None);
    }

    pub(super) fn start_tool(&mut self, tool: ToolCardAccumulator) {
        self.child_turn.start_tool(tool);
        self.cached_view_model.replace(None);
    }

    pub(super) fn end_tool(&mut self, tool_id: &str, output: String, is_error: bool) -> bool {
        let ended = self.child_turn.end_tool(tool_id, output, is_error);
        if ended {
            self.cached_view_model.replace(None);
        }
        ended
    }

    pub(crate) fn view_model(&mut self) -> TuiRenderUnit {
        // 缓存命中——直接返回。子 turn 的 VM 缓存由 mutation 时 eager sync 维护，
        // 这里不再需要 child_turn.clone() + view_models() 全量重建。
        if let Some(vm) = self.cached_view_model.borrow().as_ref() {
            return vm.clone();
        }

        // 统一走 child_turn 的统一入口（走 cache_dirty 检查，dirty 时先 sync_cache）
        // 而非直读 cached_view_models 私有字段。当前生产路径 child_turn.cache_dirty
        // 恒为 false（mutation 全部 eager sync，invalidate_cache 只作用于顶层
        // current_turn），此时该入口是 O(1) 直读，与直接访问字段成本相同；一旦未来
        // 对 child_turn 增加 invalidate 调用（如子工具时长刷新），这里会自动重同步，
        // 不会静默渲染陈旧内容。
        let child_vms = self.child_turn.view_models();
        let status = if self.is_running {
            EntryStatus::Running
        } else if self.is_error {
            EntryStatus::Error
        } else {
            EntryStatus::Completed
        };
        // [G1] hash 由 TuiSubAgentGroup::recompute_hash 单点计算（含 fold +
        // user_modified + child hash 组合）——构造与折叠 pass 共用同一公式。
        let mut group = crate::kit::tui_render_unit::TuiSubAgentGroup {
            agent_id: self.agent_id.clone(),
            agent_name: self.agent_name.clone(),
            view_models: child_vms.clone(),
            collapsed: false,
            is_running: self.is_running,
            is_error: self.is_error,
            error_reason: self.error_reason.clone(),
            fold: fold_for_status(FoldTarget::SubAgent, status),
            user_modified: false,
            content_hash: 0,
        };
        group.recompute_hash();
        let vm = TuiRenderUnit::TuiSubAgentGroup(group);
        self.cached_view_model.replace(Some(vm.clone()));
        vm
    }
}
