//! Bridge：ThemeDefinition → ratatui-kit Palette / PeriColors 映射。
//!
//! 提供 `to_palette()` 和 `to_peri_colors()` 两个转换方法。

use ratatui_kit::prelude::Palette;

use crate::peri_colors::PeriColors;
use crate::theme::ThemeDefinition;

/// ThemeDefinition 扩展：向 ratatui-kit Palette 和 PeriColors 的转换。
pub trait ThemeDefinitionExt {
    /// 映射到 ratatui-kit Palette。
    fn to_palette(&self) -> Palette;
    /// 映射到 PeriColors。
    fn to_peri_colors(&self) -> PeriColors;
}

impl ThemeDefinitionExt for ThemeDefinition {
    fn to_palette(&self) -> Palette {
        let mut p = Palette::default();
        p.fg = self.semantic.text.primary;
        p.fg_dim = self.semantic.text.muted;
        p.bg = self.palette.base.bg;
        p.surface = self.semantic.surface.default;
        p.overlay = self.semantic.surface.selection;
        p.accent = self.semantic.accent;
        p.on_accent = self.semantic.surface.default;
        p.selection = self.semantic.surface.selection;
        p.border = self.semantic.border.default;
        p.border_active = self.semantic.border.active;
        p.success = self.palette.success.primary;
        p.warning = self.palette.warning.primary;
        p.error = self.palette.danger.primary;
        p.info = self.palette.info.primary;
        p.placeholder = self.component.input.placeholder;
        p
    }

    fn to_peri_colors(&self) -> PeriColors {
        PeriColors {
            surface_user: self.semantic.surface.user,
            surface_popup: self.semantic.surface.popup,
            surface_cursor: self.semantic.surface.cursor,
            surface_raised: self.semantic.surface.raised,
            surface_sunken: self.semantic.surface.sunken,
            status_running: self.semantic.status.running,
            status_thinking: self.semantic.thinking,
            accent_user: self.semantic.accents.user,
            accent_assistant: self.semantic.accents.assistant,
            accent_reasoning: self.semantic.accents.reasoning,
            accent_tool: self.semantic.accents.tool,
            text_secondary: self.semantic.text.secondary,
            syntax_command: self.semantic.syntax.command,
            syntax_path: self.semantic.syntax.path,
            border_dim: self.semantic.border.dim,
            model_info: self.semantic.model_info,
            bash_border: self.semantic.bash_border,
            selected_fg: self.semantic.selected_fg,
            diff_add: self.semantic.diff.add,
            diff_remove: self.semantic.diff.remove,
            diff_hunk: self.semantic.diff.hunk,
            diff_add_bg: self.semantic.diff.add_bg,
            diff_remove_bg: self.semantic.diff.remove_bg,
            diff_add_word_bg: self.semantic.diff.add_word_bg,
            diff_remove_word_bg: self.semantic.diff.remove_word_bg,
            scrollbar_thumb: self.component.scrollbar.thumb,
            scrollbar_track: self.component.scrollbar.track,
            resource_good: self.component.statusbar.resource_good,
            resource_warn: self.component.statusbar.resource_warn,
            resource_bad: self.component.statusbar.resource_bad,
        }
    }
}

/// 创建 ratatui-kit Palette::default() 便于 atom 初始化。
pub fn default_palette() -> Palette {
    Palette::default()
}

/// 创建默认 PeriColors（全 Reset）。
pub fn default_peri_colors() -> PeriColors {
    PeriColors::default()
}

impl From<&ThemeDefinition> for PeriColors {
    fn from(theme: &ThemeDefinition) -> Self {
        theme.to_peri_colors()
    }
}
