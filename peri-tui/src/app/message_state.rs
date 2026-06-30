use ratatui::text::Line;

use crate::ui::message_view::MessageViewModel;

/// 消息状态 — 会话级的消息列表
pub struct MessageState {
    pub view_messages: Vec<MessageViewModel>,
    pub round_start_vm_idx: usize,
    pub last_submitted_text: Option<String>,
    /// Loading 期间用户缓存的消息（Agent 任务完成后自动提交）。
    pub pending_messages: Vec<String>,
    /// P5: Synchronous render cache — rebuilt in draw() when messages or width change.
    pub message_cache: Option<MessageRenderCache>,
    /// Phase 2.4 — App-method paths (thread_ops, agent_ops, rewind, etc.)
    /// call `push_system_note` directly without an Effect return path.
    /// These notes are queued here for `main_loop` to drain into the v2
    /// state machine (`Event::PushSystemNote`), so they reach `state.view`
    /// (the production render source) instead of being silently dropped.
    pub pending_v2_notes: Vec<String>,
}

/// 渲染换行信息：每个逻辑行在渲染后的视觉行范围。
#[derive(Clone, Debug)]
pub struct WrappedLineInfo {
    /// 该行在 cache.lines 中的索引
    pub line_idx: usize,
    /// 该逻辑行渲染后的起始视觉行号（基于 0）
    pub visual_row_start: u16,
    /// 该逻辑行渲染后的结束视觉行号（不含）
    pub visual_row_end: u16,
    /// 该逻辑行的纯文本内容（去样式，用于复制）
    pub plain_text: String,
    /// 每个字符的显示宽度序列（ASCII=1, CJK=2）
    pub char_widths: Vec<u8>,
}

/// P5: Synchronous render cache replacing async render_thread.
#[derive(Clone)]
pub struct MessageRenderCache {
    pub lines: Vec<Line<'static>>,
    pub wrap_map: Vec<WrappedLineInfo>,
    pub total_lines: usize,
    pub version: u64,
    pub width: u16,
}

impl MessageState {
    pub fn new() -> Self {
        Self {
            view_messages: Vec::new(),
            round_start_vm_idx: 0,
            last_submitted_text: None,
            pending_messages: Vec::new(),
            message_cache: None,
            pending_v2_notes: Vec::new(),
        }
    }

    /// 添加系统通知到 view_messages + pending_v2_notes 队列。
    ///
    /// `pending_v2_notes` 由 `main_loop` 取出并通过
    /// `state_machine::handle(Event::PushSystemNote)` 路由到
    /// `state.view`（生产渲染源）。view_messages 维护仅用于 legacy
    /// 兼容（测试路径），生产渲染已切换到 v2 state.view。
    pub fn push_system_note(&mut self, content: String) {
        let vm = MessageViewModel::system(content.clone());
        self.view_messages.push(vm);
        self.pending_v2_notes.push(content);
    }

    /// 取出所有待路由到 v2 状态机的系统通知（清空队列）。
    pub fn drain_pending_v2_notes(&mut self) -> Vec<String> {
        std::mem::take(&mut self.pending_v2_notes)
    }
}
