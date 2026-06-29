//! Modal-state transition: `(ModalState, Event) -> (State, Vec<Effect>)`.
//!
//! A panel or interaction popup is active and captures all keyboard input.
//! Keys are delegated to the active `PanelState` / `Handler`; Esc dismisses
//! the popup and returns to the previous state (Idle in the P2 stub).
//!
//! Reference: `docs/design/peri-tui-architecture.md` section 8.6.

use std::collections::HashMap;

use ratatui::crossterm::event::{KeyCode, KeyEvent};
use tui_textarea::Input;

use super::super::event::Event;
use super::super::state::{
    HandlerOutput, IdleState, InputState, ModalState, PanelReadContext, State,
};
use crate::panel::effect::PanelEffect;
use crate::panel::read_context::ServiceRegistrySnapshot;
use crate::runtime::effect::Effect;

/// Modal-state transition entry point.
pub fn handle(mut state: ModalState, event: Event) -> (State, Vec<Effect>) {
    match event {
        // -- Esc: dismiss popup -> back to Idle ------------------------------
        Event::Key(KeyEvent {
            code: KeyCode::Esc, ..
        }) => transition_to_idle(),

        // -- Other key events: delegate to the panel/handler -----------------
        Event::Key(key) => {
            let (panel_effects, should_close) = dispatch_key(&mut state, key);
            let effects = map_panel_effects(panel_effects);
            if should_close {
                transition_to_idle_with_effects(effects)
            } else {
                (State::Modal(state), effects)
            }
        }

        // -- Mouse / Resize: re-render so the popup lays out correctly -------
        Event::Mouse(_) | Event::Resize { .. } => (State::Modal(state), vec![Effect::Render]),

        // -- Tick: keep background processes alive while Modal is open --------
        // PollAgent keeps ACP event consumption flowing; AdvanceSpinner keeps
        // the loading animation running; Render ensures the panel redraws.
        Event::Tick => (
            State::Modal(state),
            vec![
                Effect::AdvanceSpinner,
                Effect::PollAgent,
                Effect::PollWorkflow,
                Effect::Render,
            ],
        ),

        // -- Everything else: keep Modal, no effect --------------------------
        Event::Paste(_)
        | Event::AcpEvent(_)
        | Event::AcpDisconnected
        | Event::SessionLoaded { .. }
        | Event::Shutdown => (State::Modal(state), Vec::new()),
    }
}

/// Build a fresh Idle state with a Render effect (used when dismissing modal).
fn transition_to_idle() -> (State, Vec<Effect>) {
    let idle = IdleState {
        input: InputState::default(),
        scroll_offset: 0,
        view: vec![],
        double_esc_timer: None,
        history_index: None,
    };
    (State::Idle(idle), vec![Effect::Render])
}

/// Build a fresh Idle state, preserving the given effects (e.g., ShowNotification).
fn transition_to_idle_with_effects(effects: Vec<Effect>) -> (State, Vec<Effect>) {
    let idle = IdleState {
        input: InputState::default(),
        scroll_offset: 0,
        view: vec![],
        double_esc_timer: None,
        history_index: None,
    };
    // Ensure at least one Render so the user sees the dismissal.
    let mut all_effects = effects;
    if !all_effects.iter().any(|e| matches!(e, Effect::Render)) {
        all_effects.push(Effect::Render);
    }
    (State::Idle(idle), all_effects)
}

/// Dispatch a single key to the active panel / handler, using TLS stubs.
///
/// Returns `(panel_effects, should_close)`. `should_close=true` when the panel
/// produced `PanelEffect::Close` -- caller transitions to Idle.
///
/// **Production code should use [`dispatch_key_with_ctx`] instead**, providing
/// a real [`PanelReadContext`] constructed from App data. This function exists
/// only for test compatibility (tests don't have access to App).
fn dispatch_key(state: &mut ModalState, key: KeyEvent) -> (Vec<PanelEffect>, bool) {
    thread_local! {
        static STUB_SNAPSHOT: ServiceRegistrySnapshot = ServiceRegistrySnapshot::new();
        static STUB_VMS: Vec<peri_acp_types::view_model::ViewModel> = const { Vec::new() };
        static STUB_CACHE: HashMap<String, serde_json::Value> = HashMap::new();
        static STUB_LC: crate::i18n::LcRegistry = crate::i18n::LcRegistry::default();
    }
    STUB_SNAPSHOT.with(|snapshot| {
        STUB_VMS.with(|vms| {
            STUB_CACHE.with(|cache| {
                STUB_LC.with(|lc| {
                    let ctx = PanelReadContext {
                        services: snapshot,
                        view_models: vms,
                        scroll_offset: 0,
                        area: ratatui::layout::Rect::new(0, 0, 0, 0),
                        lc,
                        acp_query_cache: cache,
                    };
                    dispatch_key_with_ctx(state, key, &ctx)
                })
            })
        })
    })
}

