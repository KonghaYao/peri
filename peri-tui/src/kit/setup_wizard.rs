//! ratatui-kit SetupWizard —— 完整交互式配置向导。
//!
//! 四步向导：Language → Choose → Form → Done。
//! 状态存储在 `SETUP_WIZARD` atom 中，显隐由 `WIZARD_ACTIVE` 控制。

#![allow(clippy::needless_update)]

use crate::app::setup_wizard::*;
use crate::i18n;
use crate::kit::atoms::{self, LANG_VERSION, SETUP_WIZARD};
use crate::kit::panel_mouse::{AreaTracker, is_scrollbar_column};
use peri_theme::atoms::THEME_ATOM;
use ratatui_kit::{
    crossterm::event::{Event, MouseButton, MouseEventKind},
    prelude::*,
    ratatui::{
        layout::{Constraint, Direction},
        style::{Modifier, Style},
        text::{Line, Span},
        widgets::{Borders, Paragraph},
    },
};

mod handler;
mod render;

use self::handler::{handle_wizard_event, wizard_click};
use self::render::{render_choose_step, render_done_step, render_form_step, render_language_step};

// ── 主组件 ────────────────────────────────────────────────────────────────────

#[component]
pub fn SetupWizard(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let semantic = THEME_ATOM.state().read().semantic;
    let _lang_ver = hooks.use_atom(&LANG_VERSION);

    // 订阅 wizard 状态
    let wizard_handle = hooks.use_atom(&SETUP_WIZARD);
    let wizard_active = hooks.use_atom(&atoms::WIZARD_ACTIVE);
    let _ = *wizard_active.read();
    let state = wizard_handle.read().clone();

    let step = state.step;
    let cursor_color = semantic.status.warning;
    let accent = semantic.status.warning;
    let dim = semantic.text.dim;
    let text_color = semantic.text.primary;
    let focus_color = semantic.status.success;
    let error_color = semantic.status.error;

    // 渲染内容
    let (title, lines) = match step {
        SetupStep::Language => render_language_step(&state, dim, accent, cursor_color, text_color),
        SetupStep::Choose => {
            render_choose_step(&state, dim, accent, cursor_color, text_color, error_color)
        }
        SetupStep::Form => render_form_step(
            &state,
            dim,
            accent,
            cursor_color,
            text_color,
            focus_color,
            error_color,
        ),
        SetupStep::Done => render_done_step(&state, dim, accent, cursor_color, text_color),
    };

    // 面板绘制区域（上一帧）——鼠标点击行号反推
    let area;
    {
        let tracker = hooks.use_hook(AreaTracker::new);
        area = tracker.rect;
    }

    // 事件处理器
    {
        let state = state.clone();
        hooks.use_event_handler_with_options(
            EventScope::Current,
            EventPriority::High,
            EventOptions { hit_test: true },
            move |event| {
                // 鼠标：区域内左键点击 = 选中该项并执行 Enter 动作（click as enter）
                if let Event::Mouse(mouse) = event {
                    if mouse.kind != MouseEventKind::Down(MouseButton::Left) {
                        return EventResult::Ignored;
                    }
                    let Some(area) = area else {
                        return EventResult::Ignored;
                    };
                    // 命中后移动光标，再复用各 step 的 Enter 分支（构造 Enter KeyEvent）
                    let mut st = SETUP_WIZARD.state().read().clone();
                    if !is_scrollbar_column(&mouse, area) && wizard_click(&mouse, area, &mut st) {
                        *SETUP_WIZARD.state().write() = st;
                        return EventResult::Consumed;
                    }
                    // 区域内点击（未命中行）也消费，防止穿透
                    return EventResult::Consumed;
                }
                handle_wizard_event(event, state.clone())
            },
        );
    }

    let title_style = Style::default().fg(accent).add_modifier(Modifier::BOLD);

    element! {
        View(
            flex_direction: Direction::Vertical,
            width: Constraint::Fill(1),
            height: Constraint::Fill(1),
        ) {
            View(width: Constraint::Fill(1), height: Constraint::Fill(1)) {
                Border(
                    flex_direction: Direction::Vertical,
                    border_style: Style::default().fg(accent),
                    borders: Borders::TOP | Borders::BOTTOM,
                    top_title: Line::from(Span::styled(i18n::tr(&title), title_style)).centered(),
                    width: Constraint::Fill(1),
                ) {
                    Text(text: Paragraph::new(lines))
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui_kit::crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
    use ratatui_kit::ratatui::layout::Rect;

    /// 内容区 visual_row 处的左键点击（area 顶部边框行不可点，故 row = area.y + 1 + visual_row）。
    fn click(area: Rect, visual_row: u16) -> MouseEvent {
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: area.x + 1,
            row: area.y + 1 + visual_row,
            modifiers: KeyModifiers::NONE,
        }
    }

    fn browse_state(providers: Vec<MigratedProvider>) -> SetupWizardState {
        SetupWizardState {
            step: SetupStep::Form,
            form_mode: FormMode::Browse,
            providers,
            ..Default::default()
        }
    }

    /// Browse 无 base_url：每 provider 7 行（provider 行 + 空行 + 4 别名 + 空行）。
    /// 第二个 provider 的 provider 行在 visual 8，点击应命中第二个并进入 Edit。
    #[test]
    fn browse_click_without_base_url_hits_second_provider() {
        let mut p1 = MigratedProvider::new(ProviderType::Anthropic);
        p1.base_url = String::new();
        let mut p2 = MigratedProvider::new(ProviderType::OpenAiCompatible);
        p2.base_url = String::new();
        let mut state = browse_state(vec![p1, p2]);
        let area = Rect::new(0, 0, 80, 30);
        assert!(
            wizard_click(&click(area, 8), area, &mut state),
            "visual 8 = 第二个 provider 的 provider 行"
        );
        assert_eq!(state.active_provider, 1, "命中第二个 provider");
        assert_eq!(state.form_mode, FormMode::Edit, "Enter 进入编辑模式");
    }

    /// Browse 带 base_url：每 provider 8 行（provider 行 + url 行 + 空行 + 4 别名 + 空行）。
    /// 第二个 provider 的 provider 行在 visual 9。
    #[test]
    fn browse_click_with_base_url_hits_second_provider() {
        let p1 = MigratedProvider::new(ProviderType::Anthropic);
        let p2 = MigratedProvider::new(ProviderType::OpenAiCompatible);
        let mut state = browse_state(vec![p1, p2]);
        let area = Rect::new(0, 0, 80, 30);
        assert!(
            wizard_click(&click(area, 9), area, &mut state),
            "visual 9 = 第二个 provider 的 provider 行"
        );
        assert_eq!(state.active_provider, 1, "命中第二个 provider");
        assert_eq!(state.form_mode, FormMode::Edit, "Enter 进入编辑模式");
    }
}
