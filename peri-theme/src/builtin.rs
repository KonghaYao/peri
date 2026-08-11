//! 内置主题：peri-dark、peri-light。
//!
//! 色值以 DEFAULT_THEME 为准，统一 DarkTheme 的差异色值。

use ratatui::style::Color;

use crate::component::{
    ComponentTokens, InputTokens, MarkdownTokens, MessageTokens, PanelTokens, PopupTokens,
    ScrollbarTokens, StatusBarTokens,
};
use crate::palette::{BasePalette, DiffPalette, GrayPalette, Palette, StatePalette};
use crate::semantic::{
    AccentTokens, BorderTokens, DiffTokens, SemanticTokens, StatusTokens, SurfaceTokens,
    SyntaxTokens, TextTokens,
};
use crate::theme::{ThemeDefinition, ThemeMode};

// ── 强调色（单一主色）────────────────────────────────────────────────────────
const ACCENT: Color = Color::Rgb(215, 119, 87); // #D77757 Claude 暖橙
const SAGE: Color = Color::Rgb(78, 186, 101); // #4EBA65 成功/工具名
const WARNING: Color = Color::Rgb(255, 193, 7); // #FFC107 警告
const ERROR: Color = Color::Rgb(255, 107, 128); // #FF6B80 错误
const THINKING: Color = Color::Rgb(162, 169, 228); // #A2A9E4 推理/思考

// ── 文字层级 ─────────────────────────────────────────────────────────────────
const TEXT: Color = Color::Rgb(255, 255, 255); // #FFFFFF
const MUTED: Color = Color::Rgb(153, 153, 153); // #999999
const DIM: Color = Color::Rgb(80, 80, 80); // #505050

// ── 边框 ─────────────────────────────────────────────────────────────────────
const BORDER: Color = Color::Rgb(80, 80, 80); // #505050
const BORDER_DIM: Color = Color::Rgb(42, 42, 48); // #2A2A30
const BORDER_ACTIVE: Color = ACCENT;

// ── 弹窗专用 ─────────────────────────────────────────────────────────────────
const POPUP_BG: Color = Color::Rgb(0, 0, 0);
const CURSOR_BG: Color = Color::Rgb(38, 38, 38); // #262626
const LOADING: Color = Color::Rgb(147, 165, 255); // #93A5FF
const USER_BG: Color = Color::Rgb(55, 55, 55); // #373737
const SELECTION_BG: Color = Color::Rgb(38, 79, 120); // #264F78
const SELECTED_FG: Color = Color::Rgb(178, 185, 249); // #B2B9F9
const TOOL_NAME: Color = SAGE;

// ── 特殊语义 ─────────────────────────────────────────────────────────────────
const MODEL_INFO: Color = Color::Rgb(160, 130, 95); // #A0825F
const BASH_BORDER: Color = Color::Rgb(253, 93, 177); // #FD5DB1

// ── Model Panel 档位语义 ─────────────────────────────────────────────────────
const MODEL_ACCENT: Color = Color::Rgb(162, 169, 228); // #A2A9E4 模型名内嵌 effort 后缀
const EFFORT: Color = Color::Rgb(229, 164, 107); // #E5A46B effort 档位值
const TOKEN_CONTEXT: Color = Color::Rgb(127, 181, 217); // #7FB5D9 200k/1m 标识
// light 主题可读性变体
const LIGHT_MODEL_ACCENT: Color = Color::Rgb(107, 114, 201); // #6B72C9
const LIGHT_EFFORT: Color = Color::Rgb(176, 111, 46); // #B06F2E
const LIGHT_TOKEN_CONTEXT: Color = Color::Rgb(62, 138, 184); // #3E8AB8

// ── 消息流语义（§4 默认主题表，dark：Tokyo Night 方向）────────────────────
const RUNNING: Color = Color::Rgb(125, 207, 255); // #7DCFFF 活动状态
const ACCENT_USER: Color = Color::Rgb(122, 162, 247); // #7AA2F7 用户 prompt
const ACCENT_ASSISTANT: Color = Color::Rgb(187, 154, 247); // #BB9AF7 assistant 回答
const ACCENT_REASONING: Color = Color::Rgb(84, 92, 126); // #545C7E reasoning
const ACCENT_TOOL: Color = Color::Rgb(115, 122, 162); // #737AA2 已完成的 tool
const TEXT_SECONDARY: Color = Color::Rgb(169, 177, 214); // #A9B1D6 次级正文
const SURFACE_RAISED: Color = Color::Rgb(41, 46, 66); // #292E42 composer / expanded tool body
const SURFACE_SUNKEN: Color = Color::Rgb(26, 27, 38); // #1A1B26 code / terminal output
const SYNTAX_COMMAND: Color = Color::Rgb(224, 175, 104); // #E0AF68 shell command
const SYNTAX_PATH: Color = Color::Rgb(255, 158, 100); // #FF9E64 文件路径