/// Dispatch a single key to the active panel / handler with a real context.
///
/// **Production path** -- `ctx` is constructed by main_loop from `&App` data.
/// Returns `(panel_effects, should_close)`.
fn dispatch_key_with_ctx(
    state: &mut ModalState,
    key: KeyEvent,
    ctx: &PanelReadContext,
) -> (Vec<PanelEffect>, bool) {
    match state {
        ModalState::Panel(panel) => {
            let panel_effects = panel.handle_key(Input::from(key), ctx);
            let should_close = panel_effects
                .iter()
                .any(|pe| matches!(pe, PanelEffect::Close));
            (panel_effects, should_close)
        }
        ModalState::Interaction(handler) => {
            if let KeyCode::Char(c) = key.code {
                match handler.handle_key(c) {
                    HandlerOutput::Nothing => (Vec::new(), false),
                    HandlerOutput::Submit(_) | HandlerOutput::Dismiss => (Vec::new(), true),
                }
            } else {
                (Vec::new(), false)
            }
        }
    }
}

/// Modal handler that accepts an external [`PanelReadContext`].
///
/// **Production path** -- called by main_loop when state is `State::Modal(...)`.
/// Uses the provided `ctx` (built from `&App`) instead of TLS stubs.
pub fn handle_with_context(
    mut state: ModalState,
    event: Event,
    ctx: &PanelReadContext,
) -> (State, Vec<Effect>) {
    match event {
        // -- Esc: dismiss popup -> back to Idle ------------------------------
        Event::Key(KeyEvent {
            code: KeyCode::Esc, ..
        }) => transition_to_idle(),

        // -- Other key events: delegate to the panel/handler -----------------
        Event::Key(key) => {
            let (panel_effects, should_close) = dispatch_key_with_ctx(&mut state, key, ctx);
            let effects = map_panel_effects(panel_effects);
            if should_close {
                transition_to_idle_with_effects(effects)
            } else {
                (State::Modal(state), effects)
            }
        }

        // -- Mouse / Resize: re-render so the popup lays out correctly -------
        Event::Mouse(_) | Event::Resize { .. } => (State::Modal(state), vec![Effect::Render]),

        // -- Tick: keep background processes alive while Modal is open --------
        Event::Tick => (
            State::Modal(state),
            vec![
                Effect::AdvanceSpinner,
                Effect::PollAgent,
                Effect::PollWorkflow,
                Effect::Render,
            ],
        ),

        // -- Everything else: keep Modal, no effect --------------------------
        Event::Paste(_)
        | Event::AcpEvent(_)
        | Event::AcpDisconnected
        | Event::SessionLoaded { .. }
        | Event::Shutdown => (State::Modal(state), Vec::new()),
    }
}

