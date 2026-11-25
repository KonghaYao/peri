use super::viewport_policy::{clamp_top, follow_top, initial_top, max_scroll};

#[test]
fn max_scroll_never_goes_negative() {
    assert_eq!(max_scroll(100, 20), 80);
    assert_eq!(max_scroll(20, 20), 0);
    assert_eq!(max_scroll(10, 20), 0);
    assert_eq!(max_scroll(0, 0), 0);
}

#[test]
fn clamp_top_collapses_follow_sentinel_and_overscroll() {
    assert_eq!(clamp_top(0, 100, 20), 0);
    assert_eq!(clamp_top(80, 100, 20), 80);
    // 跟随态哨兵与用户过滚都收敛到同一上缘。
    assert_eq!(clamp_top(usize::MAX, 100, 20), 80);
    assert_eq!(clamp_top(90_000, 100_000, 20), 90_000);
    // 内容不足一屏：任何上缘都归零，避免把首行推出视口。
    assert_eq!(clamp_top(usize::MAX, 10, 20), 0);
}

#[test]
fn clamp_top_keeps_viewport_full_after_shrink() {
    // 视口变高（或内容变矮）后旧上缘越界，收敛到新最大上缘而非归零。
    assert_eq!(clamp_top(99_980, 100_000, 100), 99_900);
    // 面板虚拟偏移保留完整高度（不上限截断），只收敛到有效窗口。
    assert_eq!(clamp_top(90_000, 100_000, 20), 90_000);
    assert_eq!(clamp_top(100_000, 100_000, 20), 99_980);
    assert_eq!(clamp_top(50, 10, 20), 0);
}

#[test]
fn initial_top_follows_tail_only_while_running() {
    assert_eq!(initial_top(true, 30, 10), 20);
    assert_eq!(initial_top(false, 30, 10), 0);
    // running 但内容不足一屏：贴尾等价于贴头。
    assert_eq!(initial_top(true, 5, 10), 0);
    assert_eq!(initial_top(false, 5, 10), 0);
}

#[test]
fn follow_top_sticks_to_tail_when_new_source_grows_below() {
    // 旧上缘正好在尾部，源变化且变高 → 贴新尾部。
    assert_eq!(follow_top(20, 30, 10, 35, 10, true), 25);
}

#[test]
fn follow_top_leaves_reader_alone_when_not_at_bottom() {
    // 用户上滚后源变化：位置不动（仍在范围内）。
    assert_eq!(follow_top(12, 30, 10, 35, 10, true), 12);
    // 非源变化（仅几何/心跳）不触发吸底。
    assert_eq!(follow_top(20, 30, 10, 35, 10, false), 20);
}

#[test]
fn follow_top_clamps_when_source_shrinks() {
    // 内容变矮时即使原本贴底也只 clamp，不越过新最大上缘。
    assert_eq!(follow_top(20, 30, 10, 15, 10, true), 5);
}

#[test]
fn follow_top_without_baseline_does_not_stick() {
    // 还没有高度基线（首帧）时不算「在尾部」：只 clamp，贴尾交给 initial_top。
    assert_eq!(follow_top(0, 0, 10, 35, 10, true), 0);
    assert_eq!(follow_top(0, 0, 10, 5, 10, true), 0);
}

#[test]
fn follow_top_treats_viewport_growth_as_at_bottom() {
    // 视口变高使内容恰好不足一屏：旧上缘在尾部 → 新上缘 0。
    assert_eq!(follow_top(10, 20, 10, 20, 30, false), 0);
}
