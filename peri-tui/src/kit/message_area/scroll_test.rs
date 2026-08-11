//! Tests
use super::*;

// ── ScrollPos（usize 滚动状态）测试 ───────────────────────────────────

#[test]
fn test_scroll_pos_exceeds_u16_max() {
    // 核心回归：滚动位置必须能超过 65535（旧 ScrollViewState 的 u16 Position 上限）
    let mut pos = ScrollPos::default();
    for _ in 0..70_000 {
        pos.scroll_down();
    }
    assert_eq!(pos.offset(), 70_000);
    assert!(pos.offset() > u16::MAX as usize);
    // 渲染侧 clamp 到 max_scroll（如 70_000 - 40）后仍可达真实底部
    let max_scroll = 70_000usize.saturating_sub(40);
    assert_eq!(pos.offset().min(max_scroll), max_scroll);
}

#[test]
fn test_scroll_pos_scroll_up_never_underflows() {
    let mut pos = ScrollPos::default();
    pos.set_offset(10);
    pos.scroll_up();
    assert_eq!(pos.offset(), 9);
    for _ in 0..20 {
        pos.scroll_up();
    }
    assert_eq!(pos.offset(), 0);
}

#[test]
fn test_scroll_pos_scroll_to_bottom_uses_max() {
    let mut pos = ScrollPos::default();
    pos.scroll_to_bottom();
    assert_eq!(pos.offset(), usize::MAX);
    pos.scroll_to_top();
    assert_eq!(pos.offset(), 0);
}

fn proximity_check(total: u16, scroll_y: u16, vis_height: u16) -> bool {
    if total == 0 {
        return false;
    }
    let max_scroll = total.saturating_sub(vis_height);
    if scroll_y >= max_scroll {
        return false;
    }
    let distance = max_scroll.saturating_sub(scroll_y);
    let threshold = (vis_height / 2).max(5);
    distance <= threshold
}

#[test]
fn test_proximity_at_bottom_should_not_trigger_scroll() {
    let total = 100;
    let vis_height = 20;
    let scroll_y = total - vis_height;
    assert!(!proximity_check(total, scroll_y, vis_height));
}

#[test]
fn test_proximity_within_half_viewport_should_follow() {
    let total = 100;
    let vis_height = 20;
    let scroll_y = total - vis_height - 10;
    assert!(proximity_check(total, scroll_y, vis_height));
}

#[test]
fn test_proximity_beyond_half_viewport_should_not_follow() {
    let total = 100;
    let vis_height = 20;
    let scroll_y = total - vis_height - 11;
    assert!(!proximity_check(total, scroll_y, vis_height));
}

#[test]
fn test_proximity_near_top_should_not_follow() {
    let total = 200;
    let vis_height = 30;
    let scroll_y = 20;
    assert!(!proximity_check(total, scroll_y, vis_height));
}

#[test]
fn test_proximity_small_viewport_minimum_threshold() {
    let total = 50;
    let vis_height = 6;
    let scroll_y = total - vis_height - 5;
    assert!(proximity_check(total, scroll_y, vis_height));
    let scroll_y = total - vis_height - 6;
    assert!(!proximity_check(total, scroll_y, vis_height));
}

#[test]
fn test_proximity_empty_content_no_follow() {
    assert!(!proximity_check(0, 0, 20));
}

#[test]
fn test_proximity_content_smaller_than_viewport_at_bottom() {
    let total = 10;
    let vis_height = 30;
    assert!(!proximity_check(total, 0, vis_height));
}

// ── 粘性吸底：用户滚动后跟随状态判定测试 ──────────────────────────────

#[test]
fn test_follow_at_bottom_after_scroll() {
    // 滚到真正底部（offset == max_scroll）→ 恢复跟随
    assert!(should_follow_after_user_scroll(80, 80));
    // Down 溢出到底（offset > max_scroll，含 End 的 usize::MAX 哨兵）→ 跟随
    assert!(should_follow_after_user_scroll(80, 81));
    assert!(should_follow_after_user_scroll(80, usize::MAX));
}

