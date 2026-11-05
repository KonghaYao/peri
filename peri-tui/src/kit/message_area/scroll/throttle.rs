use std::time::Instant;

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

/// 鼠标拖拽（滚动条 / 文本选区）的节流间隔。
/// 优先级：TuiConfig.scroll_fps > PERI_SCROLL_THROTTLE_MS 环境变量 > 默认 50ms（20fps）。
/// 下限 1ms 防止零值导致无节流。
/// TuiConfig 每次读取（try_read 代价 ~5ns，无争用时），因为用户可能运行时切换。
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

// ── 拖拽选中节流 ──────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub(in crate::kit::message_area) struct DragThrottle {
    pub(in crate::kit::message_area) last_flush: Instant,
}

impl Default for DragThrottle {
    fn default() -> Self {
        Self {
            last_flush: peri_time::monotonic_now(),
        }
    }
}

// ── 滚动落地（私有）────────────────────────────────────────────────────

/// 纯函数：offset 应用滚动量后的新位置（正=向下，负=向上；越界封顶/封底）。
/// 调用方负责先做哨兵归一化（见 [`land_scroll_delta`]）。
pub(super) fn apply_delta_to_offset(offset: usize, delta: i32, max_scroll: usize) -> usize {
    if delta > 0 {
        offset.saturating_add(delta as usize).min(max_scroll)
    } else {
        offset.saturating_sub((-delta) as usize)
    }
}

/// 纯函数：一次滚轮事件落地后的 offset。
///
/// 与 `apply_scroll` 的写入口径一致：先把跟随态哨兵（usize::MAX）归一化到当帧
/// 底部，再应用 delta。每个事件独立调用，不做时间窗合并——合并会让一次落地行数
/// = `SCROLL_LINES` × 窗口内事件数，随轮速/相位变化，步长忽大忽小。
pub(super) fn land_scroll_delta(offset: usize, delta: i32, max_scroll: usize) -> usize {
    apply_delta_to_offset(offset.min(max_scroll), delta, max_scroll)
}

/// 滚轮事件入口：每个事件立即把 delta（正=向下，负=向上）落进 scroll_state，
/// 并同步 follow 状态（上滚即刻离开跟随态，流式增长不会把视口拉回底部）。
/// write_no_update 不触发 notifier.wake()——依赖 dispatch 后 ratatui-kit loop 强制 render。
pub(super) fn apply_scroll(
    delta: i32,
    scroll_state: &State<ScrollPos>,
    scrollbar_fields: &State<ScrollbarFields>,
    follow_bottom: &State<bool>,
) {
    if delta == 0 {
        return;
    }
    let fields = *scrollbar_fields.read();
    let max_scroll = fields.content_length.saturating_sub(fields.viewport_length);
    let mut state = scroll_state.write_no_update();
    // 跟随态下 offset 可能是 usize::MAX 哨兵（scroll_to_bottom 设置、渲染 clamp
    // 前）——先归一化到当帧底部，否则滚轮上滚要先"滚空气"。
    let final_offset = land_scroll_delta(state.offset(), delta, max_scroll);
    state.set_offset(final_offset);
    drop(state);
    update_follow_on_scroll(follow_bottom, max_scroll, final_offset);
}
