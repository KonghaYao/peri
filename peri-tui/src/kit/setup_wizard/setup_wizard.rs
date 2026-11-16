//! ratatui-kit SetupWizard —— 完整交互式配置向导。
//!
//! 配置向导：Language → Choose → Preset/Form/ChatGpt 引导 → Done。
//! 状态存储在 `SETUP_WIZARD` atom 中，显隐由 `WIZARD_ACTIVE` 控制。

#![allow(clippy::needless_update)]

use super::handler;
use super::render;
use crate::app::setup_wizard;
use crate::app::setup_wizard::SetupStep;
use crate::app::setup_wizard::SetupWizardState;
use crate::kit::atoms;
use crate::kit::atoms::LANG_VERSION;
use crate::kit::atoms::SETUP_WIZARD;

use crate::i18n;

use crate::kit::panel_mouse::AreaTracker;
use peri_theme::atoms::THEME_ATOM;
use ratatui_kit::crossterm::event::Event;
use ratatui_kit::crossterm::event::MouseButton;
use ratatui_kit::crossterm::event::MouseEventKind;
use ratatui_kit::prelude::*;
use ratatui_kit::ratatui::layout::Constraint;
use ratatui_kit::ratatui::layout::Direction;
use ratatui_kit::ratatui::style::Modifier;
use ratatui_kit::ratatui::style::Style;
use ratatui_kit::ratatui::text::Line;
use ratatui_kit::ratatui::text::Span;
use ratatui_kit::ratatui::widgets::Borders;
use ratatui_kit::ratatui::widgets::Paragraph;

// ── 主组件 ────────────────────────────────────────────────────────────────────

#[component]
pub fn SetupWizard(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let semantic = THEME_ATOM.state().read().semantic;
    let _lang_ver = hooks.use_atom(&LANG_VERSION);

    // 订阅 wizard 状态
    let wizard_handle = hooks.use_atom(&SETUP_WIZARD);
    let wizard_active = hooks.use_atom(&atoms::WIZARD_ACTIVE);
    // The component mounts for each opening. Initialize only once, so later
    // keystrokes and async completions cannot replace the current draft.
    hooks.use_effect(
        || {
            if atoms::ACP_CLIENT_HANDLE.get().is_some()
                && let Some(handle) = atoms::PERI_CONFIG_HANDLE.get()
            {
                *SETUP_WIZARD.state().write() = setup_wizard::state_from_config(&handle.read());
            }
        },
        (),
    );
    let state = wizard_handle.read().clone();
    let active_now = *wizard_active.read();
    let request = state.connectivity_generation;
    let checking = state.connectivity_in_progress;
    let provider_index = state.active_provider;
    let url = state.active_provider_ref().map(|p| p.base_url.clone());
    // The hook owns exactly one future. Changing input, leaving edit mode, or
    // unmounting replaces/drops it, cancelling the request in the existing runtime.
    hooks.use_async_effect(
        async move {
            if active_now
                && checking
                && let Some(url) = url
            {
                let result = setup_wizard::test_connectivity(&url).await;
                let atom = SETUP_WIZARD.state();
                let mut current = atom.write();
                if current.connectivity_generation == request
                    && current.connectivity_in_progress
                    && current.active_provider == provider_index
                {
                    current.connectivity_in_progress = false;
                    current.connectivity_result = Some(result);
                }
            }
        },
        (active_now, request, checking),
    );
    let saving = state.save_in_progress;
    let draft = state.clone();
    hooks.use_async_effect(
        async move {
            if saving {
                finish_setup(draft).await;
            }
        },
        (saving,),
    );

    let step = state.step;
    let cursor_color = semantic.status.warning;
    let accent = semantic.status.warning;
    let dim = semantic.text.dim;
    let text_color = semantic.text.primary;
    let focus_color = semantic.status.success;
    let error_color = semantic.status.error;

    // 面板绘制区域（上一帧）——鼠标点击行号反推
    let area;
    {
        let tracker = hooks.use_hook(AreaTracker::new);
        area = tracker.rect;
    }

    // 事件处理器
    {
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
                    if !crate::kit::panel_mouse::is_scrollbar_column(&mouse, area)
                        && self::handler::wizard_click(&mouse, area, &mut st)
                    {
                        *SETUP_WIZARD.state().write() = st;
                        return EventResult::Consumed;
                    }
                    // 区域内点击（未命中行）也消费，防止穿透
                    return EventResult::Consumed;
                }
                dispatch_wizard_event(event)
            },
        );
    }

    // 渲染内容
    let (title, lines) = match step {
        SetupStep::Language => {
            self::render::render_language_step(&state, dim, accent, cursor_color, text_color)
        }
        SetupStep::Choose => self::render::render_choose_step(
            &state,
            dim,
            accent,
            cursor_color,
            text_color,
            error_color,
        ),
        SetupStep::ChatGpt => {
            self::render::render_chatgpt_step(&state, dim, accent, cursor_color, text_color)
        }
        SetupStep::Preset => {
            self::render::render_preset_step(&state, dim, accent, cursor_color, text_color)
        }
        SetupStep::Form => self::render::render_form_step(
            &state,
            dim,
            accent,
            cursor_color,
            text_color,
            focus_color,
            error_color,
        ),
        SetupStep::Done => self::render::render_done_step(
            &state,
            dim,
            accent,
            cursor_color,
            text_color,
            error_color,
        ),
    };

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

