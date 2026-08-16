use std::time::{Duration, Instant};

use crate::kit::atoms::TUI_CONFIG_HANDLE;
use crate::kit::message_area::props::ScrollbarFields;
use ratatui_kit::prelude::*;

use super::{ScrollPos, update_follow_on_scroll};

// ── 滚动速度控制 ──────────────────────────────────────────────────────────

/// 鼠标滚轮每格的滚动行数倍数。
/// pub(crate)：面板滚轮仲裁（panel_scroll.rs）复用同一步长，统一跨区域滚动速度。
pub(crate) const SCROLL_LINES: u16 = 3;

/// mod.rs 在 total_visual_rows 上追加的滚动缓冲行数（仅影响 max_scroll /
/// content_length，不计入实际渲染内容）。吸底跟随恢复判定需扣除该缓冲，
/// 见 `should_follow_after_user_scroll` 的 [Fix padding]。
pub(in crate::kit::message_area) const SCROLL_PADDING: usize = 2;

/// scroll_frame_ms() 的默认值。fps=20 → 50ms。
const DEFAULT_SCROLL_FRAME_MS: u64 = 50;

/// fps 值转换为毫秒间隔
fn fps_to_ms(fps: u32) -> u64 {
    match fps {
        60 => 16,
        30 => 33,
        20 => 50,
        _ => 16,
    }
}

/// 优先级：TuiConfig.scroll_fps > PERI_SCROLL_THROTTLE_MS 环境变量 > 默认 50ms（20fps）。
/// 下限 1ms 防止零值导致无节流。
/// TuiConfig 每次读取（try_read 代价 ~5ns，无争用时），因为用户可能运行时切换。
/// pub(crate)：面板滚轮仲裁（panel_scroll.rs）复用同一帧率配置。
pub(crate) fn scroll_frame_ms() -> u64 {
    // 优先读 TuiConfig
    if let Some(handle) = TUI_CONFIG_HANDLE.get()
        && let Some(tui) = handle.try_read()
        && let Some(fps) = tui.scroll_fps
    {
        return fps_to_ms(fps).max(1);
    }
    // fallback: 环境变量
    thread_local! {
        static ENV_VAL: Option<u64> = std::env::var("PERI_SCROLL_THROTTLE_MS")
            .ok()
            .and_then(|v| v.parse().ok())
            .map(|v: u64| v.max(1));
    }
    if let Some(ms) = ENV_VAL.with(|v| *v) {
        return ms;
    }
    DEFAULT_SCROLL_FRAME_MS
}

#[derive(Debug, Clone)]
/// pub(crate)：面板滚轮仲裁（panel_scroll.rs）复用同一节流器。
pub(crate) struct ScrollThrottle {
    pub(crate) last_flush: Instant,
    pub(crate) pending_delta: i32, // positive = scroll_down, negative = scroll_up
}

impl Default for ScrollThrottle {
    fn default() -> Self {
        Self {
            last_flush: Instant::now(),
            pending_delta: 0,
        }
    }
}

// ── 拖拽选中节流 ──────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub(in crate::kit::message_area) struct DragThrottle {
    pub(in crate::kit::message_area) last_flush: Instant,
}

impl Default for DragThrottle {
    fn default() -> Self {
        Self {
            last_flush: Instant::now(),
        }
    }
}

// ── 滚动节流（私有）────────────────────────────────────────────────────

/// 纯函数：offset 应用滚动量后的新位置（正=向下，负=向上；越界封顶/封底）。
/// 哨兵归一化（offset > max_scroll 时先落到 max_scroll）由 `apply_pending` 负责。
pub(super) fn apply_delta_to_offset(offset: usize, delta: i32, max_scroll: usize) -> usize {
    if delta > 0 {
        offset.saturating_add(delta as usize).min(max_scroll)
    } else {
        offset.saturating_sub((-delta) as usize)
    }
}

/// 反向判定：pending 与新 delta 方向相反时需要先落地旧 pending，
/// 避免「先累积后抵消」造成滚动不到位/回弹（ghostty/ssh burst 场景）。
pub(super) fn is_reverse_direction(pending_delta: i32, delta: i32) -> bool {
    pending_delta != 0 && (pending_delta > 0) != (delta > 0)
}

/// 把一段滚动量（正=向下，负=向上）推入 scroll_state，并同步 follow 状态。
fn apply_pending(
    pending: i32,
    scroll_state: &State<ScrollPos>,
    scrollbar_fields: &State<ScrollbarFields>,
    follow_bottom: &State<bool>,
) {
    if pending == 0 {
        return;
    }
    let fields = *scrollbar_fields.read();
    let max_scroll = fields.content_length.saturating_sub(fields.viewport_length);
    let mut state = scroll_state.write_no_update();
    // 跟随态下 offset 可能是 usize::MAX 哨兵（scroll_to_bottom 设置、渲染 clamp
    // 前）——先归一化到当帧底部，否则滚轮上滚要先"滚空气"。
    if state.offset() > max_scroll {
        state.set_offset(max_scroll);
    }
    let final_offset = apply_delta_to_offset(state.offset(), pending, max_scroll);
    state.set_offset(final_offset);
    drop(state);
    update_follow_on_scroll(follow_bottom, max_scroll, final_offset);
}

/// 节流 flush 核心：把累积的 pending_delta 一次性推入 scroll_state 并同步 follow。
/// 供 `apply_scroll`（事件到达时）与渲染帧兜底（mod.rs 每帧调用）共用。
/// 返回是否实际 flush 了非零滚动量。
pub(in crate::kit::message_area) fn flush_scroll_if_due(
    scroll_throttle: &State<ScrollThrottle>,
    scroll_state: &State<ScrollPos>,
    scrollbar_fields: &State<ScrollbarFields>,
    follow_bottom: &State<bool>,
) -> bool {
    let mut st = scroll_throttle.write_no_update();
    let now = Instant::now();
    if now.duration_since(st.last_flush) < Duration::from_millis(scroll_frame_ms()) {
        return false;
    }
    let pending = st.pending_delta;
    st.pending_delta = 0;
    st.last_flush = now;
    drop(st);
    apply_pending(pending, scroll_state, scrollbar_fields, follow_bottom);
    pending != 0
}

/// 滚动节流：累积 delta，仅在距上次 flush ≥ scroll_frame_ms() 时推入 scroll_state。
/// write_no_update 不触发 notifier.wake()——依赖 dispatch 后 ratatui-kit loop 强制 render。
pub(super) fn apply_scroll(
    delta: i32,
    scroll_throttle: &State<ScrollThrottle>,
    scroll_state: &State<ScrollPos>,
    scrollbar_fields: &State<ScrollbarFields>,
    follow_bottom: &State<bool>,
) {
    {
        let mut st = scroll_throttle.write_no_update();
        // [Fix 反向落地] 反向滚动时旧方向 pending 立即落地（即使未到节流窗口），
        // 再累积新方向——消除「先动后猛跳」的抵消错位（ghostty/ssh burst 场景）。
        if is_reverse_direction(st.pending_delta, delta) {
            let old = st.pending_delta;
            st.pending_delta = 0;
            st.last_flush = Instant::now();
            drop(st);
            apply_pending(old, scroll_state, scrollbar_fields, follow_bottom);
        } else {
            st.pending_delta += delta;
        }
    }
    flush_scroll_if_due(
        scroll_throttle,
        scroll_state,
        scrollbar_fields,
        follow_bottom,
    );
}
