//! 16ms 帧率节流器。
//!
//! 设计目标（参见 `docs/design/peri-tui-architecture.md` §4.5）：
//!
//! - 主循环在 `Effect::Render` 执行时检查距上次渲染时间。
//! - `view-commit` / `turn-done` / `turn-interrupted` 跳过节流立即渲染
//!   （边界事件不能延迟，否则用户感知卡顿）。
//! - `Tick` 在 Idle 下不渲染（省电）；Streaming 下推进 spinner 帧并渲染。
//! - 用户输入（Key/Mouse/Paste/Resize）总是立即渲染（响应性优先）。
//!
//! ## 用法
//!
//! ```ignore
//! let mut throttle = Throttle::default();
//!
//! // Tick 事件
//! if throttle.should_render(RenderReason::Tick) {
//!     render::render(...);
//!     throttle.mark_rendered();
//! }
//!
//! // view-commit 事件
//! if throttle.should_render(RenderReason::ViewCommit) {
//!     render::render(...);
//!     throttle.mark_rendered();
//! }
//! ```

use std::time::{Duration, Instant};

/// 目标帧间隔（约 60 FPS）。
///
/// 16ms 是 60Hz 屏幕的一帧时长。低于此间隔的连续 Tick 会被节流。
pub const TARGET_FRAME_INTERVAL: Duration = Duration::from_millis(16);

/// 触发渲染的原因。
///
/// 决定是否跳过 16ms 节流：
/// - [`RenderReason::Boundary`] 和 [`RenderReason::UserInput`] 总是立即渲染。
/// - [`RenderReason::Tick`] 受 16ms 节流。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderReason {
    /// 用户输入（Key/Mouse/Paste/Resize）— 立即渲染。
    UserInput,

    /// 边界事件（view-commit / turn-done / turn-interrupted）— 立即渲染。
    Boundary,

    /// 周期性 Tick — 16ms 节流。
    Tick,
}

/// 16ms 帧率节流器。
///
/// 跟踪上次渲染的 [`Instant`]，决定当前帧是否应该渲染。
#[derive(Debug, Clone, Default)]
pub struct Throttle {
    /// 上次渲染的时刻。`None` 表示尚未渲染过——首次渲染总是通过。
    last_render: Option<Instant>,
}

impl Throttle {
    /// 创建新的节流器。
    pub fn new() -> Self {
        Self::default()
    }

    /// 决定是否应该渲染。
    ///
    /// - [`RenderReason::Boundary`] / [`RenderReason::UserInput`]：总是返回 `true`。
    /// - [`RenderReason::Tick`]：距上次渲染 ≥ 16ms 才返回 `true`。
    pub fn should_render(&self, reason: RenderReason) -> bool {
        match reason {
            RenderReason::Boundary | RenderReason::UserInput => true,
            RenderReason::Tick => match self.last_render {
                None => true,
                Some(last) => last.elapsed() >= TARGET_FRAME_INTERVAL,
            },
        }
    }

    /// 标记本次渲染已完成，更新 `last_render` 时间戳。
    ///
    /// 应该在 `should_render` 返回 `true` 并执行渲染后调用。
    pub fn mark_rendered(&mut self) {
        self.last_render = Some(Instant::now());
    }

    /// 重置节流器（清空 `last_render`）。
    ///
    /// 用于会话切换等场景，强制下次渲染总是通过。
    pub fn reset(&mut self) {
        self.last_render = None;
    }

    /// 距上次渲染的时间。`None` 表示尚未渲染过。
    pub fn time_since_last_render(&self) -> Option<Duration> {
        self.last_render.map(|t| t.elapsed())
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_no_last_render() {
        let t = Throttle::default();
        assert!(t.time_since_last_render().is_none());
    }

    #[test]
    fn test_boundary_always_renders() {
        let mut t = Throttle::default();
        t.mark_rendered();
        // 即使刚渲染过，boundary 也要立即渲染
        assert!(t.should_render(RenderReason::Boundary));
    }

    #[test]
    fn test_user_input_always_renders() {
        let mut t = Throttle::default();
        t.mark_rendered();
        assert!(t.should_render(RenderReason::UserInput));
    }

    #[test]
    fn test_first_tick_renders() {
        let t = Throttle::default();
        assert!(t.should_render(RenderReason::Tick));
    }

    #[test]
    fn test_tick_throttled_after_mark() {
        let mut t = Throttle::default();
        t.mark_rendered();
        // 距上次渲染 < 16ms，应该被节流
        assert!(!t.should_render(RenderReason::Tick));
    }

    #[test]
    fn test_reset_clears_last_render() {
        let mut t = Throttle::default();
        t.mark_rendered();
        assert!(t.time_since_last_render().is_some());

        t.reset();
        assert!(t.time_since_last_render().is_none());
        assert!(t.should_render(RenderReason::Tick));
    }

    #[test]
    fn test_mark_rendered_updates_timestamp() {
        let mut t = Throttle::default();
        assert!(t.time_since_last_render().is_none());

        t.mark_rendered();
        let elapsed = t.time_since_last_render().unwrap();
        // 刚渲染完，elapsed 应该非常小
        assert!(elapsed.as_millis() < 100);
    }
}
