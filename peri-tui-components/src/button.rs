use peri_theme::theme::ThemeDefinition;
use ratatui::{
    style::{Modifier, Style},
    text::Span,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ButtonChrome {
    Filled,
    Bracketed { padded: bool },
}

impl ButtonChrome {
    /// 只生成按钮文本；布局调用方可以不读取主题。
    pub fn text(self, label: &str) -> String {
        match self {
            Self::Filled => format!(" {label} "),
            Self::Bracketed { padded: true } => format!("[ {label} ]"),
            Self::Bracketed { padded: false } => format!("[{label}]"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ButtonTone {
    Primary,
    Secondary,
    Danger,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ButtonState {
    Normal,
    Focused,
    Pressed,
    Disabled,
}

pub struct ButtonSpec<'a> {
    pub label: &'a str,
    pub chrome: ButtonChrome,
    pub tone: ButtonTone,
    pub state: ButtonState,
}

pub struct ButtonVisual {
    pub text: String,
    pub style: Style,
    pub width: u16,
}

pub fn button_visual(spec: ButtonSpec<'_>, theme: &ThemeDefinition) -> ButtonVisual {
    let text = spec.chrome.text(spec.label);
    let width = Span::raw(text.as_str()).width().min(u16::MAX as usize) as u16;
    ButtonVisual {
        text,
        style: button_style(spec.chrome, spec.tone, spec.state, theme),
        width,
    }
}

pub fn button_style(
    chrome: ButtonChrome,
    tone: ButtonTone,
    state: ButtonState,
    theme: &ThemeDefinition,
) -> Style {
    let semantic = &theme.semantic;
    let foreground = match tone {
        ButtonTone::Primary if chrome == ButtonChrome::Filled => semantic.accent,
        ButtonTone::Primary => semantic.text.primary,
        ButtonTone::Secondary => semantic.text.muted,
        ButtonTone::Danger => semantic.status.error,
    };
    let normal = Style::default().fg(foreground);
    let filled = if chrome == ButtonChrome::Filled {
        Modifier::REVERSED
    } else {
        Modifier::empty()
    };
    match state {
        ButtonState::Normal => normal.add_modifier(filled),
        ButtonState::Focused => Style::default()
            .fg(semantic.accent)
            .add_modifier(filled | Modifier::BOLD),
        ButtonState::Pressed => {
            let (fg, bg) = if chrome == ButtonChrome::Filled {
                (foreground, semantic.surface.default)
            } else {
                (semantic.surface.default, semantic.accent)
            };
            // Normal 的反色由终端完成；覆盖已绘制单元时也必须显式撤销 REVERSED。
            Style::default()
                .fg(fg)
                .bg(bg)
                .remove_modifier(Modifier::REVERSED)
                .add_modifier(Modifier::BOLD)
        }
        ButtonState::Disabled => Style::default()
            .fg(semantic.text.muted)
            .add_modifier(Modifier::DIM),
    }
}

/// 命中指定行上的半开列区间 `[x_start, x_end)`。
pub fn hit_test(row: u16, col: u16, hit_row: u16, x_start: u16, x_end: u16) -> bool {
    row == hit_row && (x_start..x_end).contains(&col)
}

#[cfg(test)]
#[path = "button_test.rs"]
mod tests;
