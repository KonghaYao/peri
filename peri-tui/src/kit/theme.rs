//! TUI 统一颜色主题（对齐 Claude Code Dark 配色方案）。
//!
//! S11 起类型定义集中在 kit 内，`ui::theme` 通过 re-export 保持 legacy 兼容。
//!
//! ## 设计哲学
//!
//! 中性灰层级 + Claude 暖橙品牌色。背景透明——不使用任何 bg() 颜色
//! （弹窗光标行和用户消息区除外）。信息层级用亮度区分（TEXT/MUTED/DIM），
//! 颜色表达状态语义。

use ratatui::style::Color;

// ── 强调色（单一主色）────────────────────────────────────────────────────────

/// Claude 暖橙 — 唯一主交互色，品牌色 #D77757
pub const ACCENT: Color = Color::Rgb(215, 119, 87);

// ── 功能色 ───────────────────────────────────────────────────────────────────

/// 明亮绿 — 成功/工具名/在线状态 #4EBA65
pub const SAGE: Color = Color::Rgb(78, 186, 101);

/// 明亮琥珀 — 次要强调/警告 #FFC107
pub const WARNING: Color = Color::Rgb(255, 193, 7);

/// 明亮红 — 错误/拒绝 #FF6B80
pub const ERROR: Color = Color::Rgb(255, 107, 128);

/// 标准紫 — 推理/CoT 思考内容 #A2A9E4
pub const THINKING: Color = Color::Rgb(162, 169, 228);

// ── 文字层级（三级亮度）──────────────────────────────────────────────────────

/// 纯白 — 主文字 #FFFFFF
pub const TEXT: Color = Color::Rgb(255, 255, 255);

/// 浅灰 — 标签/路径/辅助信息 #999999
pub const MUTED: Color = Color::Rgb(153, 153, 153);

/// 深灰 — 占位/已完成项/分隔符 #505050
pub const DIM: Color = Color::Rgb(80, 80, 80);

// ── 边框 ─────────────────────────────────────────────────────────────────────

/// 中性灰 — 空闲边框 #505050
pub const BORDER: Color = Color::Rgb(80, 80, 80);

/// 暗灰 — 非活跃 session 分隔线 #2A2A30
pub const BORDER_DIM: Color = Color::Rgb(42, 42, 48);

/// 激活边框 — 输入框/当前 panel focus 状态
pub const BORDER_ACTIVE: Color = ACCENT;

// ── 弹窗专用 ─────────────────────────────────────────────────────────────────

/// 纯黑 — 弹窗底色 #000000
pub const POPUP_BG: Color = Color::Rgb(0, 0, 0);

/// 中性暗灰 — 光标行背景（列表选中行）#262626
pub const CURSOR_BG: Color = Color::Rgb(38, 38, 38);

/// 浅蓝紫 — Loading/Spinner 专用 #93A5FF
pub const LOADING: Color = Color::Rgb(147, 165, 255);

/// 用户消息背景色 #373737（Claude userMessageBackground）
pub const USER_BG: Color = Color::Rgb(55, 55, 55);

/// 文本选区背景色 #264f78（深色主题下网页默认选中蓝的暗色版本）
pub const SELECTION_BG: Color = Color::Rgb(38, 79, 120);

/// 选中行前景色（列表高亮文字，蓝紫色系）#B2B9F9
pub const SELECTED_FG: Color = Color::Rgb(178, 185, 249);

/// Bash 工具调用边框色 #FD5DB1（Claude bashBorder）
pub const BASH_BORDER: Color = Color::Rgb(253, 93, 177);

// ── 语义别名 ─────────────────────────────────────────────────────────────────

/// 工具名颜色（= SAGE）
pub const TOOL_NAME: Color = SAGE;

/// SubAgent 颜色（= SAGE）
pub const SUB_AGENT: Color = SAGE;

/// 模型信息颜色 — 棕金，对应 #A0825F（状态栏模型名，不抢眼）
pub const MODEL_INFO: Color = Color::Rgb(160, 130, 95);
