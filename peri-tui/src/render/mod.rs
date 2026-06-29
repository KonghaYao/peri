//! v2 渲染入口 — 主线程同步渲染 + 16ms 帧率节流。
//!
//! 设计目标（参见 `docs/design/peri-tui-architecture.md` §4.5 + 02-execution-plan.md Phase 5）：
//!
//! - **同步渲染**：从 `State` 读取 ViewStore + CurrentTurn 派生最终视图，直接调
//!   `terminal.draw()`。无独立渲染线程、无 `RenderCache`、无 `RenderEvent` 通道。
//! - **16ms 帧率节流**：`Effect::Render` 在 `Tick` 上触发时，距上次渲染不足 16ms
//!   则跳过。`view-commit` / `turn-done` / `turn-interrupted` 立即渲染（不节流）。
//! - **Idle 省电**：Idle 状态下 `Tick` 不触发渲染（无 spinner 需要动画）。
//! - **block_mode 内部细节**：Markdown 围栏缓冲作为渲染层内部实现，不暴露给状态机。
//!
//! ## 当前状态（P5 骨架）
//!
//! 本模块提供：
//! - [`Throttle`]：16ms 帧率节流器（含 force_render 旁路）。
//! - [`render`]：同步渲染入口（目前委托给 legacy `App::draw`，后续替换为
//!   `State` 驱动的派生）。
//!
//! **message_pipeline 暂时保留**作为渲染数据源（P5 完整切换前的过渡）。
//! 当 P3 面板迁移完成、`ViewStore` 在状态机内积累足够数据后，本模块切换为
//! 从 `State.view + current_turn` 派生最终视图。

pub mod block_mode;
pub mod throttle;

pub use throttle::{RenderReason, Throttle};

use std::time::Instant;

use ratatui::{backend::Backend, layout::Rect, Frame, Terminal};

use crate::state_machine::State;

/// 同步渲染入口。
///
/// 从 `State` 读取数据，调用 `terminal.draw()` 绘制一帧。
///
/// **当前实现（P5 骨架）**：直接委托给 legacy `App::draw` 路径。
/// 后续将切换为 `ViewStore::for_render(state.view, state.current_turn)` 派生
/// 最终视图。
pub fn render<B: Backend>(
    _state: &State,
    terminal: &mut Terminal<B>,
    app_draw: impl FnOnce(&mut Frame, Rect),
) where
    B::Error: std::fmt::Debug,
{
    if let Err(e) = terminal.draw(|f| {
        let area = f.area();
        app_draw(f, area);
    }) {
        tracing::warn!(error = ?e, "terminal draw failed");
    }
}

/// 单次渲染需要的时间戳。
///
/// 用 [`Instant`] 而非 `SystemTime`（单调时钟，不受系统时间调整影响）。
pub type RenderTimestamp = Instant;
