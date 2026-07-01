use ratatui::text::Line;

/// 消息状态 — 会话级的消息列表
pub struct MessageState {
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

    /// Cron #24 P1 #2 — AskUser 答案由 `ask_user_confirm` 推送到此队列，
    /// `main_loop` 取出后通过 `Event::PushUserBubble` 路由到 v2 `state.view`。
    ///
    /// 历史 bug：原代码直接 `view_messages.push(UserBubble)`，但 v2 渲染路径
    /// 只读 `state.view`，导致用户回答后答案在生产渲染中消失。镜像
    /// `pending_v2_notes` 的 queue-and-drain 模式修复。
    pub pending_v2_user_bubbles: Vec<String>,
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

impl Default for MessageState {
    fn default() -> Self {
        Self::new()
    }
}

impl MessageState {
    pub fn new() -> Self {
        Self {
            round_start_vm_idx: 0,
            last_submitted_text: None,
            pending_messages: Vec::new(),
            message_cache: None,
            pending_v2_notes: Vec::new(),
            pending_v2_user_bubbles: Vec::new(),
        }
    }

    /// 入队一条系统通知到 v2 状态机渲染源。
    ///
    /// Phase 2.6 step 5 — `view_messages.push` 分支已删除。SystemNote
    /// 仅入 `pending_v2_notes` 队列，由 `main_loop` 取出并通过
    /// `state_machine::handle(Event::PushSystemNote)` 路由到
    /// `state.view`（生产渲染源）。
    pub fn push_system_note(&mut self, content: String) {
        self.pending_v2_notes.push(content);
    }

    /// 取出所有待路由到 v2 状态机的系统通知（清空队列）。
    pub fn drain_pending_v2_notes(&mut self) -> Vec<String> {
        std::mem::take(&mut self.pending_v2_notes)
    }

    /// Cron #24 P1 #2 — 入队一条 UserBubble 文本到 v2 状态机渲染源。
    ///
    /// 由 `ask_user_confirm` 调用，将用户的回答格式化后入队。`main_loop`
    /// 取出后通过 `Event::PushUserBubble` 路由到 `state.view`，确保
    /// 答案在生产渲染路径中可见。
    pub fn push_user_bubble(&mut self, text: String) {
        self.pending_v2_user_bubbles.push(text);
    }

    /// 取出所有待路由到 v2 状态机的 UserBubble 文本（清空队列）。
    pub fn drain_pending_v2_user_bubbles(&mut self) -> Vec<String> {
        std::mem::take(&mut self.pending_v2_user_bubbles)
    }
}