pub(super) fn dispatch_wizard_event(event: Event) -> EventResult {
    let current = SETUP_WIZARD.state().read().clone();
    self::handler::handle_wizard_event(event, current)
}

// Durable save and runtime activation are separate stages. The wizard stays
// open on either failure; only successful activation publishes the shared config.
pub(crate) async fn finish_setup(draft: SetupWizardState) {
    let from_command = draft.from_command;
    let saved = tokio::task::spawn_blocking(move || setup_wizard::save_setup(&draft)).await;
    let config = match saved {
        Ok(Ok(config)) => config,
        _ => {
            report_setup_failure("setup-save-failed");
            return;
        }
    };
    if from_command {
        let Some(client) = atoms::ACP_CLIENT_HANDLE.get() else {
            report_setup_failure("setup-activation-failed");
            return;
        };
        if client.update_config(&config).await.is_err() {
            report_setup_failure("setup-activation-failed");
            return;
        }
    }
    let Some(handle) = atoms::PERI_CONFIG_HANDLE.get() else {
        report_setup_failure("setup-activation-failed");
        return;
    };
    *handle.write() = config;
    {
        let atom = SETUP_WIZARD.state();
        let mut current = atom.write();
        current.save_in_progress = false;
        current.submit_error = None;
    }
    if !from_command {
        *atoms::SETUP_COMPLETED.state().write() = true;
    }
    *atoms::WIZARD_ACTIVE.state().write() = false;
}

pub(super) fn report_setup_failure(key: &str) {
    let atom = SETUP_WIZARD.state();
    let mut current = atom.write();
    current.save_in_progress = false;
    current.submit_error = Some(i18n::tr(key));
}

#[cfg(test)]
mod tests {
    use super::super::handler;
    use crate::app::setup_wizard::FormMode;
    use crate::app::setup_wizard::MigratedProvider;
    use crate::app::setup_wizard::ProviderType;
    use crate::app::setup_wizard::SetupSource;
    use crate::app::setup_wizard::SetupStep;
    use crate::app::setup_wizard::SetupWizardState;
    use crate::kit::atoms;

    use crate::kit::atoms::SETUP_WIZARD;

    use ratatui_kit::crossterm::event::Event;
    use ratatui_kit::crossterm::event::{
        KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    };
    use ratatui_kit::prelude::EventResult;
    use ratatui_kit::ratatui::layout::Rect;

