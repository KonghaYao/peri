//! 扁平、已解析的主题键转换为强类型 token；保留旧主题缺省字段的兼容值。

use std::collections::HashMap;

use ratatui::style::Color;

use super::ThemeLoadError;
use super::resolve::{parse_hex_color, resolve_refs};
use crate::theme::{ThemeDefinition, ThemeMode};

pub(super) fn decode_flat(
    mut flat: HashMap<String, String>,
) -> Result<ThemeDefinition, ThemeLoadError> {
    let name = flat.remove("name").unwrap_or_else(|| "unnamed".to_string());
    let mode = flat.remove("mode").unwrap_or_else(|| "dark".to_string());
    let mode = match mode.as_str() {
        "dark" => ThemeMode::Dark,
        "light" => ThemeMode::Light,
        "highcontrast" | "high_contrast" | "high-contrast" => ThemeMode::HighContrast,
        _ => {
            return Err(ThemeLoadError::InvalidColor(format!(
                "unknown mode: {mode}"
            )));
        }
    };
    build_theme_from_flat(&name, mode, &resolve_refs(&flat, 0)?)
}

/// 解析会话标题底色板。逐项读取 `component.input.session_title_palette.{i}`，
/// 缺省或非法时回退到内置 dark 主题的默认色板——保证旧版用户主题
/// （无该字段）也能加载，且缺失项不阻断整个主题。
fn build_session_title_palette(
    flat: &HashMap<String, String>,
) -> Result<[Color; 8], ThemeLoadError> {
    let defaults = crate::builtin::dark_theme()
        .component
        .input
        .session_title_palette;
    let mut palette = [Color::Rgb(0, 0, 0); 8];
    for i in 0..8 {
        palette[i] = match flat.get(&format!("component.input.session_title_palette.{i}")) {
            Some(val) => parse_hex_color(val).unwrap_or(defaults[i]),
            None => defaults[i],
        };
    }
    Ok(palette)
}