#[test]
fn test_follow_at_visual_bottom_with_padding() {
    // [Fix padding] max_scroll 含 mod.rs 的 +SCROLL_PADDING 滚动缓冲：用户滚到
    // 视觉底部（真实内容底 = max_scroll - SCROLL_PADDING）时即恢复跟随，
    // 不再恒差 2 行导致吸底永不恢复。
    assert!(should_follow_after_user_scroll(80, 80 - SCROLL_PADDING));
    assert!(should_follow_after_user_scroll(80, 80 - SCROLL_PADDING + 1));
}

#[test]
fn test_no_follow_when_scrolled_up() {
    // 一向上滚动离开视觉底部（offset < max_scroll - SCROLL_PADDING）→ 退出跟随（浏览模式）
    assert!(!should_follow_after_user_scroll(
        80,
        80 - SCROLL_PADDING - 1
    ));
    assert!(!should_follow_after_user_scroll(80, 0));
}

#[test]
fn test_follow_when_content_fits_viewport() {
    // 内容不满一屏（max_scroll = 0）：offset 只能为 0，视为在底部 → 跟随
    assert!(should_follow_after_user_scroll(0, 0));
    // 短内容（max_scroll ≤ padding）：0 即底部 → 跟随
    assert!(should_follow_after_user_scroll(1, 0));
}

// ── 滚动节流：反向落地与位置转换（纯函数） ─────────────────────────────

#[test]
fn test_is_reverse_direction() {
    // 同向或零 pending：不触发反向落地
    assert!(!is_reverse_direction(0, 3));
    assert!(!is_reverse_direction(3, 3));
    assert!(!is_reverse_direction(-3, -3));
    // 反向：旧 pending 立即落地，再累积新方向
    assert!(is_reverse_direction(3, -3));
    assert!(is_reverse_direction(-3, 3));
}

#[test]
fn test_apply_delta_to_offset_clamps() {
    // 向下不超过 max_scroll（原 scroll_down 无限递增、渲染侧才 clamp）
    assert_eq!(apply_delta_to_offset(10, 3, 12), 12);
    assert_eq!(apply_delta_to_offset(10, 3, 100), 13);
    // 向上不低于 0
    assert_eq!(apply_delta_to_offset(1, -3, 100), 0);
    // 从底部向上滚动
    assert_eq!(apply_delta_to_offset(100, -3, 100), 97);
}

// ── 滚动条几何 / 反推公式测试 ────────────────────────────────────────

fn make_rect(x: u16, y: u16, width: u16, height: u16) -> Rect {
    Rect::new(x, y, width, height)
}

#[test]
fn test_is_scrollbar_column_rightmost() {
    // area: x=10, width=80 → scrollbar 列 = 10 + 80 - 1 = 89
    let area = make_rect(10, 0, 80, 24);
    assert!(is_scrollbar_column(89, area));
}

#[test]
fn test_is_scrollbar_column_text_area() {
    let area = make_rect(10, 0, 80, 24);
    assert!(!is_scrollbar_column(10, area));
    assert!(!is_scrollbar_column(50, area));
    assert!(!is_scrollbar_column(88, area));
}

#[test]
fn test_compute_thumb_geometry_no_overflow_returns_none() {
    let fields = ScrollbarFields {
        content_length: 50,
        position: 0,
        viewport_length: 60,
    };
    let area = make_rect(0, 0, 80, 24);
    assert!(compute_thumb_geometry(&fields, area).is_none());
}

#[test]
fn test_compute_thumb_geometry_thumb_length_clamped_to_one() {
    let fields = ScrollbarFields {
        content_length: 1000,
        position: 0,
        viewport_length: 10,
    };
    let area = make_rect(0, 0, 80, 24);
    let geo = compute_thumb_geometry(&fields, area).expect("应有溢出");
    assert!(geo.thumb_length >= 1);
    assert!(geo.thumb_length <= geo.track_length);
    // content=1000, viewport=10, track=22 → thumb ≈ round(10*22/1009) = round(0.218) = 0 → clamp 到 1
    assert_eq!(geo.thumb_length, 1);
}

