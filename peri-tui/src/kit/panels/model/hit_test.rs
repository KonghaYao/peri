//! Model 面板视口、命中/绘制同源与 hook 顺序回归。

use super::*;

// ── 视口随面板高度变化 ──────────────────────────────────────────────────────

/// 可见档位数由面板实测内容区行数推导，不再写死 4 档。
#[test]
#[serial]
fn rendered_profiles_follow_panel_height() {
    let guard = ModelPanelGuard::install();
    guard.set_active_alias("fable");

    let short = buffer_text(&mount_panel_frames(SHORT_AREA, 2, |_, _| {}));
    assert!(short.contains("fable ·"), "矮面板至少渲染第一档：{short}");
    assert!(short.contains("opus ·"), "内容区 10 行放得下 2 档：{short}");
    assert!(
        !short.contains("sonnet ·"),
        "内容区 10 行（2+2×3+1）只放得下 2 档，不应渲染第三档：{short}"
    );

    let tall = buffer_text(&mount_panel_frames(TEST_AREA, 2, |_, _| {}));
    assert!(
        tall.contains("haiku ·"),
        "面板变高后必须渲染全部四档，而不是停在矮面板的 2 档：{tall}"
    );
}

// ── 命中与绘制同源 ─────────────────────────────────────────────────────────

/// 点击渲染出的档位卡片 → 切换该档位，且下一帧把它渲染为当前档位。
#[test]
#[serial]
fn click_on_rendered_profile_card_switches_that_profile() {
    let guard = ModelPanelGuard::install();
    guard.set_active_alias("fable");
    let handle = Arc::clone(&guard.handle);

    let buf = mount_panel_frames(TEST_AREA, 2, |_, _| {});
    let (row, _) = rendered_line_of(&buf, "haiku ·");

    let switched = switched();
    let clicked = Rc::clone(&switched);
    let buf = mount_panel_frames(TEST_AREA, 3, move |frame, panel| {
        if frame == 0 {
            let result = panel.event(left_down(row, 5), TEST_AREA, &clicked);
            assert_eq!(result, EventResult::Consumed);
            // 复现「切换落盘成功」后的内存效果：真实动作写的就是这一档
            let idx = clicked.borrow().expect("点击必须触发档位切换");
            handle.write().config.active_alias = PROFILE_KEYS[idx].to_string();
        }
    });

    assert_eq!(
        *switched.borrow(),
        Some(3),
        "点击必须切换被点的那一档（haiku）"
    );
    let text = buffer_text(&buf);
    let (_, haiku_line) = rendered_line_of(&buf, "haiku ·");
    assert!(
        haiku_line.trim_start().starts_with('●'),
        "被点中的档位必须在下一帧渲染为当前档位：\n{text}"
    );
    let (_, fable_line) = rendered_line_of(&buf, "fable ·");
    assert!(
        fable_line.trim_start().starts_with('○'),
        "未选中的档位必须渲染为普通项：\n{text}"
    );
}

/// 矮面板：窗口跟随当前档位滚动，命中取同一窗口——点击「渲染出的那一行」切换的
/// 是同一档（旧实现按写死的 4 档 + 偏移 0 命中，会切到窗口外的另一档）。
#[test]
#[serial]
fn short_panel_hit_matches_scrolled_window() {
    let guard = ModelPanelGuard::install();
    guard.set_active_alias("sonnet");

    let buf = mount_panel_frames(SHORT_AREA, 2, |_, _| {});
    let text = buffer_text(&buf);
    assert!(
        text.contains("sonnet ·") && text.contains("haiku ·"),
        "窗口必须滚到当前档位所在的末尾 2 档：{text}"
    );
    assert!(
        !text.contains("fable ·"),
        "内容区 10 行只放得下 2 档，窗口外的档位不得渲染：{text}"
    );
    let (last_row, _) = rendered_line_of(&buf, "haiku ·");
    let (active_row, _) = rendered_line_of(&buf, "sonnet ·");

    let switched = switched();
    mount_panel_frames(SHORT_AREA, 2, move |frame, panel| {
        if frame == 0 {
            let result = panel.event(left_down(last_row, 5), SHORT_AREA, &switched);
            assert_eq!(result, EventResult::Consumed);
            assert_eq!(
                *switched.borrow(),
                Some(3),
                "点击渲染出的 haiku 卡片必须切换 haiku"
            );
            *switched.borrow_mut() = None;
            panel.event(left_down(active_row, 5), SHORT_AREA, &switched);
            assert_eq!(
                *switched.borrow(),
                Some(2),
                "点击渲染出的 sonnet 卡片必须切换 sonnet"
            );
        }
    });
}

