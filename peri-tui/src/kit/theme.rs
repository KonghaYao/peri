//! TUI 统一颜色主题（对齐 Claude Code Dark 配色方案）。
//!
//! S11 起类型定义集中在 kit 内，由 kit 组件直接引用。
//!
//! ## 设计哲学
//!
//! 中性灰层级 + Claude 暖橙品牌色。背景透明——不使用任何 bg() 颜色
//! （弹窗光标行和用户消息区除外）。信息层级用亮度区分（TEXT/MUTED/DIM），
//! 颜色表达状态语义。

use ratatui::style::Color;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeMode {
    Dark,
    Light,
    HighContrast,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThemeDefinition {
    pub name: &'static str,
    pub mode: ThemeMode,
    pub palette: Palette,
    pub semantic: SemanticTokens,
    pub component: ComponentTokens,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Palette {
    pub base: BasePalette,
    pub brand: StatePalette,
    pub gray: GrayPalette,
    pub accent: StatePalette,
    pub success: StatePalette,
    pub warning: StatePalette,
    pub danger: StatePalette,
    pub info: StatePalette,
    pub diff: DiffPalette,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BasePalette {
    pub bg: Color,
    pub fg: Color,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GrayPalette {
    pub bright: Color,
    pub muted: Color,
    pub dim: Color,
    pub dark: Color,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatePalette {
    pub primary: Color,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiffPalette {
    pub add: Color,
    pub remove: Color,
    pub hunk: Color,
    pub add_bg: Color,
    pub remove_bg: Color,
    pub add_word_bg: Color,
    pub remove_word_bg: Color,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SemanticTokens {
    pub accent: Color,
    pub text: TextTokens,
    pub border: BorderTokens,
    pub status: StatusTokens,
    pub surface: SurfaceTokens,
    pub diff: DiffTokens,
    pub loading: Color,
    pub thinking: Color,
    pub model_info: Color,
    pub bash_border: Color,
    pub selected_fg: Color,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextTokens {
    pub primary: Color,
    pub muted: Color,
    pub dim: Color,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BorderTokens {
    pub default: Color,
    pub active: Color,
    pub dim: Color,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatusTokens {
    pub running: Color,
    pub success: Color,
    pub warning: Color,
    pub error: Color,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SurfaceTokens {
    pub default: Color,
    pub user: Color,
    pub popup: Color,
    pub selection: Color,
    pub cursor: Color,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiffTokens {
    pub add: Color,
    pub remove: Color,
    pub hunk: Color,
    pub add_bg: Color,
    pub remove_bg: Color,
    pub add_word_bg: Color,
    pub remove_word_bg: Color,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ComponentTokens {
    pub message: MessageTokens,
    pub input: InputTokens,
    pub panel: PanelTokens,
    pub popup: PopupTokens,
    pub statusbar: StatusBarTokens,
    pub markdown: MarkdownTokens,
    pub scrollbar: ScrollbarTokens,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MessageTokens {
    pub user_bg: Color,
    pub ai_prefix: Color,
    pub tool_indicator: Color,
    pub reasoning: Color,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InputTokens {
    pub border: Color,
    pub border_loading: Color,
    pub cursor_fg: Color,
    pub cursor_bg: Color,
    pub prompt: Color,
    pub prompt_loading: Color,
    pub continuation: Color,
    pub placeholder: Color,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PanelTokens {
    pub border: Color,
    pub title: Color,
    pub row_selected: Color,
    pub min_height: u16,
    pub max_height: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PopupTokens {
    pub bg: Color,
    pub border: Color,
    pub action_primary: Color,
    pub selected_fg: Color,
    pub modal_max_width: u16,
    pub modal_max_height: u16,
    pub inline_height: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatusBarTokens {
    pub text: Color,
    pub muted: Color,
    pub dim: Color,
    pub mode_accept_edit: Color,
    pub mode_auto: Color,
    pub mode_bypass: Color,
    pub resource_good: Color,
    pub resource_warn: Color,
    pub resource_bad: Color,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MarkdownTokens {
    pub text: Color,
    pub code: Color,
    pub quote: Color,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScrollbarTokens {
    pub thumb: Color,
    pub track: Color,
}

pub const DEFAULT_THEME: ThemeDefinition = ThemeDefinition {
    name: "peri-dark",
    mode: ThemeMode::Dark,
    palette: Palette {
        base: BasePalette {
            bg: Color::Rgb(0, 0, 0),
            fg: TEXT,
        },
        brand: StatePalette { primary: ACCENT },
        gray: GrayPalette {
            bright: TEXT,
            muted: MUTED,
            dim: DIM,
            dark: BORDER_DIM,
        },
        accent: StatePalette { primary: ACCENT },
        success: StatePalette { primary: SAGE },
        warning: StatePalette { primary: WARNING },
        danger: StatePalette { primary: ERROR },
        info: StatePalette { primary: THINKING },
        diff: DiffPalette {
            add: SAGE,
            remove: ERROR,
            hunk: THINKING,
            add_bg: Color::Rgb(18, 52, 26),
            remove_bg: Color::Rgb(55, 20, 18),
            add_word_bg: Color::Rgb(26, 78, 36),
            remove_word_bg: Color::Rgb(78, 28, 22),
        },
    },
    semantic: SemanticTokens {
        text: TextTokens {
            primary: TEXT,
            muted: MUTED,
            dim: DIM,
        },
        border: BorderTokens {
            default: BORDER,
            active: BORDER_ACTIVE,
            dim: BORDER_DIM,
        },
        status: StatusTokens {
            running: LOADING,
            success: SAGE,
            warning: WARNING,
            error: ERROR,
        },
        surface: SurfaceTokens {
            default: Color::Rgb(0, 0, 0),
            user: USER_BG,
            popup: POPUP_BG,
            selection: SELECTION_BG,
            cursor: CURSOR_BG,
        },
        diff: DiffTokens {
            add: SAGE,
            remove: ERROR,
            hunk: THINKING,
            add_bg: Color::Rgb(18, 52, 26),
            remove_bg: Color::Rgb(55, 20, 18),
            add_word_bg: Color::Rgb(26, 78, 36),
            remove_word_bg: Color::Rgb(78, 28, 22),
        },
        loading: LOADING,
        thinking: THINKING,
        accent: ACCENT,
        model_info: Color::Rgb(160, 130, 95),
        bash_border: Color::Rgb(253, 93, 177),
        selected_fg: Color::Rgb(178, 185, 249),
    },
    component: ComponentTokens {
        message: MessageTokens {
            user_bg: USER_BG,
            ai_prefix: TEXT,
            tool_indicator: TOOL_NAME,
            reasoning: THINKING,
        },
        input: InputTokens {
            border: BORDER_ACTIVE,
            border_loading: MUTED,
            cursor_fg: POPUP_BG,
            cursor_bg: TEXT,
            prompt: ACCENT,
            prompt_loading: MUTED,
            continuation: DIM,
            placeholder: MUTED,
        },
        panel: PanelTokens {
            border: BORDER_ACTIVE,
            title: TEXT,
            row_selected: SELECTED_FG,
            min_height: 8,
            max_height: 28,
        },
        popup: PopupTokens {
            bg: POPUP_BG,
            border: THINKING,
            action_primary: ACCENT,
            selected_fg: SELECTED_FG,
            modal_max_width: 90,
            modal_max_height: 28,
            inline_height: 10,
        },
        statusbar: StatusBarTokens {
            text: TEXT,
            muted: MUTED,
            dim: DIM,
            mode_accept_edit: THINKING,
            mode_auto: WARNING,
            mode_bypass: ERROR,
            resource_good: SAGE,
            resource_warn: WARNING,
            resource_bad: ERROR,
        },
        markdown: MarkdownTokens {
            text: TEXT,
            code: WARNING,
            quote: MUTED,
        },
        scrollbar: ScrollbarTokens {
            thumb: MUTED,
            track: DIM,
        },
    },
};

pub fn current() -> &'static ThemeDefinition {
    &DEFAULT_THEME
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_theme_maps_legacy_colors_to_tokens() {
        let theme = current();
        assert_eq!(theme.name, "peri-dark");
        assert_eq!(theme.component.input.border, BORDER_ACTIVE);
        assert_eq!(theme.component.statusbar.resource_good, SAGE);
        assert_eq!(theme.semantic.status.error, ERROR);
        assert_eq!(theme.component.panel.min_height, 8);
        assert_eq!(theme.component.panel.max_height, 28);
        assert_eq!(theme.component.popup.modal_max_width, 90);
        assert_eq!(theme.component.popup.modal_max_height, 28);
        assert_eq!(theme.component.popup.inline_height, 10);
    }
}

pub fn semantic() -> &'static SemanticTokens {
    &DEFAULT_THEME.semantic
}

pub fn component() -> &'static ComponentTokens {
    &DEFAULT_THEME.component
}

// ── 强调色（单一主色）────────────────────────────────────────────────────────

/// Claude 暖橙 — 唯一主交互色，品牌色 #D77757
const ACCENT: Color = Color::Rgb(215, 119, 87);

// ── 功能色 ───────────────────────────────────────────────────────────────────

/// 明亮绿 — 成功/工具名/在线状态 #4EBA65
const SAGE: Color = Color::Rgb(78, 186, 101);

/// 明亮琥珀 — 次要强调/警告 #FFC107
const WARNING: Color = Color::Rgb(255, 193, 7);

/// 明亮红 — 错误/拒绝 #FF6B80
const ERROR: Color = Color::Rgb(255, 107, 128);

/// 标准紫 — 推理/CoT 思考内容 #A2A9E4
const THINKING: Color = Color::Rgb(162, 169, 228);

// ── 文字层级（三级亮度）──────────────────────────────────────────────────────

/// 纯白 — 主文字 #FFFFFF
const TEXT: Color = Color::Rgb(255, 255, 255);

/// 浅灰 — 标签/路径/辅助信息 #999999
const MUTED: Color = Color::Rgb(153, 153, 153);

/// 深灰 — 占位/已完成项/分隔符 #505050
const DIM: Color = Color::Rgb(80, 80, 80);

// ── 边框 ─────────────────────────────────────────────────────────────────────

/// 中性灰 — 空闲边框 #505050
const BORDER: Color = Color::Rgb(80, 80, 80);

/// 暗灰 — 非活跃 session 分隔线 #2A2A30
const BORDER_DIM: Color = Color::Rgb(42, 42, 48);

/// 激活边框 — 输入框/当前 panel focus 状态
const BORDER_ACTIVE: Color = ACCENT;

// ── 弹窗专用 ─────────────────────────────────────────────────────────────────

/// 纯黑 — 弹窗底色 #000000
const POPUP_BG: Color = Color::Rgb(0, 0, 0);

/// 中性暗灰 — 光标行背景（列表选中行）#262626
const CURSOR_BG: Color = Color::Rgb(38, 38, 38);

/// 浅蓝紫 — Loading/Spinner 专用 #93A5FF
const LOADING: Color = Color::Rgb(147, 165, 255);

/// 用户消息背景色 #373737（Claude userMessageBackground）
const USER_BG: Color = Color::Rgb(55, 55, 55);

/// 文本选区背景色 #264f78（深色主题下网页默认选中蓝的暗色版本）
const SELECTION_BG: Color = Color::Rgb(38, 79, 120);

/// 选中行前景色（列表高亮文字，蓝紫色系）#B2B9F9
const SELECTED_FG: Color = Color::Rgb(178, 185, 249);

// ── 语义别名 ─────────────────────────────────────────────────────────────────

/// 工具名颜色（= SAGE）
const TOOL_NAME: Color = SAGE;