    #[test]
    #[serial_test::serial]
    fn setup_keyboard_dispatch_releases_snapshot_before_writing_state() {
        SETUP_WIZARD.set(SetupWizardState {
            step: SetupStep::Choose,
            ..Default::default()
        });
        assert_eq!(
            super::dispatch_wizard_event(Event::Key(KeyEvent::new(
                KeyCode::Down,
                KeyModifiers::NONE
            ))),
            EventResult::Consumed
        );
        assert_eq!(SETUP_WIZARD.state().read().choose_cursor, 1);
        assert_eq!(
            super::dispatch_wizard_event(Event::Key(KeyEvent::new(
                KeyCode::Up,
                KeyModifiers::NONE
            ))),
            EventResult::Consumed
        );
        assert_eq!(SETUP_WIZARD.state().read().choose_cursor, 0);
        super::dispatch_wizard_event(Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
        assert_eq!(SETUP_WIZARD.state().read().step, SetupStep::Language);
        atoms::WIZARD_ACTIVE.set(true);
        super::dispatch_wizard_event(Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
        assert!(!atoms::WIZARD_ACTIVE.get());
    }

    /// 内容区 visual_row 处的左键点击（area 顶部边框行不可点，故 row = area.y + 1 + visual_row）。
    fn click(area: Rect, visual_row: u16) -> MouseEvent {
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: area.x + 1,
            row: area.y + 1 + visual_row,
            modifiers: KeyModifiers::NONE,
        }
    }

    #[must_use]
    fn browse_state(providers: Vec<MigratedProvider>) -> SetupWizardState {
        SetupWizardState {
            step: SetupStep::Form,
            form_mode: FormMode::Browse,
            providers,
            ..Default::default()
        }
    }

    #[test]
    fn chatgpt_guide_click_back_returns_to_source_without_opening_login() {
        let mut state = SetupWizardState {
            step: SetupStep::ChatGpt,
            ..Default::default()
        };
        let area = Rect::new(0, 0, 80, 30);
        assert!(!self::handler::wizard_click(
            &click(area, 10),
            area,
            &mut state
        ));
        assert!(self::handler::wizard_click(
            &click(area, 13),
            area,
            &mut state
        ));
        assert_eq!(state.step, SetupStep::Choose);
    }

    #[test]
    #[serial_test::serial]
    fn chatgpt_guide_click_continue_opens_login_only_after_confirmation() {
        let mut state = SetupWizardState {
            step: SetupStep::Choose,
            source: SetupSource::ChatGptSubscription,
            ..Default::default()
        };
        atoms::SETUP_PREFLIGHT.set(false);
        atoms::WIZARD_ACTIVE.set(true);
        let area = Rect::new(0, 0, 80, 30);
        assert!(self::handler::wizard_click(
            &click(area, 3 + 2 * 3),
            area,
            &mut state
        ));
        assert_eq!(state.step, SetupStep::ChatGpt);
        assert!(atoms::WIZARD_ACTIVE.get());
        assert!(self::handler::wizard_click(
            &click(area, 11),
            area,
            &mut state
        ));
        assert!(!atoms::WIZARD_ACTIVE.get());
        assert_eq!(
            *atoms::ACTIVE_PANEL.state().read(),
            Some(crate::app::panel_types::PanelKind::Login)
        );
        crate::kit::panel_registry::close_active_panel();
    }

    #[test]
    #[serial_test::serial]
    fn chatgpt_guide_direction_keys_and_escape_dispatch_without_locking() {
        SETUP_WIZARD.set(SetupWizardState {
            step: SetupStep::ChatGpt,
            ..Default::default()
        });
        for direction in [KeyCode::Down, KeyCode::Left, KeyCode::Right, KeyCode::Up] {
            assert_eq!(
                super::dispatch_wizard_event(Event::Key(KeyEvent::new(
                    direction,
                    KeyModifiers::NONE
                ))),
                EventResult::Consumed
            );
        }
        super::dispatch_wizard_event(Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
        assert_eq!(SETUP_WIZARD.state().read().step, SetupStep::Choose);
    }

    #[test]
    #[serial_test::serial]
    fn chatgpt_guide_preflight_confirmation_retains_login_model_handoff() {
        let mut state = SetupWizardState {
            step: SetupStep::ChatGpt,
            ..Default::default()
        };
        atoms::SETUP_PREFLIGHT.set(true);
        atoms::SETUP_CHATGPT_PHASE.set(None);
        atoms::WIZARD_ACTIVE.set(true);
        let area = Rect::new(0, 0, 80, 30);
        assert!(self::handler::wizard_click(
            &click(area, 11),
            area,
            &mut state
        ));
        assert_eq!(
            atoms::SETUP_CHATGPT_PHASE.get(),
            Some(atoms::SetupChatGptPhase::Login)
        );
        assert_eq!(
            *atoms::ACTIVE_PANEL.state().read(),
            Some(crate::app::panel_types::PanelKind::Login)
        );
        crate::kit::panel_registry::close_active_panel();
        atoms::SETUP_CHATGPT_PHASE.set(None);
        atoms::SETUP_PREFLIGHT.set(false);
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
            self::handler::wizard_click(&click(area, 8), area, &mut state),
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
            self::handler::wizard_click(&click(area, 9), area, &mut state),
            "visual 9 = 第二个 provider 的 provider 行"
        );
        assert_eq!(state.active_provider, 1, "命中第二个 provider");
        assert_eq!(state.form_mode, FormMode::Edit, "Enter 进入编辑模式");
    }
}