/// 滚动条列（左栏最右 1 列）不参与列表命中。
#[test]
#[serial]
fn click_on_scrollbar_column_does_not_switch() {
    let _guard = ModelPanelGuard::install();
    let switched = switched();
    mount_panel_frames(SHORT_AREA, 2, {
        let switched = Rc::clone(&switched);
        move |_, panel| {
            // 左栏右边界列 = 其 ScrollView 的滚动条列（命中判定按 `< end` 排除）
            let column = super::super::panel_input::left_column_end(SHORT_AREA);
            let result = panel.event(left_down(6, column), SHORT_AREA, &switched);
            assert_eq!(result, EventResult::Consumed, "区域内左键点击仍消费防穿透");
        }
    });
    assert!(switched.borrow().is_none(), "滚动条列点击不得触发档位切换");
}

/// 滚动仲裁注册区域与鼠标命中共用 `AreaTracker`：首帧未测量时 0 尺寸，
/// 之后为实测面板区域（此前用 `use_previous_size()`，与命中区域不同源）。
#[test]
#[serial]
fn scroll_areas_come_from_measured_panel_rect() {
    let _guard = ModelPanelGuard::install();

    mount_panel_frames(TEST_AREA, 2, |frame, _panel| {
        if frame == 1 {
            // 帧 0 组件尚未测量自身区域，注册 0 尺寸而不是猜测的尺寸
            assert_eq!(
                registered_scroll_areas(),
                vec![Rect::default(), Rect::default()]
            );
        }
    });

    let (left, right) = crate::kit::panel_scroll::split_vertical(TEST_AREA, 45);
    assert_eq!(
        registered_scroll_areas(),
        vec![left, right],
        "注册区域必须是 AreaTracker 的实测区域（与鼠标命中同源）"
    );
}

// ── hook 顺序（TUI-HOOK-001） ──────────────────────────────────────────────

/// 跨帧挂载：不同终端高度/焦点状态下 hook 数量与类型不变——ratatui-kit 的
/// `use_hook` 按调用下标取已注册 hook 并做类型校验，顺序偏移会直接 panic。
#[test]
#[serial]
fn hooks_stay_stable_across_frames() {
    let _guard = ModelPanelGuard::install();
    for area in [TEST_AREA, SHORT_AREA, Rect::new(0, 0, 80, 40)] {
        let buf = mount_panel_frames(area, 3, |frame, panel| {
            // 每帧切换焦点与字段光标：走不同的渲染分支
            *panel.right_focus.write() = frame % 2 == 1;
            *panel.right_cursor.write() = frame;
        });
        assert!(
            buffer_text(&buf).contains("Provider"),
            "每个尺寸/焦点组合都必须渲染出右栏字段"
        );
    }
}

/// TUI-HOOK-001（静态面）：组件体内所有 `hooks.use_*` 都在事件注册之前，且事件
/// 注册之前没有任何提前 `return`——hook 数量与顺序不可能随渲染路径变化。
#[test]
fn model_hooks_precede_event_registration_and_returns() {
    let src = include_str!("../model.rs");
    let body = src
        .split("#[path = \"model_test.rs\"]")
        .next()
        .expect("组件模块体");
    let component = body.split("pub fn ModelPanel").nth(1).expect("组件函数体");
    let handler = component
        .find("hooks.use_event_handler_with_options")
        .expect("事件注册存在");

    assert!(
        component[..handler].contains("hooks.use_state")
            && component[..handler].contains("hooks.use_atom")
            && component[..handler].contains("hooks.use_hook")
            && component[..handler].contains("hooks.use_terminal_size"),
        "状态/atom/区域/终端尺寸 hook 都必须在事件注册之前无条件调用"
    );
    assert!(
        component.find("return ").is_none_or(|pos| pos > handler),
        "事件注册之前不得出现提前 return（hook 会被跳过）"
    );
    assert_eq!(
        component[handler..].matches("hooks.use_").count(),
        1,
        "事件注册之后不得再注册 hook"
    );
}