// ── 消息流语义（light 可读等值：同色相加深，保证浅底可读）────────────────
const LIGHT_RUNNING: Color = Color::Rgb(30, 136, 200); // #1E88C8
const LIGHT_ACCENT_USER: Color = Color::Rgb(46, 125, 224); // #2E7DE0
const LIGHT_ACCENT_ASSISTANT: Color = Color::Rgb(122, 93, 199); // #7A5DC7
const LIGHT_ACCENT_REASONING: Color = Color::Rgb(138, 147, 168); // #8A93A8
const LIGHT_ACCENT_TOOL: Color = Color::Rgb(110, 118, 134); // #6E7686
const LIGHT_TEXT_SECONDARY: Color = Color::Rgb(74, 74, 74); // #4A4A4A
const LIGHT_SURFACE_RAISED: Color = Color::Rgb(255, 255, 255); // #FFFFFF
const LIGHT_SURFACE_SUNKEN: Color = Color::Rgb(233, 233, 237); // #E9E9ED
const LIGHT_SYNTAX_COMMAND: Color = Color::Rgb(154, 107, 0); // #9A6B00
const LIGHT_SYNTAX_PATH: Color = Color::Rgb(177, 92, 0); // #B15C00

/// 构建 peri-dark 完整主题定义。
pub fn dark_theme() -> ThemeDefinition {
    ThemeDefinition {
        name: "peri-dark".into(),
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
            accents: AccentTokens {
                primary: ACCENT,
                user: ACCENT_USER,
                assistant: ACCENT_ASSISTANT,
                reasoning: ACCENT_REASONING,
                tool: ACCENT_TOOL,
            },
            text: TextTokens {
                primary: TEXT,
                secondary: TEXT_SECONDARY,
                muted: MUTED,
                dim: DIM,
            },
            border: BorderTokens {
                default: BORDER,
                active: BORDER_ACTIVE,
                dim: BORDER_DIM,
            },
            status: StatusTokens {
                running: RUNNING,
                success: SAGE,
                warning: WARNING,
                error: ERROR,
            },
            surface: SurfaceTokens {
                default: Color::Rgb(0, 0, 0),
                raised: SURFACE_RAISED,
                sunken: SURFACE_SUNKEN,
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
            syntax: SyntaxTokens {
                command: SYNTAX_COMMAND,
                path: SYNTAX_PATH,
            },
            loading: LOADING,
            thinking: THINKING,
            accent: ACCENT,
            model_info: MODEL_INFO,
            model_accent: MODEL_ACCENT,
            effort: EFFORT,
            token_context: TOKEN_CONTEXT,
            bash_border: BASH_BORDER,
            selected_fg: SELECTED_FG,
        },
        component: ComponentTokens {
            message: MessageTokens {
                user_bg: USER_BG,
                ai_prefix: TEXT,
                tool_indicator: TOOL_NAME,
                reasoning: THINKING,
            },
            input: InputTokens {
                border: MUTED,
                border_loading: MUTED,
                cursor_fg: POPUP_BG,
                cursor_bg: TEXT,
                prompt: MUTED,
                prompt_loading: MUTED,
                continuation: DIM,
                placeholder: MUTED,
                // 深色底色板：配白色前景（可读性由 readable_fg 按亮度决定）
                session_title_palette: [
                    Color::Rgb(18, 52, 26),  // 深绿
                    Color::Rgb(55, 20, 18),  // 深红
                    Color::Rgb(26, 78, 36),  // 草绿
                    Color::Rgb(78, 28, 22),  // 砖红
                    Color::Rgb(38, 79, 120), // 深蓝
                    Color::Rgb(80, 60, 30),  // 深橙棕
                    Color::Rgb(60, 40, 80),  // 深紫
                    Color::Rgb(30, 60, 80),  // 深青
                ],
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
    }
}

/// 构建 peri-light 完整主题定义（浅色背景为基础）。
pub fn light_theme() -> ThemeDefinition {
    // Light 主题使用浅色背景、深色文字，保留品牌色基调。
    let light_bg = Color::Rgb(250, 250, 250); // #FAFAFA 暖白背景
    let light_text = Color::Rgb(46, 46, 42); // #2E2E2A 柔和深灰
    let light_muted = Color::Rgb(120, 120, 120); // #787878
    let light_dim = Color::Rgb(200, 200, 200); // #C8C8C8
    let light_border = Color::Rgb(180, 180, 180); // #B4B4B4
    let light_border_dim = Color::Rgb(225, 225, 230); // #E1E1E6
    let light_surface = Color::Rgb(245, 245, 248); // #F5F5F8
    let light_user_bg = Color::Rgb(230, 230, 235); // #E6E6EB
    let light_selection = Color::Rgb(200, 220, 245); // #C8DCF5
    let light_cursor = Color::Rgb(220, 220, 225); // #DCDCE1

    ThemeDefinition {
        name: "peri-light".into(),
        mode: ThemeMode::Light,
        palette: Palette {
            base: BasePalette {
                bg: light_bg,
                fg: light_text,
            },
            brand: StatePalette { primary: ACCENT },
            gray: GrayPalette {
                bright: light_text,
                muted: light_muted,
                dim: light_dim,
                dark: light_border_dim,
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
                add_bg: Color::Rgb(220, 245, 220),
                remove_bg: Color::Rgb(245, 220, 220),
                add_word_bg: Color::Rgb(200, 235, 200),
                remove_word_bg: Color::Rgb(235, 200, 200),
            },
        },
        semantic: SemanticTokens {
            accents: AccentTokens {
                primary: ACCENT,
                user: LIGHT_ACCENT_USER,
                assistant: LIGHT_ACCENT_ASSISTANT,
                reasoning: LIGHT_ACCENT_REASONING,
                tool: LIGHT_ACCENT_TOOL,
            },
            text: TextTokens {
                primary: light_text,
                secondary: LIGHT_TEXT_SECONDARY,
                muted: light_muted,
                dim: light_dim,
            },
            border: BorderTokens {
                default: light_border,
                active: ACCENT,
                dim: light_border_dim,
            },
            status: StatusTokens {
                running: LIGHT_RUNNING,
                success: SAGE,
                warning: WARNING,
                error: ERROR,
            },
            surface: SurfaceTokens {
                default: light_surface,
                raised: LIGHT_SURFACE_RAISED,
                sunken: LIGHT_SURFACE_SUNKEN,
                user: light_user_bg,
                popup: light_bg,
                selection: light_selection,
                cursor: light_cursor,
            },
            diff: DiffTokens {
                add: SAGE,
                remove: ERROR,
                hunk: THINKING,
                add_bg: Color::Rgb(220, 245, 220),
                remove_bg: Color::Rgb(245, 220, 220),
                add_word_bg: Color::Rgb(200, 235, 200),
                remove_word_bg: Color::Rgb(235, 200, 200),
            },
            syntax: SyntaxTokens {
                command: LIGHT_SYNTAX_COMMAND,
                path: LIGHT_SYNTAX_PATH,
            },
            loading: LOADING,
            thinking: THINKING,
            accent: ACCENT,
            model_info: MODEL_INFO,
            model_accent: LIGHT_MODEL_ACCENT,
            effort: LIGHT_EFFORT,
            token_context: LIGHT_TOKEN_CONTEXT,
            bash_border: BASH_BORDER,
            selected_fg: SELECTED_FG,
        },
        component: ComponentTokens {
            message: MessageTokens {
                user_bg: light_user_bg,
                ai_prefix: light_text,
                tool_indicator: TOOL_NAME,
                reasoning: THINKING,
            },
            input: InputTokens {
                border: light_muted,
                border_loading: light_muted,
                cursor_fg: light_bg,
                cursor_bg: light_text,
                prompt: light_muted,
                prompt_loading: light_muted,
                continuation: light_dim,
                placeholder: light_muted,
                // 浅色底色板：配黑色前景（可读性由 readable_fg 按亮度决定）
                session_title_palette: [
                    Color::Rgb(210, 230, 215), // 浅绿
                    Color::Rgb(235, 210, 210), // 浅红
                    Color::Rgb(200, 230, 210), // 嫩绿
                    Color::Rgb(240, 215, 205), // 浅橙
                    Color::Rgb(205, 220, 240), // 浅蓝
                    Color::Rgb(235, 220, 200), // 浅黄
                    Color::Rgb(220, 205, 235), // 浅紫
                    Color::Rgb(200, 225, 235), // 浅青
                ],
            },
            panel: PanelTokens {
                border: ACCENT,
                title: light_text,
                row_selected: SELECTED_FG,
                min_height: 8,
                max_height: 28,
            },
            popup: PopupTokens {
                bg: light_bg,
                border: THINKING,
                action_primary: ACCENT,
                selected_fg: SELECTED_FG,
                modal_max_width: 90,
                modal_max_height: 28,
                inline_height: 10,
            },
            statusbar: StatusBarTokens {
                text: light_text,
                muted: light_muted,
                dim: light_dim,
                mode_accept_edit: THINKING,
                mode_auto: WARNING,
                mode_bypass: ERROR,
                resource_good: SAGE,
                resource_warn: WARNING,
                resource_bad: ERROR,
            },
            markdown: MarkdownTokens {
                text: light_text,
                code: WARNING,
                quote: light_muted,
            },
            scrollbar: ScrollbarTokens {
                thumb: light_muted,
                track: light_dim,
            },
        },
    }
}
