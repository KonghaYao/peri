use peri_widgets::ScrollbarMetrics;
use tui_textarea::TextArea;

use super::at_mention::AtMentionState;
use crate::app::text_selection::{PanelTextSelection, TextSelection};

/// 预测输入状态：agent 完成后 LLM 生成的下一步输入建议。
pub struct PredictionState {
    pub text: String,
    pub received_at: std::time::Instant,
}

/// UI 交互状态：会话级的输入、滚动、选区、历史等。
pub struct UiState {
    pub textarea: TextArea<'static>,
    pub loading: bool,
    pub scroll_offset: u16,
    pub scroll_follow: bool,
    pub show_tool_messages: bool,
    pub input_history: Vec<String>,
    pub history_index: Option<usize>,
    pub draft_input: Option<String>,
    pub text_selection: TextSelection,
    pub messages_area: Option<ratatui::layout::Rect>,
    pub textarea_area: Option<ratatui::layout::Rect>,
    pub copy_message_until: Option<std::time::Instant>,
    pub copy_char_count: usize,
    pub panel_selection: PanelTextSelection,
    pub panel_area: Option<ratatui::layout::Rect>,
    pub panel_plain_lines: Vec<String>,
    pub panel_scroll_offset: u16,
    /// 用户是否正在拖拽消息区域右侧滚动条
    pub scrollbar_dragging: bool,
    /// 消息区域滚动条的最大偏移量（内容高度 - 可见高度）
    pub scrollbar_max_offset: u16,
    /// 滚动条拖拽起始时的鼠标 Y 坐标
    pub scrollbar_drag_start_y: u16,
    /// 滚动条拖拽起始时的 scroll offset
    pub scrollbar_drag_start_offset: u16,
    /// Panel scrollbar geometry for mouse interaction
    pub panel_scrollbar_metrics: Option<ScrollbarMetrics>,
    /// Whether user is currently dragging the panel scrollbar
    pub panel_scrollbar_dragging: bool,
    /// @ 文件提及状态
    pub at_mention: AtMentionState,
    /// 后台 Agent Bar 光标位置
    pub bg_bar_cursor: Option<usize>,
    /// 后台 Agent Bar 渲染区域（用于鼠标点击检测）
    pub bg_bar_area: Option<ratatui::layout::Rect>,
    /// Write/Edit 工具结果内联 diff 是否可见
    pub diff_visible: bool,
    /// Rewind 完成后待回填到输入框的用户消息文本
    pub pending_rewind_text: Option<String>,
    /// 预测输入建议（灰色 placeholder，Tab 接受）
    pub prediction: Option<PredictionState>,
}

impl UiState {
    pub fn new(textarea: TextArea<'static>, cwd: &str, diff_enabled: bool) -> Self {
        let _ = cwd; // 历史路径已迁移至 ~/.peri/，cwd 保留用于未来扩展
        let input_history = super::history_persistence::load_input_history();
        Self {
            textarea,
            loading: false,
            scroll_offset: u16::MAX,
            scroll_follow: true,
            show_tool_messages: false,
            input_history,
            history_index: None,
            draft_input: None,
            text_selection: TextSelection::new(),
            messages_area: None,
            textarea_area: None,
            copy_message_until: None,
            copy_char_count: 0,
            panel_selection: PanelTextSelection::new(),
            panel_area: None,
            panel_plain_lines: Vec::new(),
            panel_scroll_offset: 0,
            scrollbar_dragging: false,
            scrollbar_max_offset: 0,
            scrollbar_drag_start_y: 0,
            scrollbar_drag_start_offset: 0,
            panel_scrollbar_metrics: None,
            panel_scrollbar_dragging: false,
            at_mention: AtMentionState::new(),
            bg_bar_cursor: None,
            bg_bar_area: None,
            diff_visible: diff_enabled,
            pending_rewind_text: None,
            prediction: None,
        }
    }
}
