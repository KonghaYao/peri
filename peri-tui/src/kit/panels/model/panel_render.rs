//! Model panel render helpers shared by drawing and hit testing.
//!
//! Keeping the label/alignment helpers next to the component would push
//! `model.rs` over the file budget; the event path imports the same functions so
//! text width and column geometry never drift between draw and hit.

use crate::i18n;
use peri_theme::atoms::THEME_ATOM;
use peri_theme::theme::ThemeDefinition;
use ratatui_kit::ratatui::style::Style;
use ratatui_kit::ratatui::text::Span;

use super::VALUE_ALIGN_COL;
use super::editor_view::{PaneLayout, render_window_editor};
use super::window_edit::WindowEditState;

/// 底部“管理 Provider”按钮文本——渲染与鼠标命中同源，避免两处文本宽度漂移。
pub(super) fn provider_button_label() -> String {
    format!("[{}]", i18n::tr("model-panel-provider-manage-hint"))
}

/// 右栏内联视图的绘制布局（命中只取行位置，颜色不参与判定）。
pub(super) fn window_editor_layout(editor: &WindowEditState) -> PaneLayout {
    let theme_state = THEME_ATOM.state();
    let theme = theme_state.read();
    render_window_editor(editor, &theme)
}

/// 模型名内嵌 effort 后缀（如 "gpt-5.6-luna high"）：主色用 model_info，后缀用 model accent 色高亮。
pub(super) fn styled_model_name(model: &str, theme: &ThemeDefinition) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let lower = model.to_lowercase();
    for level in [" low", " medium", " high", " xhigh", " max"] {
        if let Some(pos) = lower.rfind(level) {
            let (head, tail) = model.split_at(pos + 1);
            spans.push(Span::styled(
                head.to_string(),
                Style::new().fg(theme.semantic.model_info),
            ));
            spans.push(Span::styled(
                tail.to_string(),
                Style::new().fg(theme.semantic.model_accent).bold(),
            ));
            return spans;
        }
    }
    spans.push(Span::styled(
        model.to_string(),
        Style::new().fg(theme.semantic.model_info),
    ));
    spans
}

/// 右侧 K/V 行对齐列：宽屏保持 VALUE_ALIGN_COL 右对齐；窄屏收缩到右列可容纳的最大宽度。
///
/// 布局为 `Percentage(45) | Length(1) | Fill(1)`，右列宽 = 面板宽 - 45% - 1。
/// 行总宽 = key_span + pad + value = align，故 align 上限为右列宽 - 4（mark + 两侧空格余量），
/// 保证窄屏下行宽不超过右列可视区域，值不被截断。
pub(super) fn right_align_col(panel_width: usize) -> usize {
    let right_w = panel_width.saturating_sub(panel_width * 45 / 100 + 1);
    VALUE_ALIGN_COL.min(right_w.saturating_sub(4))
}
