use ratatui::text::Line;

use crate::ui::message_view::MessageViewModel;

/// 消息状态���会话级的消息列表
pub struct MessageState {
    pub view_messages: Vec<MessageViewModel>,
    pub round_start_vm_idx: usize,
    pub last_submitted_text: Option<String>,
    /// 临时系统通知（不在 BaseMessage[] 中），记录 (锚点索引, VM)。
    /// 锚点 = 创建时 view_messages.len()，RebuildAll 时按锚点插入到对应位置。
    pub ephemeral_notes: Vec<(usize, MessageViewModel)>,
    /// Loading 期间用户缓存的消息（Agent 任务完成后自动提交）。
    pub pending_messages: Vec<String>,
    /// P5: Synchronous render cache — rebuilt in draw() when messages or width change.
    pub message_cache: Option<MessageRenderCache>,
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
            ephemeral_notes: Vec::new(),
            pending_messages: Vec::new(),
            message_cache: None,
        }
    }

    /// 添加系统通知并记录锚点位置。
    pub fn push_system_note(&mut self, content: String) {
        let anchor = self.view_messages.len();
        let vm = MessageViewModel::system(content);
        self.ephemeral_notes.push((anchor, vm.clone()));
        self.view_messages.push(vm);
    }
}