/// Map `PanelEffect`s to top-level `Effect`s.
///
/// - `SendToAcp` → forwarded to ACP transport via ApplyContext.
/// - `Copy` → clipboard write via ApplyContext.
/// - `ShowNotification` → main_loop injects into message area.
/// - `UpdateConfig` → main_loop persists to PeriConfig + syncs ACP Server.
/// - `SwitchSession` → main_loop asks App to switch sessions.
/// - `Close` → handled by caller (state transition), not emitted as Effect.
fn map_panel_effects(panel_effects: Vec<PanelEffect>) -> Vec<Effect> {
    panel_effects
        .into_iter()
        .filter_map(|pe| match pe {
            PanelEffect::SendToAcp { event, data } => Some(Effect::SendToAcp {
                method: event,
                params: data,
            }),
            PanelEffect::Copy(text) => Some(Effect::CopyToClipboard(text)),
            PanelEffect::ShowNotification(text) => Some(Effect::ShowNotification(text)),
            PanelEffect::UpdateConfig { key, value } => Some(Effect::UpdateConfig { key, value }),
            PanelEffect::SwitchSession(session_id) => Some(Effect::SwitchSession(session_id)),
            PanelEffect::Close => None,
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::panel_manager::PanelKind;
    use crate::panel::registry::create_panel;
    use crate::state_machine::handler::NoopHandler;
    use ratatui::crossterm::event::{KeyModifiers, MouseEvent, MouseEventKind};

    /// 所有 14 个 PanelKind，用于全量回归测试。
    const ALL_PANEL_KINDS: [PanelKind; 14] = [
        PanelKind::Model,
        PanelKind::Login,
        PanelKind::Agent,
        PanelKind::Hooks,
        PanelKind::Config,
        PanelKind::ThreadBrowser,
        PanelKind::Mcp,
        PanelKind::Plugin,
        PanelKind::Cron,
        PanelKind::Status,
        PanelKind::Memory,
        PanelKind::Tasks,
        PanelKind::Betas,
        PanelKind::Workflow,
    ];

    #[test]
    fn test_esc_dismisses_modal() {
        let modal = ModalState::Interaction(Box::new(NoopHandler));
        let (next, _effects) = handle(
            modal,
            Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
        );
        assert!(matches!(next, State::Idle(_)));
    }

    #[test]
    fn test_tick_keeps_background_processes_alive_in_modal() {
        // Tick 在 Modal 期间必须保持 PollAgent + AdvanceSpinner + Render，
        // 否则 ACP 事件消费停止、loading 动画冻结、面板不再重绘。
        let modal = ModalState::Interaction(Box::new(NoopHandler));
        let (next, effects) = handle(modal, Event::Tick);
        assert!(matches!(next, State::Modal(_)));
        assert!(effects.iter().any(|e| matches!(e, Effect::PollAgent)));
        assert!(effects.iter().any(|e| matches!(e, Effect::AdvanceSpinner)));
        assert!(effects.iter().any(|e| matches!(e, Effect::Render)));
    }

    #[test]
    fn test_panel_close_transitions_to_idle() {
        // Betas panel + Esc -> Close effect -> transition to Idle.
        let panel = create_panel(PanelKind::Betas);
        let modal = ModalState::Panel(panel);
        let (next, _effects) = handle(
            modal,
            Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
        );
        assert!(matches!(next, State::Idle(_)));
    }

    // -----------------------------------------------------------------------
    // 全量面板回归测试（14 个 PanelKind × 关键事件）
    // -----------------------------------------------------------------------

    #[test]
    fn test_all_14_panels_esc_returns_to_idle() {
        // 每个 v2 面板都必须响应 Esc 返回 Idle。
        // 这是面板契约的最基础保证 —— 用户不会被困在面板里。
        for kind in ALL_PANEL_KINDS {
            let panel = create_panel(kind);
            let modal = ModalState::Panel(panel);
            let (next, effects) = handle(
                modal,
                Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            );
            assert!(
                matches!(next, State::Idle(_)),
                "Esc on {kind:?} should transition to Idle, got {next:?}"
            );
            // 至少有一个 Render effect 让用户看到面板已关闭。
            assert!(
                effects.iter().any(|e| matches!(e, Effect::Render)),
                "Esc on {kind:?} should emit at least one Render effect"
            );
        }
    }

    #[test]
    fn test_all_14_panels_tick_keeps_background_alive() {
        // Tick 在面板打开期间必须保持后台进程活跃（PollAgent/AdvanceSpinner/Render）。
        for kind in ALL_PANEL_KINDS {
            let panel = create_panel(kind);
            let modal = ModalState::Panel(panel);
            let (next, effects) = handle(modal, Event::Tick);
            assert!(
                matches!(next, State::Modal(_)),
                "Tick on {kind:?} should keep Modal state, got {next:?}"
            );
            assert!(
                effects.iter().any(|e| matches!(e, Effect::PollAgent)),
                "Tick on {kind:?} should emit PollAgent"
            );
            assert!(
                effects.iter().any(|e| matches!(e, Effect::AdvanceSpinner)),
                "Tick on {kind:?} should emit AdvanceSpinner"
            );
            assert!(
                effects.iter().any(|e| matches!(e, Effect::Render)),
                "Tick on {kind:?} should emit Render"
            );
        }
    }

    #[test]
    fn test_all_14_panels_resize_triggers_render() {
        // Resize 必须触发重绘以重新计算面板布局。
        for kind in ALL_PANEL_KINDS {
            let panel = create_panel(kind);
            let modal = ModalState::Panel(panel);
            let (next, effects) = handle(
                modal,
                Event::Resize {
                    width: 100,
                    height: 40,
                },
            );
            assert!(
                matches!(next, State::Modal(_)),
                "Resize on {kind:?} should keep Modal state"
            );
            assert!(
                effects.iter().any(|e| matches!(e, Effect::Render)),
                "Resize on {kind:?} should emit Render effect"
            );
        }
    }

    #[test]
    fn test_all_14_panels_mouse_triggers_render() {
        // Mouse 事件应该触发重绘（高亮、悬停状态可能改变）。
        let mouse_event = MouseEvent {
            kind: MouseEventKind::Moved,
            column: 5,
            row: 5,
            modifiers: KeyModifiers::NONE,
        };
        for kind in ALL_PANEL_KINDS {
            let panel = create_panel(kind);
            let modal = ModalState::Panel(panel);
            let (next, effects) = handle(modal, Event::Mouse(mouse_event));
            assert!(
                matches!(next, State::Modal(_)),
                "Mouse on {kind:?} should keep Modal state"
            );
            assert!(
                effects.iter().any(|e| matches!(e, Effect::Render)),
                "Mouse on {kind:?} should emit Render effect"
            );
        }
    }

    #[test]
    fn test_all_14_panels_acp_disconnected_is_noop() {
        // ACP 断开时，面板状态保持不变 —— 断开由 main_loop 上层处理。
        for kind in ALL_PANEL_KINDS {
            let panel = create_panel(kind);
            let modal = ModalState::Panel(panel);
            let (next, effects) = handle(modal, Event::AcpDisconnected);
            assert!(
                matches!(next, State::Modal(_)),
                "AcpDisconnected on {kind:?} should keep Modal state"
            );
            assert!(
                effects.is_empty(),
                "AcpDisconnected on {kind:?} should emit no effects"
            );
        }
    }

    #[test]
    fn test_all_14_panels_shutdown_is_noop() {
        // Shutdown 事件在 Modal 状态下被忽略（由 main_loop 顶层处理）。
        for kind in ALL_PANEL_KINDS {
            let panel = create_panel(kind);
            let modal = ModalState::Panel(panel);
            let (next, effects) = handle(modal, Event::Shutdown);
            assert!(
                matches!(next, State::Modal(_)),
                "Shutdown on {kind:?} should keep Modal state"
            );
            assert!(
                effects.is_empty(),
                "Shutdown on {kind:?} should emit no effects"
            );
        }
    }

    // -----------------------------------------------------------------------
    // PanelEffect → Effect 映射测试（核心数据流）
    // -----------------------------------------------------------------------

    #[test]
    fn test_map_panel_effects_close_filtered_out() {
        // Close 不应该作为 Effect 传播 —— 它通过状态转换（→ Idle）处理。
        let panel_effects = vec![PanelEffect::Close];
        let mapped = map_panel_effects(panel_effects);
        assert!(mapped.is_empty(), "Close should be filtered out");
    }

    #[test]
    fn test_map_panel_effects_send_to_acp() {
        let panel_effects = vec![PanelEffect::SendToAcp {
            event: "set_model".to_string(),
            data: serde_json::json!({"alias": "sonnet"}),
        }];
        let mapped = map_panel_effects(panel_effects);
        assert_eq!(mapped.len(), 1);
        match &mapped[0] {
            Effect::SendToAcp { method, params } => {
                assert_eq!(method, "set_model");
                assert_eq!(params["alias"], "sonnet");
            }
            other => panic!("expected SendToAcp, got {other:?}"),
        }
    }

    #[test]
    fn test_map_panel_effects_copy() {
        let panel_effects = vec![PanelEffect::Copy("hello".to_string())];
        let mapped = map_panel_effects(panel_effects);
        assert_eq!(mapped.len(), 1);
        assert_eq!(mapped[0], Effect::CopyToClipboard("hello".to_string()));
    }

    #[test]
    fn test_map_panel_effects_show_notification() {
        let panel_effects = vec![PanelEffect::ShowNotification("saved".to_string())];
        let mapped = map_panel_effects(panel_effects);
        assert_eq!(mapped.len(), 1);
        assert_eq!(mapped[0], Effect::ShowNotification("saved".to_string()));
    }

    #[test]
    fn test_map_panel_effects_update_config() {
        let panel_effects = vec![PanelEffect::UpdateConfig {
            key: "model".to_string(),
            value: "opus".to_string(),
        }];
        let mapped = map_panel_effects(panel_effects);
        assert_eq!(mapped.len(), 1);
        match &mapped[0] {
            Effect::UpdateConfig { key, value } => {
                assert_eq!(key, "model");
                assert_eq!(value, "opus");
            }
            other => panic!("expected UpdateConfig, got {other:?}"),
        }
    }

    #[test]
    fn test_map_panel_effects_switch_session() {
        let panel_effects = vec![PanelEffect::SwitchSession("sess_123".to_string())];
        let mapped = map_panel_effects(panel_effects);
        assert_eq!(mapped.len(), 1);
        assert_eq!(mapped[0], Effect::SwitchSession("sess_123".to_string()));
    }

    #[test]
    fn test_map_panel_effects_mixed_batch() {
        // 模拟 ModelPanel::apply_effects 的真实批量输出。
        let panel_effects = vec![
            PanelEffect::UpdateConfig {
                key: "model".to_string(),
                value: "sonnet".to_string(),
            },
            PanelEffect::SendToAcp {
                event: "set_model".to_string(),
                data: serde_json::json!({}),
            },
            PanelEffect::ShowNotification("switched".to_string()),
            PanelEffect::Close,
        ];
        let mapped = map_panel_effects(panel_effects);
        // Close 被过滤，剩 3 个。
        assert_eq!(mapped.len(), 3);
        assert!(matches!(mapped[0], Effect::UpdateConfig { .. }));
        assert!(matches!(mapped[1], Effect::SendToAcp { .. }));
        assert!(matches!(mapped[2], Effect::ShowNotification(_)));
    }

    #[test]
    fn test_map_panel_effects_empty() {
        let mapped = map_panel_effects(Vec::new());
        assert!(mapped.is_empty());
    }

    // -----------------------------------------------------------------------
    // Handler (Interaction) 测试
    // -----------------------------------------------------------------------

    #[test]
    fn test_handler_submit_closes_modal() {
        // NoopHandler 对任何字符都返回 Nothing，所以应该保持 Modal。
        let modal = ModalState::Interaction(Box::new(NoopHandler));
        let (next, _effects) = handle(
            modal,
            Event::Key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE)),
        );
        // NoopHandler 返回 Nothing，所以保持 Modal。
        assert!(matches!(next, State::Modal(_)));
    }

    #[test]
    fn test_handler_esc_dismisses_to_idle() {
        let modal = ModalState::Interaction(Box::new(NoopHandler));
        let (next, effects) = handle(
            modal,
            Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
        );
        assert!(matches!(next, State::Idle(_)));
        assert!(
            effects.iter().any(|e| matches!(e, Effect::Render)),
            "Esc on interaction should emit Render"
        );
    }

    // -----------------------------------------------------------------------
    // transition_to_idle_with_effects 单元测试
    // -----------------------------------------------------------------------

    #[test]
    fn test_transition_to_idle_with_effects_adds_render_if_missing() {
        let effects = vec![Effect::CopyToClipboard("x".to_string())];
        let (next, all_effects) = transition_to_idle_with_effects(effects);
        assert!(matches!(next, State::Idle(_)));
        // 自动补一个 Render。
        assert!(
            all_effects.iter().any(|e| matches!(e, Effect::Render)),
            "should auto-add Render if missing"
        );
        assert_eq!(all_effects.len(), 2);
    }

    #[test]
    fn test_transition_to_idle_with_effects_preserves_existing_render() {
        let effects = vec![Effect::Render];
        let (next, all_effects) = transition_to_idle_with_effects(effects);
        assert!(matches!(next, State::Idle(_)));
        // 已有 Render，不再补。
        assert_eq!(all_effects.len(), 1);
    }

    #[test]
    fn test_transition_to_idle_basic() {
        let (next, effects) = transition_to_idle();
        assert!(matches!(next, State::Idle(_)));
        assert_eq!(effects.len(), 1);
        assert!(matches!(effects[0], Effect::Render));
    }
}