#[test]
fn test_compute_thumb_geometry_track_length_minus_two() {
    let fields = ScrollbarFields {
        content_length: 200,
        position: 100,
        viewport_length: 20,
    };
    let area = make_rect(0, 0, 80, 24);
    let geo = compute_thumb_geometry(&fields, area).expect("应有溢出");
    assert_eq!(geo.track_length, 22); // 24 - 2
}

#[test]
fn test_thumb_start_to_position_at_top() {
    let fields = ScrollbarFields {
        content_length: 200,
        position: 100,
        viewport_length: 20,
    };
    let area = make_rect(0, 0, 80, 24);
    let geo = compute_thumb_geometry(&fields, area).expect("应有溢出");
    let pos = thumb_start_to_position(0, &geo);
    assert_eq!(pos, 0);
}

#[test]
fn test_thumb_start_to_position_at_bottom() {
    let fields = ScrollbarFields {
        content_length: 200,
        position: 0,
        viewport_length: 20,
    };
    let area = make_rect(0, 0, 80, 24);
    let geo = compute_thumb_geometry(&fields, area).expect("应有溢出");
    let max_thumb_start = geo.track_length - geo.thumb_length;
    let pos = thumb_start_to_position(max_thumb_start, &geo);
    // thumb 到底 → position 应接近 max_position（=199），允许 ±2 舍入误差
    assert!(
        pos >= geo.max_position.saturating_sub(2),
        "thumb 到底时 position 应接近 max_position，实际 = {}，max = {}",
        pos,
        geo.max_position
    );
}

#[test]
fn test_thumb_start_to_position_clamps_to_max() {
    let fields = ScrollbarFields {
        content_length: 100,
        position: 0,
        viewport_length: 10,
    };
    let area = make_rect(0, 0, 80, 24);
    let geo = compute_thumb_geometry(&fields, area).expect("应有溢出");
    let pos = thumb_start_to_position(usize::MAX, &geo);
    assert_eq!(pos, geo.max_position);
}

#[test]
fn test_round_divide_basic() {
    assert_eq!(round_divide(10, 4), 3); // (10+2)/4 = 3
    assert_eq!(round_divide(8, 4), 2); // (8+2)/4 = 2
    assert_eq!(round_divide(7, 0), 0);
    assert_eq!(round_divide(0, 5), 0);
}

#[test]
fn test_scrollbar_drag_state_default() {
    let s = ScrollbarDragState::default();
    assert!(!s.active);
    assert_eq!(s.thumb_offset, 0);
}

#[test]
fn test_position_to_scroll_y_linear() {
    // max_position=99, max_scroll=90（content=100, viewport=10）
    // position=0 → scroll_y=0
    assert_eq!(position_to_scroll_y(0, 99, 90), 0);
    // position=99 → scroll_y=99*90/99 = 90
    assert_eq!(position_to_scroll_y(99, 99, 90), 90);
    // position=50 → ceil(50*90/99) = ceil(45.45) = 46（ceil 反推，与正向 floor 互逆）
    assert_eq!(position_to_scroll_y(50, 99, 90), 46);
}

#[test]
fn test_position_to_scroll_y_ceil_hits_bottom() {
    // [Fix] 用 ceil 后，thumb 拖到接近底部时就该映射到 max_scroll，
    // 而不是必须恰好拖到 max_position。例如 max_scroll=2, max_position=9 时，
    // position=8（89%）应映射到 scroll_y=2（到底）
    // ceil(8*2/9) = ceil(1.78) = 2
    assert_eq!(position_to_scroll_y(8, 9, 2), 2);
    // ceil(7*2/9) = ceil(1.56) = 2
    assert_eq!(position_to_scroll_y(7, 9, 2), 2);
    // ceil(6*2/9) = ceil(1.33) = 2
    assert_eq!(position_to_scroll_y(6, 9, 2), 2);
    // ceil(4*2/9) = ceil(0.89) = 1
    assert_eq!(position_to_scroll_y(4, 9, 2), 1);
}

