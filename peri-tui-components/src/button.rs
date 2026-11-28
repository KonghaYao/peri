use peri_theme::theme::ThemeDefinition;
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    text::Span,
};

use crate::text::truncate_to_width;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ButtonChrome {
    /// 反色填充块：用于页面主操作。
    Filled,
    /// 只有标签文字：用于次要操作，靠色调区分。
    Plain,
    /// 旧式方括号外观：`[ label ]`；保留给既有调用方。
    Bracketed { padded: bool },
}

impl ButtonChrome {
    /// 只生成按钮文本；布局调用方可以不读取主题。
    pub fn text(self, label: &str) -> String {
        match self {
            Self::Filled => format!(" {label} "),
            Self::Plain => label.to_owned(),
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

fn text_width(text: &str) -> u16 {
    Span::raw(text).width().min(u16::MAX as usize) as u16
}

pub fn button_visual(spec: ButtonSpec<'_>, theme: &ThemeDefinition) -> ButtonVisual {
    let text = spec.chrome.text(spec.label);
    ButtonVisual {
        width: text_width(&text),
        text,
        style: button_style(spec.chrome, spec.tone, spec.state, theme),
    }
}

/// A draw-only button.
///
/// The widget never registers hit regions or claims focus: the host owns
/// geometry, hit testing and focus, and passes the resulting state in.
pub struct Button<'a> {
    pub label: &'a str,
    pub chrome: ButtonChrome,
    pub tone: ButtonTone,
    pub state: ButtonState,
}

impl<'a> Button<'a> {
    #[must_use]
    pub fn new(label: &'a str, chrome: ButtonChrome, tone: ButtonTone, state: ButtonState) -> Self {
        Self {
            label,
            chrome,
            tone,
            state,
        }
    }

    /// Width of the chrome text in cells. Independent of [`Self::state`].
    #[must_use]
    pub fn width(&self) -> u16 {
        text_width(&self.chrome.text(self.label))
    }

    /// Draw the button into `rect`, clipping the text to `rect.width`.
    pub fn render(&self, buf: &mut Buffer, rect: Rect, theme: &ThemeDefinition) {
        let rect = rect.intersection(buf.area);
        if rect.is_empty() {
            return;
        }
        let visual = button_visual(
            ButtonSpec {
                label: self.label,
                chrome: self.chrome,
                tone: self.tone,
                state: self.state,
            },
            theme,
        );
        let text = truncate_to_width(&visual.text, usize::from(rect.width));
        buf.set_string(rect.x, rect.y, text, visual.style);
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
