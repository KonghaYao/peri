//! v2 同步渲染入口 — 主线程 `terminal.draw()` 统一入口。
//!
//! 从 `State` 读取 ViewStore + CurrentTurn 派生最终视图，直接调
//! `terminal.draw()`。无独立渲染线程、无 `RenderCache`、无 `RenderEvent` 通道。

pub mod view_render;

use std::time::Instant;

use ratatui::prelude::{CrosstermBackend, Terminal};
use tracing::warn;

use crate::app::App;
use crate::panel::read_context::ServiceRegistrySnapshot;
use crate::state_machine::state::PanelReadContext;
use crate::state_machine::{ModalKind, ModalState, State};

/// 单次渲染需要的时间戳。
///
/// 用 [`Instant`] 而非 `SystemTime`（单调时钟，不受系统时间调整影响）。
pub type RenderTimestamp = Instant;

/// 同步渲染入口。从 `State` 读取数据，调用 `terminal.draw()` 绘制一帧。
///
/// 包含 v2 panel overlay 渲染逻辑：预计算 v2 modal 高度让 legacy 布局保留空间，
/// 再渲染 modal overlay。
pub fn draw(
    state: &mut State,
    app: &mut App,
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
) {
    let view_models: Vec<peri_acp_types::view_model::ViewModel> = state.view_models().to_vec();
    let mut v2_vms: Vec<peri_acp_types::view_model::ViewModel> = view_models.clone();
    if let State::Streaming(s) = &mut *state {
        let turn_vms = s.current_turn.view_models().to_vec();
        if !turn_vms.is_empty() {
            v2_vms.extend(turn_vms);
        }
    }
    let v2_vms_ref: &[peri_acp_types::view_model::ViewModel] = &v2_vms;

    let session = app.session_mgr.current();
    let probe = crate::app::SessionSubAgentProbe::new(session.subagent_status.clone());
    let status_probe: std::rc::Rc<dyn crate::render::view_render::SubAgentStatusProbe> =
        std::rc::Rc::new(probe);
    let draw_result = crate::render::view_render::with_status_probe(status_probe, || {
        terminal.draw(|f| {
            let v2_panel_height = match &*state {
                State::Modal(ModalState {
                    kind: ModalKind::Panel(panel),
                    ..
                }) => Some(panel.desired_height(f.area().height, f.area().width)),
                State::Modal(ModalState {
                    kind: ModalKind::Interaction(handler),
                    ..
                }) => Some(handler.desired_height(f.area().height, f.area().width)),
                _ => None,
            };
            crate::ui::main_ui::render(f, app, v2_panel_height, Some(v2_vms_ref));

            if let State::Modal(ModalState { kind, .. }) = state {
                let area = app
                    .session_mgr
                    .current()
                    .ui
                    .panel_area
                    .unwrap_or(ratatui::layout::Rect::new(0, 0, 80, 24));
                match kind {
                    ModalKind::Panel(panel) => {
                        panel.refresh(app);
                        panel.render(f, area, &build_v2_panel_read_context(app, &view_models));
                    }
                    ModalKind::Interaction(handler) => {
                        handler.render(f, area);
                    }
                }
            }
        })
    });
    if let Err(e) = draw_result {
        warn!(error = %e, "terminal draw failed");
    }
}

/// Build a [`PanelReadContext`] for v2 panel rendering from live App data.
fn build_v2_panel_read_context<'a>(
    app: &'a App,
    view_models: &'a [peri_acp_types::view_model::ViewModel],
) -> PanelReadContext<'a> {
    use std::collections::HashMap;
    use std::sync::LazyLock;

    static EMPTY_CACHE: LazyLock<HashMap<String, serde_json::Value>> = LazyLock::new(HashMap::new);

    let session = app.session_mgr.current();
    let services = ServiceRegistrySnapshot::from_app(app);

    PanelReadContext {
        services,
        view_models,
        scroll_offset: session.ui.scroll_offset,
        area: session
            .ui
            .panel_area
            .unwrap_or(ratatui::layout::Rect::new(0, 0, 80, 24)),
        lc: &app.services.lc,
        acp_query_cache: &EMPTY_CACHE,
    }
}