#[test]
fn test_position_to_scroll_y_zero_max_position() {
    // max_position=0（content=1）→ 总是 0
    assert_eq!(position_to_scroll_y(0, 0, 0), 0);
    assert_eq!(position_to_scroll_y(5, 0, 10), 0);
}

#[test]
fn test_position_to_scroll_y_zero_max_scroll() {
    // max_scroll=0（content <= viewport）→ 总是 0
    assert_eq!(position_to_scroll_y(5, 100, 0), 0);
}

// ── §8.1 `↓ New output` 指示器（Slice 2）────────────────────────────────

#[test]
fn test_new_output_indicator_browsing_with_new_content() {
    // 浏览态（follow=false）且视口未到真实内容底 → 显示
    assert!(new_output_indicator_active(false, 5, 20, 40));
}

#[test]
fn test_new_output_indicator_following_hides() {
    // 跟随态恒不显示（滚底由 auto_follow 处理，无需指示）
    assert!(!new_output_indicator_active(true, 5, 20, 40));
    assert!(!new_output_indicator_active(true, 0, 20, 40));
}

#[test]
fn test_new_output_indicator_at_bottom_hides() {
    // 视口底 == 内容底（scroll_y + vis == content_bottom）→ 不显示
    assert!(!new_output_indicator_active(false, 20, 20, 40));
    // 超出底部（padding 缓冲区域）→ 不显示
    assert!(!new_output_indicator_active(false, 22, 20, 40));
}

#[test]
fn test_new_output_indicator_boundary_last_row() {
    // 视口恰好露出内容末行（scroll_y + vis == content_bottom - 1）→ 显示
    assert!(new_output_indicator_active(false, 19, 20, 40));
}

#[test]
fn test_new_output_indicator_content_bottom_excludes_padding() {
    // [口径] content_bottom 不含 SCROLL_PADDING 缓冲：max_scroll 含 padding 时，
    // 滚到视觉底部（max_scroll - padding）指示器必须消失——与
    // should_follow_after_user_scroll 的扣缓冲口径对齐。
    let content_bottom = 38; // core+footer 实际行数
    let total_visual = content_bottom + SCROLL_PADDING; // 含 2 行缓冲
    let vis_height = 20;
    let visual_bottom_scroll = total_visual.saturating_sub(vis_height) - SCROLL_PADDING;
    assert_eq!(
        visual_bottom_scroll + vis_height,
        content_bottom,
        "视觉底部 = scroll_y + vis_height == content_bottom"
    );
    assert!(
        !new_output_indicator_active(false, visual_bottom_scroll, vis_height, content_bottom),
        "滚到视觉底部即消失（缓冲行不可见，不算内容）"
    );
    // 上滚 1 行 → 显示
    assert!(new_output_indicator_active(
        false,
        visual_bottom_scroll - 1,
        vis_height,
        content_bottom
    ));
}

// ── [Slice 4 §6.8] Interaction block 锚定（anchor_scroll_target 矩阵）──

/// block 末行超出视口 → 对齐到 block 底部（浏览态与跟随态均生效）。
#[test]
fn test_anchor_scroll_target_aligns_below_viewport() {
    // 视口 10 行，scroll_y=5，block 末行在视觉行 20 → 对齐目标 = 20-10 = 10
    assert_eq!(
        super::anchor_scroll_target(5, 10, 20, 100),
        Some(10),
        "block 超出视口 → 对齐到 block 底部"
    );
}

/// block 已完全在视口内 → 不调整（None）。
#[test]
fn test_anchor_scroll_target_in_viewport_noop() {
    assert_eq!(
        super::anchor_scroll_target(5, 10, 14, 100),
        None,
        "block 末行 ≤ 视口底 → 不动"
    );
    assert_eq!(
        super::anchor_scroll_target(5, 10, 15, 100),
        None,
        "block 末行恰在视口底 → 不动（边界）"
    );
}

