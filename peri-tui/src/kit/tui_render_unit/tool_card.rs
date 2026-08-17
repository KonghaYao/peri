use super::diff::{TuiDiffBlock, diff_change_counts};
use super::fold::FoldState;
use super::hash::{tui_hash_combine, tui_hash_str};

/// Tool invocation card -- name, summaries, optional diff.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum TuiToolPresentation {
    #[default]
    Generic,
    Skill(TuiSkillPresentation),
    Todo(TuiTodoPresentation),
}

/// User-facing information for a successfully loaded skill.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TuiSkillPresentation {
    pub name: String,
}

/// User-facing TodoWrite snapshot and the changes from its previous successful call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TuiTodoPresentation {
    pub current_items: Vec<TuiTodoItem>,
    pub changes: Vec<TuiTodoChange>,
    pub is_initial: bool,
    pub completed_count: usize,
    pub total_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TuiTodoItem {
    pub content: String,
    pub active_form: Option<String>,
    pub status: TuiTodoStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TuiTodoStatus {
    Pending,
    InProgress,
    Completed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TuiTodoChange {
    pub kind: TuiTodoChangeKind,
    pub content: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TuiTodoChangeKind {
    Added,
    Started,
    Completed,
    Reopened,
    ActiveFormUpdated,
    Removed,
}

/// Tool invocation card -- name, summaries, optional diff.
#[derive(Debug, Clone)]
pub struct TuiToolCard {
    /// Stable identifier for this tool call.
    pub tool_id: String,
    /// Human-readable tool name (e.g. "Edit", "Bash").
    pub tool_name: String,
    /// One-line summary of the input / arguments.
    pub input_summary: String,
    /// One-line summary of the output / result.
    pub output_summary: String,
    /// Whether the tool invocation resulted in an error.
    pub is_error: bool,
    /// Whether the tool is still streaming/running.
    pub is_running: bool,
    /// Elapsed time in milliseconds for a running tool.
    pub running_duration_ms: Option<u64>,
    /// 已完成的工具冻结时长（毫秒）——`end_tool` 时由同源 `started_at` 冻结
    /// （G-started_at），完成行 `37ms`/`4.2s` 显示用；Running 中为 `None`。
    pub completed_duration_ms: Option<u64>,
    /// Inline diff preview (Write / Edit tools).
    pub diff: Option<TuiDiffBlock>,
    /// 专属工具的用户语义展示；默认保持通用工具卡片。
    pub presentation: TuiToolPresentation,
    /// 折叠状态——折叠 pass（spec §7 表）与用户覆盖（FOLD_OVERRIDES）驱动。
    pub fold: FoldState,
    /// 用户手动操作过折叠状态——自动策略免疫（spec §7）。
    pub user_modified: bool,
    /// 内容哈希——rebuild 时用于检测是否需重新渲染
    pub content_hash: u64,
    /// Agent 工具专用的子工具调用计数（由 sync_cache 后处理 pair_agent_tool_cards 配对填充）。
    pub tool_calls_count: usize,
}

impl TuiToolCard {
    /// [G1] 内容哈希公式单点——`build_tool_card` / replay 构造 / 折叠 pass 共用。
    /// 包含 fold + user_modified：折叠状态变化必须触发按 hash 分片的渲染缓存重建。
    /// duration 按秒取整后纳入 hash——避免每毫秒 hash 变化导致分片缓存频繁失效；
    /// 同时保证 duration 文本每秒刷新。completed_duration_ms 是冻结值（秒级稳定）。
    pub fn recompute_hash(&mut self) {
        let duration_secs = self.running_duration_ms.map(|ms| ms / 1000);
        let completed_secs = self.completed_duration_ms.map(|ms| ms / 1000);
        let mut h = tui_hash_str(&format!(
            "{}|{}|{}|{}|{}|{}|{:?}|{:?}|{:?}|{:?}|{}",
            self.tool_id,
            self.tool_name,
            self.input_summary,
            self.output_summary,
            self.is_error,
            self.is_running,
            duration_secs,
            completed_secs,
            self.presentation,
            self.fold,
            self.user_modified,
        ));
        // [G-Diff] diff 定型于 tool-ended，此后不变——稳定摘要纳入 hash 保证
        // diff 变更（含路径/计数/截断）触发按 hash 分片的渲染缓存重建。
        h = tui_hash_combine(h, self.diff_code());
        self.content_hash = h;
    }

    /// [G-Diff] diff 的稳定摘要 hash：path + 总 change 数 + is_binary/is_too_large/
    /// is_new_file + 截断信息。`None` → 0（普通工具卡无 diff 不改变 hash）。
    pub fn diff_code(&self) -> u64 {
        let Some(d) = &self.diff else {
            return 0;
        };
        let (adds, dels) = diff_change_counts(d);
        let mut h = tui_hash_combine(0, tui_hash_str(&d.path));
        h = tui_hash_combine(h, adds as u64);
        h = tui_hash_combine(h, dels as u64);
        h = tui_hash_combine(h, d.more_change_lines as u64);
        h = tui_hash_combine(h, u64::from(d.is_binary));
        h = tui_hash_combine(h, u64::from(d.is_too_large));
        h = tui_hash_combine(h, u64::from(d.is_new_file));
        h
    }
}

tui_impl_partial_eq!(TuiToolCard: tool_id, tool_name, input_summary, output_summary, is_error, is_running, running_duration_ms, completed_duration_ms, diff, presentation, fold, user_modified);