/// 从展开的扁平 map 构建 ThemeDefinition。
fn build_theme_from_flat(
    name: &str,
    mode: ThemeMode,
    flat: &HashMap<String, String>,
) -> Result<ThemeDefinition, ThemeLoadError> {
    use crate::component::*;
    use crate::palette::*;
    use crate::semantic::*;

    let get_color = |key: &str| -> Result<Color, ThemeLoadError> {
        let val = flat
            .get(key)
            .ok_or_else(|| ThemeLoadError::MissingField(key.to_string()))?;
        parse_hex_color(val)
    };

    let get_u16 = |key: &str| -> Result<u16, ThemeLoadError> {
        let val = flat
            .get(key)
            .ok_or_else(|| ThemeLoadError::MissingField(key.to_string()))?;
        val.parse::<u16>()
            .map_err(|e| ThemeLoadError::InvalidColor(format!("invalid u16 for {key}: {e}")))
    };

    // 可选色：旧版用户主题可能缺 Model Panel 新语义键，缺失时回退默认值
    let get_color_opt = |key: &str| -> Result<Option<Color>, ThemeLoadError> {
        match flat.get(key) {
            Some(val) => parse_hex_color(val).map(Some),
            None => Ok(None),
        }
    };
    let model_accent_default = Color::Rgb(162, 169, 228); // #A2A9E4
    let effort_default = Color::Rgb(229, 164, 107); // #E5A46B
    let token_context_default = Color::Rgb(127, 181, 217); // #7FB5D9

    let palette = Palette {
        base: BasePalette {
            bg: get_color("palette.base.bg")?,
            fg: get_color("palette.base.fg")?,
        },
        brand: StatePalette {
            primary: get_color("palette.brand.primary")?,
        },
        gray: GrayPalette {
            bright: get_color("palette.gray.bright")?,
            muted: get_color("palette.gray.muted")?,
            dim: get_color("palette.gray.dim")?,
            dark: get_color("palette.gray.dark")?,
        },
        accent: StatePalette {
            primary: get_color("palette.accent.primary")?,
        },
        success: StatePalette {
            primary: get_color("palette.success.primary")?,
        },
        warning: StatePalette {
            primary: get_color("palette.warning.primary")?,
        },
        danger: StatePalette {
            primary: get_color("palette.danger.primary")?,
        },
        info: StatePalette {
            primary: get_color("palette.info.primary")?,
        },
        diff: DiffPalette {
            add: get_color("palette.diff.add")?,
            remove: get_color("palette.diff.remove")?,
            hunk: get_color("palette.diff.hunk")?,
            add_bg: get_color("palette.diff.add_bg")?,
            remove_bg: get_color("palette.diff.remove_bg")?,
            add_word_bg: get_color("palette.diff.add_word_bg")?,
            remove_word_bg: get_color("palette.diff.remove_word_bg")?,
        },
    };

    let semantic = SemanticTokens {
        accent: get_color("semantic.accent")?,
        accents: {
            // 旧版用户主题可能缺消息流新语义键，缺失时回退内置 dark 默认值
            let defaults = crate::builtin::dark_theme().semantic.accents;
            AccentTokens {
                primary: get_color_opt("semantic.accents.primary")?.unwrap_or(defaults.primary),
                user: get_color_opt("semantic.accents.user")?.unwrap_or(defaults.user),
                assistant: get_color_opt("semantic.accents.assistant")?
                    .unwrap_or(defaults.assistant),
                reasoning: get_color_opt("semantic.accents.reasoning")?
                    .unwrap_or(defaults.reasoning),
                tool: get_color_opt("semantic.accents.tool")?.unwrap_or(defaults.tool),
            }
        },
        text: TextTokens {
            primary: get_color("semantic.text.primary")?,
            secondary: get_color_opt("semantic.text.secondary")?
                .unwrap_or(crate::builtin::dark_theme().semantic.text.secondary),
            muted: get_color("semantic.text.muted")?,
            dim: get_color("semantic.text.dim")?,
        },
        border: BorderTokens {
            default: get_color("semantic.border.default")?,
            active: get_color("semantic.border.active")?,
            dim: get_color("semantic.border.dim")?,
        },
        status: StatusTokens {
            running: get_color("semantic.status.running")?,
            success: get_color("semantic.status.success")?,
            warning: get_color("semantic.status.warning")?,
            error: get_color("semantic.status.error")?,
        },
        surface: SurfaceTokens {
            default: get_color("semantic.surface.default")?,
            raised: get_color_opt("semantic.surface.raised")?
                .unwrap_or(crate::builtin::dark_theme().semantic.surface.raised),
            sunken: get_color_opt("semantic.surface.sunken")?
                .unwrap_or(crate::builtin::dark_theme().semantic.surface.sunken),
            user: get_color("semantic.surface.user")?,
            popup: get_color("semantic.surface.popup")?,
            selection: get_color("semantic.surface.selection")?,
            cursor: get_color("semantic.surface.cursor")?,
        },
        diff: DiffTokens {
            add: get_color("semantic.diff.add")?,
            remove: get_color("semantic.diff.remove")?,
            hunk: get_color("semantic.diff.hunk")?,
            add_bg: get_color("semantic.diff.add_bg")?,
            remove_bg: get_color("semantic.diff.remove_bg")?,
            add_word_bg: get_color("semantic.diff.add_word_bg")?,
            remove_word_bg: get_color("semantic.diff.remove_word_bg")?,
        },
        syntax: SyntaxTokens {
            command: get_color_opt("semantic.syntax.command")?
                .unwrap_or(crate::builtin::dark_theme().semantic.syntax.command),
            path: get_color_opt("semantic.syntax.path")?
                .unwrap_or(crate::builtin::dark_theme().semantic.syntax.path),
        },
        loading: get_color("semantic.loading")?,
        thinking: get_color("semantic.thinking")?,
        model_info: get_color("semantic.model_info")?,
        model_accent: get_color_opt("semantic.model_accent")?.unwrap_or(model_accent_default),
        effort: get_color_opt("semantic.effort")?.unwrap_or(effort_default),
        token_context: get_color_opt("semantic.token_context")?.unwrap_or(token_context_default),
        bash_border: get_color("semantic.bash_border")?,
        selected_fg: get_color("semantic.selected_fg")?,
    };

    let component = ComponentTokens {
        message: MessageTokens {
            user_bg: get_color("component.message.user_bg")?,
            ai_prefix: get_color("component.message.ai_prefix")?,
            tool_indicator: get_color("component.message.tool_indicator")?,
            reasoning: get_color("component.message.reasoning")?,
        },
        input: InputTokens {
            border: get_color("component.input.border")?,
            border_loading: get_color("component.input.border_loading")?,
            cursor_fg: get_color("component.input.cursor_fg")?,
            cursor_bg: get_color("component.input.cursor_bg")?,
            prompt: get_color("component.input.prompt")?,
            prompt_loading: get_color("component.input.prompt_loading")?,
            continuation: get_color("component.input.continuation")?,
            placeholder: get_color("component.input.placeholder")?,
            session_title_palette: build_session_title_palette(flat)?,
        },
        panel: PanelTokens {
            border: get_color("component.panel.border")?,
            title: get_color("component.panel.title")?,
            row_selected: get_color("component.panel.row_selected")?,
            min_height: get_u16("component.panel.min_height")?,
            max_height: get_u16("component.panel.max_height")?,
        },
        popup: PopupTokens {
            bg: get_color("component.popup.bg")?,
            border: get_color("component.popup.border")?,
            action_primary: get_color("component.popup.action_primary")?,
            selected_fg: get_color("component.popup.selected_fg")?,
            modal_max_width: get_u16("component.popup.modal_max_width")?,
            modal_max_height: get_u16("component.popup.modal_max_height")?,
            inline_height: get_u16("component.popup.inline_height")?,
        },
        statusbar: StatusBarTokens {
            text: get_color("component.statusbar.text")?,
            muted: get_color("component.statusbar.muted")?,
            dim: get_color("component.statusbar.dim")?,
            mode_accept_edit: get_color("component.statusbar.mode_accept_edit")?,
            mode_auto: get_color("component.statusbar.mode_auto")?,
            mode_bypass: get_color("component.statusbar.mode_bypass")?,
            resource_good: get_color("component.statusbar.resource_good")?,
            resource_warn: get_color("component.statusbar.resource_warn")?,
            resource_bad: get_color("component.statusbar.resource_bad")?,
        },
        markdown: MarkdownTokens {
            text: get_color("component.markdown.text")?,
            code: get_color("component.markdown.code")?,
            quote: get_color("component.markdown.quote")?,
        },
        scrollbar: ScrollbarTokens {
            thumb: get_color("component.scrollbar.thumb")?,
            track: get_color("component.scrollbar.track")?,
        },
    };

    Ok(ThemeDefinition {
        name: name.to_string(),
        mode,
        palette,
        semantic,
        component,
    })
}