/// 对齐目标钳制到 max_scroll（内容不足一屏 / block 接近内容底）。
#[test]
fn test_anchor_scroll_target_clamps_to_max_scroll() {
    // max_scroll=8：对齐目标 10 被钳制到 8
    assert_eq!(super::anchor_scroll_target(2, 10, 20, 8), Some(8));
    // block 在视口内但 max_scroll 更小——先判定超出，再钳制
    assert_eq!(super::anchor_scroll_target(0, 10, 12, 4), Some(2));
}

/// anchor=None（无 pending block）时锚定逻辑不介入——既有路径全绿由
/// run_auto_follow 的既有测试矩阵覆盖（此处锁定纯函数 None 语义）。
#[test]
fn test_anchor_scroll_target_none_semantics() {
    // 视口已覆盖 block → 不调整（等效 anchor 无效果）
    assert_eq!(super::anchor_scroll_target(0, 20, 10, 30), None);
}

// ── entry 单击展开（header 行命中 + 手抖容差）────────────────────────────

fn wm_entry(logical: usize, vstart: usize, vend: usize, slot: usize) -> WrappedLineInfo {
    WrappedLineInfo {
        logical_idx: logical,
        visual_start: vstart,
        visual_end: vend,
        slot_index: slot,
    }
}

/// 单击判定容差矩阵：≤1 行、≤2 列算单击；超过视为拖拽意图（不触发展开）。
#[test]
fn test_is_click_tolerance_matrix() {
    let down = (10usize, 20u16);
    assert!(is_click(down, (10, 20)), "原地 Up = 单击");
    assert!(is_click(down, (11, 20)), "±1 行");
    assert!(is_click(down, (9, 20)), "-1 行");
    assert!(is_click(down, (10, 22)), "±2 列");
    assert!(is_click(down, (10, 18)), "-2 列");
    assert!(!is_click(down, (12, 20)), "2 行 = 拖拽意图");
    assert!(!is_click(down, (10, 23)), "3 列 = 拖拽意图");
    assert!(!is_click(down, (9, 17)), "组合超差");
}

/// 仅首行（header/label 行）可点：正文行（含 wrap 续行）不命中；
/// footer 区域（wrap_map 外）不命中；多 slot 偏移换算正确。
#[test]
fn test_entry_click_target_header_line_only() {
    // slot0：逻辑行 0（header，1 视觉行）+ 逻辑行 1（正文 wrap 成 2 视觉行）；
    // slot1：逻辑行 2（header，1 视觉行，offsets 累加 = 2）
    let wm = vec![
        wm_entry(0, 0, 1, 0),
        wm_entry(1, 1, 3, 0),
        wm_entry(2, 3, 4, 1),
    ];
    let offsets = vec![0usize, 2usize];
    assert_eq!(
        entry_click_target(&wm, &offsets, 0),
        Some((0, 0)),
        "slot0 header"
    );
    assert_eq!(entry_click_target(&wm, &offsets, 1), None, "slot0 正文行");
    assert_eq!(
        entry_click_target(&wm, &offsets, 2),
        None,
        "slot0 正文 wrap 续行"
    );
    assert_eq!(
        entry_click_target(&wm, &offsets, 3),
        Some((1, 0)),
        "slot1 header"
    );
    assert_eq!(entry_click_target(&wm, &offsets, 4), None, "footer 区域");
    assert_eq!(entry_click_target(&[], &[], 0), None, "空 wrap_map");
}

/// header 换行成多视觉行：所属视觉行全部命中首行（都属于 header）。
#[test]
fn test_entry_click_target_wrapped_header_all_visual_rows_hit() {
    let wm = vec![wm_entry(0, 0, 2, 0)];
    let offsets = vec![0usize];
    assert_eq!(entry_click_target(&wm, &offsets, 0), Some((0, 0)));
    assert_eq!(entry_click_target(&wm, &offsets, 1), Some((0, 0)));
}
