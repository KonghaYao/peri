//! Browse 列表、删除确认门与配置候选回归。

use super::super::browse_render;
use super::*;
use crate::kit::panel_mouse::hit_row;
use crate::kit::panel_overlay::panel_content_rows;
use ratatui_kit::ratatui::style::Color;

/// 删除确认门绑定确认时的身份：确认期间列表刷新（每 tick 重派生）不改变待删目标。
///
/// 旧实现只存下标，`delete_provider(*cursor.read())` 会在刷新后删到另一个 provider。
#[test]
#[serial]
fn delete_confirms_the_provider_that_was_frozen() {
    let _list = ProviderListGuard::install(vec![provider("alpha"), provider("beta")]);
    let _note = NotificationGuard::install();
    let _ = take_delete_requests();

    let mut frame = 0usize;
    let (tx, rx) = std::sync::mpsc::channel();
    mount_panel_at(4, move |panel| {
        if frame == 0 {
            // Ctrl+D：冻结光标处（下标 0 = alpha）的身份
            let opened = panel.event(
                Event::Key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL)),
                TEST_AREA,
            );
            assert_eq!(opened, EventResult::Consumed);
            assert_eq!(
                panel.confirm_delete_target.read().clone(),
                Some("alpha".to_string()),
                "进入确认时必须冻结待删除身份"
            );
            // 列表刷新：下标 0 变成 beta
            *PROVIDER_LIST.state().write() = vec![provider("beta"), provider("alpha")];
        }
        if frame == 1 {
            let result = panel.event(key(KeyCode::Enter), TEST_AREA);
            tx.send((
                result,
                *panel.mode.read(),
                panel.confirm_delete_target.read().clone(),
            ))
            .expect("测试结果通道仍打开");
        }
        frame += 1;
    });

    let (result, mode, target) = rx.try_recv().expect("确认必须走真实事件入口");
    assert_eq!(result, EventResult::Consumed);
    assert_eq!(mode, LoginPanelMode::Browse, "确认后退出确认门");
    assert_eq!(target, None, "确认门是一次性的，目标随之清空");
    assert_eq!(
        take_delete_requests(),
        vec!["alpha".to_string()],
        "删除必须按确认时冻结的身份发出，而不是刷新后该下标上的 beta"
    );
}

/// 目标已消失（列表刷新 / 他处删除）时不发送请求、不误删其他项，并给出可见提示。
#[test]
#[serial]
fn delete_skips_request_and_hints_when_target_vanished() {
    let _list = ProviderListGuard::install(vec![provider("alpha"), provider("beta")]);
    let _note = NotificationGuard::install();
    let _ = take_delete_requests();

    let mut frame = 0usize;
    let (tx, rx) = std::sync::mpsc::channel();
    mount_panel_at(4, move |panel| {
        if frame == 0 {
            let opened = panel.event(
                Event::Key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL)),
                TEST_AREA,
            );
            assert_eq!(opened, EventResult::Consumed);
            // 目标从列表消失
            *PROVIDER_LIST.state().write() = vec![provider("beta")];
        }
        if frame == 1 {
            let result = panel.event(key(KeyCode::Enter), TEST_AREA);
            tx.send((result, *panel.mode.read()))
                .expect("测试结果通道仍打开");
        }
        frame += 1;
    });

    let (result, mode) = rx.try_recv().expect("确认必须走真实事件入口");
    assert_eq!(result, EventResult::Consumed);
    assert_eq!(mode, LoginPanelMode::Browse);
    assert!(
        take_delete_requests().is_empty(),
        "目标已消失时不得发出删除请求（更不得改成删除当前下标上的 beta）"
    );
    let message = NOTIFICATION
        .state()
        .read()
        .as_ref()
        .map(|note| note.message.clone())
        .expect("目标消失必须给出可见提示");
    assert!(
        message.contains("alpha"),
        "提示必须指明确认时的目标身份，实际：{message}"
    );
}

/// 确认视图显示的是冻结身份，不随列表刷新换成当前下标上的 provider。
#[test]
#[serial]
fn confirm_view_shows_frozen_target_after_refresh() {
    let _list = ProviderListGuard::install(vec![provider("alpha"), provider("beta")]);

    let mut frame = 0usize;
    let buf = mount_panel_at(4, move |panel| {
        if frame == 0 {
            let _ = panel.event(
                Event::Key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL)),
                TEST_AREA,
            );
            // 刷新：下标 0 变成 beta，确认门目标必须仍是 alpha
            *PROVIDER_LIST.state().write() = vec![provider("beta"), provider("alpha")];
        }
        frame += 1;
    });

    let text = buffer_text(&buf);
    assert!(
        text.contains("Provider: alpha"),
        "确认视图必须显示冻结的目标身份：\n{text}"
    );
    assert!(
        !text.contains("Provider: beta"),
        "确认视图不得改成刷新后该下标上的 provider：\n{text}"
    );
}

/// 鼠标点击在确认门内等价于 Enter（click as enter），同样按冻结身份删除。
#[test]
#[serial]
fn click_confirms_frozen_target() {
    let _list = ProviderListGuard::install(vec![provider("alpha"), provider("beta")]);
    let _ = take_delete_requests();

    let mut frame = 0usize;
    mount_panel_at(3, move |panel| {
        if frame == 0 {
            let _ = panel.event(
                Event::Key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL)),
                TEST_AREA,
            );
            *PROVIDER_LIST.state().write() = vec![provider("beta"), provider("alpha")];
        }
        if frame == 1 {
            let result = panel.event(
                Event::Mouse(MouseEvent {
                    kind: MouseEventKind::Down(MouseButton::Left),
                    column: 5,
                    row: 3,
                    modifiers: KeyModifiers::NONE,
                }),
                TEST_AREA,
            );
            assert_eq!(result, EventResult::Consumed);
        }
        frame += 1;
    });

    assert_eq!(
        take_delete_requests(),
        vec!["alpha".to_string()],
        "点击确认与 Enter 同语义：按冻结身份删除"
    );
}

/// 编辑状态异常（Edit 模式但没有编辑状态）时在渲染前回退：本帧渲染的就是 Browse 视图。
///
/// 旧实现在渲染体内 `*mode.write()`：本帧先渲染出 Internal error，违背 TUI-RENDER-001。
#[test]
#[serial]
fn edit_mode_without_state_renders_browse_in_the_same_frame() {
    let _list = ProviderListGuard::install(vec![provider("alpha")]);

    let mut frame = 0usize;
    let (tx, rx) = std::sync::mpsc::channel();
    let buf = mount_panel_at(2, move |panel| {
        if frame == 0 {
            *panel.mode.write() = LoginPanelMode::Edit;
            *panel.edit_state.write() = None;
        }
        if frame == 1 {
            tx.send(*panel.mode.read()).expect("测试结果通道仍打开");
        }
        frame += 1;
    });

    let mode = rx.try_recv().expect("末帧必须上报状态");
    assert_eq!(mode, LoginPanelMode::Browse, "状态异常必须回退到 Browse");
    let text = buffer_text(&buf);
    assert!(
        text.contains("providers configured"),
        "回退发生在渲染之前，本帧即渲染 Browse 视图：\n{text}"
    );
    assert!(
        !text.contains("Internal error"),
        "渲染段不得改写状态（回退由 effect 承担）：\n{text}"
    );
}

/// 含给定 provider 身份的宿主配置——只服务于删除候选的纯函数回归。
fn config_with_providers(ids: &[&str]) -> crate::config::PeriConfig {
    let mut cfg = crate::config::PeriConfig::default();
    cfg.config.providers = ids
        .iter()
        .map(|id| ProviderConfig {
            id: (*id).to_string(),
            provider_type: "anthropic".to_string(),
            ..Default::default()
        })
        .collect();
    cfg
}

/// 删除候选（纯函数）：目标存在才产出「移除后」的副本，且不改源配置。
///
/// 真实删除路径要进程级 `PERI_CONFIG_HANDLE`（测试进程不可安装），这里覆盖候选取舍
/// 这一唯一不依赖句柄的判定：无候选 = 调用方跳过落盘，重复删除不产生写入。
#[test]
fn delete_candidate_prunes_copy_only_when_target_exists() {
    let cfg = config_with_providers(&["alpha", "beta"]);

    let candidate = delete_candidate(&cfg, "alpha").expect("目标存在时必须产出候选");
    assert_eq!(
        candidate
            .config
            .providers
            .iter()
            .map(|p| p.id.as_str())
            .collect::<Vec<_>>(),
        vec!["beta"],
        "候选移除的只能是目标身份"
    );
    assert_eq!(
        cfg.config
            .providers
            .iter()
            .map(|p| p.id.as_str())
            .collect::<Vec<_>>(),
        vec!["alpha", "beta"],
        "候选构造在副本上进行，不得改动源配置"
    );
    assert!(
        delete_candidate(&cfg, "gamma").is_none(),
        "目标已不在配置里时无候选：调用方跳过落盘与发布"
    );
}

// ── Browse 铺满：视口、渲染与命中同源 ────────────────────────────────────────

fn providers(count: usize) -> Vec<ProviderSummary> {
    (0..count)
        .map(|index| provider(&format!("p{index}")))
        .collect()
}

fn provider_with_url(id: &str, url: &str) -> ProviderSummary {
    ProviderSummary {
        base_url: Some(url.to_string()),
        ..provider(id)
    }
}

fn browse_colors() -> browse_render::BrowseRenderColors {
    browse_render::BrowseRenderColors {
        marker: Color::Reset,
        text: Color::Reset,
        dim: Color::Reset,
        muted: Color::Reset,
        success: Color::Reset,
        error: Color::Reset,
        accent: Color::Reset,
    }
}

fn line_text(line: &ratatui_kit::ratatui::text::Line<'_>) -> String {
    line.spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect()
}

/// 面板内容区行数预测：终端 24/40/60 → 18/26/26（主题 max_height=28）。
#[test]
fn content_rows_prediction_matches_panel_overlay() {
    assert_eq!(panel_content_rows(24), 18);
    assert_eq!(panel_content_rows(40), 26);
    assert_eq!(panel_content_rows(60), 26);
}

/// 可见槽位由内容区行数推导；5 项在高度 40 时全部显示，高度 24 时只显示 4 项。
#[test]
fn visible_slots_follow_panel_height() {
    let list = providers(5);

    let short = browse_render::viewport(panel_content_rows(24), list.len(), 0);
    assert_eq!(short.visible_items, 4);
    assert_eq!(short.window(&list).count(), 4, "内容区 18 行只放得下 4 项");

    let tall = browse_render::viewport(panel_content_rows(40), list.len(), 0);
    assert_eq!(tall.visible_items, 6);
    assert_eq!(tall.window(&list).count(), 5);

    let component = browse_render::viewport(22, list.len(), 0);
    assert_eq!(component.visible_items, 5, "内容区 22 行正好显示 5 项");
    assert_eq!(component.scroll_start, 0);

    let minimum = browse_render::viewport(6, 20, 0);
    assert_eq!(
        minimum.visible_items, 1,
        "面板最小高度仍须显示 1 项，不能是 0"
    );
    assert_eq!(minimum.window(&providers(20)).count(), 1);
}

/// 窗口跟随选中项；越界选择按列表长度收敛，不 panic。
#[test]
fn scroll_follows_selection() {
    let items: Vec<usize> = (0..20).collect();

    let middle = browse_render::viewport(22, items.len(), 10);
    assert_eq!(middle.visible_items, 5);
    assert_eq!(middle.scroll_start, 9);
    assert_eq!(
        middle
            .window(&items)
            .map(|(index, _)| index)
            .collect::<Vec<_>>(),
        vec![9, 10, 11, 12, 13]
    );

    let last = browse_render::viewport(22, items.len(), 19);
    assert_eq!(last.scroll_start, 15);
    assert_eq!(
        last.window(&items)
            .map(|(index, _)| index)
            .collect::<Vec<_>>(),
        vec![15, 16, 17, 18, 19]
    );

    let clamped = browse_render::viewport(22, items.len(), 30);
    assert_eq!(clamped.scroll_start, 15, "越界选择按末项计算窗口");
    assert_eq!(clamped.window(&items).count(), 5);

    let empty = browse_render::viewport(22, 0, 3);
    assert_eq!(empty.window(&Vec::<usize>::new()).count(), 0);
    assert_eq!(empty.hit_layout().item_count, 0);
}

/// 绘制窗口与命中布局逐槽位一致：同一 `ListViewport` 产出。
#[test]
fn window_and_hit_layout_agree() {
    let rect = Rect::new(0, 0, 80, 24);
    let items: Vec<usize> = (0..20).collect();
    let viewport = browse_render::viewport(22, items.len(), 10);
    let layout = viewport.hit_layout();

    let visible: Vec<usize> = viewport.window(&items).map(|(index, _)| index).collect();
    assert_eq!(visible, vec![9, 10, 11, 12, 13]);
    for (slot, expected) in visible.iter().enumerate() {
        let visual_row = layout.header_rows + (slot as u16) * layout.item_rows;
        assert_eq!(
            hit_row(rect.y + 1 + visual_row, rect, layout),
            Some(*expected),
            "第 {slot} 个可见槽位命中的全量索引必须与 window 一致"
        );
    }

    // 提示行与底部边框不可点
    assert_eq!(hit_row(rect.y + 1 + 21, rect, layout), None);
    assert_eq!(hit_row(rect.y + 23, rect, layout), None);
}

/// 每项严格 4 行；Base URL 缺省用 `—` 占位，空列表只有空态提示行。
#[test]
fn body_lines_keep_fixed_item_height() {
    let list = vec![
        provider_with_url("with-url", "https://example.test/v1"),
        provider("no-url"),
    ];
    let lines = browse_render::body_lines(
        browse_render::viewport(22, list.len(), 0),
        &list,
        0,
        &browse_colors(),
    );

    assert_eq!(lines.len(), 8, "2 项 × 4 行");
    assert!(line_text(&lines[0]).contains("with-url"));
    assert!(line_text(&lines[1]).contains("api key: configured"));
    assert!(line_text(&lines[2]).contains("base url: https://example.test/v1"));
    assert_eq!(lines[3].width(), 0, "每项第 4 行是分隔空行");

    assert!(line_text(&lines[4]).contains("no-url"));
    assert!(
        line_text(&lines[6]).contains("base url: —"),
        "无 URL 时仍占一行，行高固定：{}",
        line_text(&lines[6])
    );
    assert_eq!(lines[7].width(), 0);

    let empty =
        browse_render::body_lines(browse_render::viewport(22, 0, 0), &[], 0, &browse_colors());
    assert_eq!(empty.len(), 1);
    assert!(line_text(&empty[0]).contains("Ctrl+N"));
}

/// 高度 24：5 项全见，提示行贴内容区最后一行（22），底边框在 23。
#[test]
#[serial]
fn height_24_shows_all_items_and_pins_hint_to_bottom() {
    let buf = {
        let _list = ProviderListGuard::install(providers(5));
        mount_panel_sized(3, 80, 24, |_| {})
    };

    let text = buffer_text(&buf);
    for index in 0..5 {
        assert!(
            text.contains(&format!("p{index}")),
            "高度 24 应显示 p{index}：\n{text}"
        );
    }
    assert!(buffer_line(&buf, 1).contains("5 providers configured"));
    assert!(buffer_line(&buf, 22).contains("↑/↓"), "提示行必须贴底");
    assert!(
        !buffer_line(&buf, 23).contains("Enter"),
        "第 23 行是底边框/路径，不应放提示"
    );
}

/// 高度 20：内容区 18 行只放得下 4 项，第 5 项不可见；提示行仍在 18。
#[test]
#[serial]
fn height_20_shows_four_items_and_hint_at_18() {
    let buf = {
        let _list = ProviderListGuard::install(providers(5));
        mount_panel_sized(3, 80, 20, |_| {})
    };

    let text = buffer_text(&buf);
    assert!(text.contains("p3"));
    assert!(!text.contains("p4"), "第 5 项超出窗口：\n{text}");
    assert!(buffer_line(&buf, 18).contains("↑/↓"));
}

/// 高度 28、宽 120：5 项全见，提示行在 26。
#[test]
#[serial]
fn height_28_shows_all_items_and_hint_at_26() {
    let buf = {
        let _list = ProviderListGuard::install(providers(5));
        mount_panel_sized(3, 120, 28, |_| {})
    };

    let text = buffer_text(&buf);
    for index in 0..5 {
        assert!(text.contains(&format!("p{index}")), "\n{text}");
    }
    assert!(buffer_line(&buf, 26).contains("↑/↓"));
}

/// 高度/宽度矩阵：不 panic，5 项全见，提示行始终在内容区最后一行。
#[test]
#[serial]
fn size_matrix_keeps_hint_on_last_content_row() {
    for (width, height) in [
        (80, 24),
        (100, 24),
        (120, 24),
        (80, 40),
        (100, 40),
        (120, 40),
        (80, 60),
        (100, 60),
        (120, 60),
    ] {
        let buf = {
            let _list = ProviderListGuard::install(providers(5));
            mount_panel_sized(2, width, height, |_| {})
        };
        let text = buffer_text(&buf);
        for index in 0..5 {
            assert!(
                text.contains(&format!("p{index}")),
                "{width}x{height} 应显示 p{index}：\n{text}"
            );
        }
        let hint = buffer_line(&buf, height - 2);
        assert!(
            hint.contains("↑/↓"),
            "{width}x{height} 的提示行必须在 y={}：{hint:?}",
            height - 2
        );
    }
}

/// 列表长度矩阵：0/3/5/20 都按视口裁剪，提示行位置不随列表长度变化。
#[test]
#[serial]
fn list_length_matrix_renders_expected_window() {
    for count in [0usize, 3, 5, 20] {
        let buf = {
            let _list = ProviderListGuard::install(providers(count));
            mount_panel_sized(2, 80, 24, |_| {})
        };
        let text = buffer_text(&buf);
        assert!(
            buffer_line(&buf, 1).contains(&format!("{count} providers configured")),
            "计数行必须反映列表长度"
        );
        assert!(buffer_line(&buf, 22).contains("↑/↓"));

        if count == 0 {
            assert!(text.contains("Ctrl+N"), "空列表必须给出新建提示：\n{text}");
            assert!(!text.contains("p0"));
            continue;
        }

        assert!(text.contains("p0"));
        assert!(text.contains(&format!("p{}", count.saturating_sub(1).min(4))));
        let first_row = (0..22)
            .find(|&y| buffer_line(&buf, y).contains("p0"))
            .expect("非空列表必须渲染首项");
        assert_eq!(first_row, 2, "内容不足时列表顶部对齐（header 下一行）");
        if count == 3 {
            assert!(
                buffer_line(&buf, 14).trim().is_empty(),
                "第 3 项后的剩余空间留在列表体内部"
            );
        }
        if count > 5 {
            assert!(!text.contains("p5"), "20 项时窗口只显示前 5 项：\n{text}");
        }
    }
}

/// 20 项、高度 20：按 ↓ 10 次后窗口为 9..12，选中项 10 带 `>` 标记。
#[test]
#[serial]
fn down_key_scrolls_window_with_selection() {
    let _list = ProviderListGuard::install(providers(20));
    let mut frame = 0usize;
    let buf = mount_panel_sized(3, 80, 20, move |panel| {
        if frame == 0 {
            let area = Rect::new(0, 0, 80, 20);
            for _ in 0..10 {
                let result = panel.event(key(KeyCode::Down), area);
                assert_eq!(result, EventResult::Consumed);
            }
            assert_eq!(*panel.cursor.read(), 10);
        }
        frame += 1;
    });

    let text = buffer_text(&buf);
    for index in [9usize, 10, 11, 12] {
        assert!(text.contains(&format!("p{index}")), "\n{text}");
    }
    assert!(!text.contains("p0"), "窗口已滚离首项：\n{text}");
    assert!(!text.contains("p13"), "窗口只保留 4 项：\n{text}");
    assert!(
        buffer_line(&buf, 6).contains("> p10"),
        "选中项 10 位于窗口第 2 槽：{:?}",
        buffer_line(&buf, 6)
    );
}

/// 每项固定 4 行：有/无 Base URL 的项起始行间隔都是 4。
#[test]
#[serial]
fn items_with_and_without_url_keep_four_row_spacing() {
    let buf = {
        let _list = ProviderListGuard::install(vec![
            provider_with_url("p0", "https://example.test/v1"),
            provider("p1"),
        ]);
        mount_panel_sized(2, 80, 24, |_| {})
    };

    let row_of = |id: &str| {
        (0..24)
            .find(|&y| buffer_line(&buf, y).contains(id))
            .unwrap_or_else(|| panic!("未找到 {id}：\n{}", buffer_text(&buf)))
    };
    let first = row_of("p0");
    let second = row_of("p1");
    assert_eq!(second - first, 4);
    assert!(buffer_line(&buf, first + 2).contains("base url: https://example.test/v1"));
    assert!(buffer_line(&buf, second + 2).contains("base url: —"));
}

/// 窄屏：列表行按宽度裁剪但不换行，提示行仍在最后一行。
#[test]
#[serial]
fn narrow_widths_clip_rows_and_keep_hint_row() {
    for width in [60u16, 80, 120] {
        let buf = {
            let _list = ProviderListGuard::install(providers(5));
            mount_panel_sized(2, width, 24, |_| {})
        };
        assert!(
            buffer_line(&buf, 22).contains("↑/↓"),
            "宽度 {width} 的提示行必须在最后一行"
        );
        assert!(buffer_line(&buf, 2).contains("p0"));
    }
}

/// 点击第 3 个可见项所在行 → 选中其全量索引（2）。
#[test]
#[serial]
fn click_third_visible_item_selects_full_index() {
    let _list = ProviderListGuard::install(providers(5));
    let mut frame = 0usize;
    let (tx, rx) = std::sync::mpsc::channel();
    mount_panel_sized(2, 80, 20, move |panel| {
        if frame == 0 {
            let area = Rect::new(0, 0, 80, 20);
            let result = panel.event(click_at(10, 5), area);
            tx.send((result, *panel.cursor.read()))
                .expect("测试结果通道仍打开");
        }
        frame += 1;
    });

    let (result, cursor) = rx.try_recv().expect("点击必须走真实事件入口");
    assert_eq!(result, EventResult::Consumed);
    assert_eq!(cursor, 2);
}

/// 滚动后的窗口：点击槽位 4 命中 provider 19，点击槽位 0 命中 provider 15。
#[test]
#[serial]
fn click_after_scroll_hits_window_slot() {
    let _list = ProviderListGuard::install(providers(20));
    let mut frame = 0usize;
    let (tx, rx) = std::sync::mpsc::channel();
    mount_panel_sized(2, 80, 24, move |panel| {
        if frame == 0 {
            let area = Rect::new(0, 0, 80, 24);
            *panel.cursor.write() = 19;
            let result_last = panel.event(click_at(18, 5), area);
            let cursor_last = *panel.cursor.read();

            // 同帧再点一次：恢复 Browse（测试进程无配置句柄，进入编辑会回退）
            *panel.mode.write() = LoginPanelMode::Browse;
            *panel.edit_state.write() = None;
            *panel.cursor.write() = 19;
            let result_first = panel.event(click_at(2, 5), area);
            let cursor_first = *panel.cursor.read();

            tx.send((result_last, cursor_last, result_first, cursor_first))
                .expect("测试结果通道仍打开");
        }
        frame += 1;
    });

    let (result_last, cursor_last, result_first, cursor_first) =
        rx.try_recv().expect("点击必须走真实事件入口");
    assert_eq!(result_last, EventResult::Consumed);
    assert_eq!(cursor_last, 19, "槽位 4 对应窗口起点 15 + 4");
    assert_eq!(result_first, EventResult::Consumed);
    assert_eq!(cursor_first, 15);
}

/// 计数行、提示行、边框与滚动条列都不改变选择。
#[test]
#[serial]
fn non_item_rows_do_not_change_selection() {
    let _list = ProviderListGuard::install(providers(2));
    let mut frame = 0usize;
    let (tx, rx) = std::sync::mpsc::channel();
    mount_panel_sized(2, 80, 24, move |panel| {
        if frame == 0 {
            let area = Rect::new(0, 0, 80, 24);
            *panel.cursor.write() = 1;
            for row in [0u16, 1, 16, 22, 23] {
                let result = panel.event(click_at(row, 5), area);
                assert_eq!(result, EventResult::Consumed);
            }
            // 右侧滚动条列即使落在列表行上也不可点
            let result = panel.event(click_at(10, 79), area);
            assert_eq!(result, EventResult::Consumed);
            tx.send((*panel.cursor.read(), *panel.mode.read()))
                .expect("测试结果通道仍打开");
        }
        frame += 1;
    });

    let (cursor, mode) = rx.try_recv().expect("点击必须走真实事件入口");
    assert_eq!(cursor, 1, "非列表行与滚动条列不得改变选择");
    assert_eq!(mode, LoginPanelMode::Browse);
}

/// 空列表：显示计数与空态提示，提示行贴底，空白区域点击不改变选择。
#[test]
#[serial]
fn empty_list_has_hint_and_ignores_body_clicks() {
    let _list = ProviderListGuard::install(Vec::new());
    let mut frame = 0usize;
    let (tx, rx) = std::sync::mpsc::channel();
    let buf = mount_panel_sized(2, 80, 24, move |panel| {
        if frame == 0 {
            let area = Rect::new(0, 0, 80, 24);
            let result = panel.event(click_at(3, 5), area);
            assert_eq!(result, EventResult::Consumed);
            tx.send((*panel.cursor.read(), *panel.mode.read()))
                .expect("测试结果通道仍打开");
        }
        frame += 1;
    });

    let text = buffer_text(&buf);
    assert!(text.contains("0 providers configured"), "\n{text}");
    assert!(text.contains("Ctrl+N"), "\n{text}");
    assert!(buffer_line(&buf, 22).contains("↑/↓"));
    assert_eq!(
        rx.try_recv().expect("点击必须走真实事件入口"),
        (0, LoginPanelMode::Browse)
    );
}

/// 进入 Edit 的三条入口都归零 Browse 阶段残留的 `ScrollViewState` 偏移。
#[test]
#[serial]
fn entering_edit_resets_browse_scroll_offset() {
    let _list = ProviderListGuard::install(providers(3));
    let mut frame = 0usize;
    let _ = mount_panel_sized(3, 80, 24, move |panel| {
        let area = Rect::new(0, 0, 80, 24);
        // 测试进程无配置句柄：每次进入 Edit 后由 effect 回退，这里恢复 Browse 继续下一入口
        if frame > 0 {
            *panel.mode.write() = LoginPanelMode::Browse;
            *panel.edit_state.write() = None;
        }
        match frame {
            0 => {
                panel.sv.write().set_offset(Position::new(0, 7));
                let _ = panel.event(key(KeyCode::Enter), area);
                assert_eq!(panel.sv.read().offset().y, 0, "Enter 进入 Edit 前必须归零");
            }
            1 => {
                panel.sv.write().set_offset(Position::new(0, 5));
                let _ = panel.event(
                    Event::Key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL)),
                    area,
                );
                assert_eq!(panel.sv.read().offset().y, 0, "Ctrl+N 进入 Edit 前必须归零");
            }
            2 => {
                panel.sv.write().set_offset(Position::new(0, 3));
                let _ = panel.event(click_at(2, 5), area);
                assert_eq!(panel.sv.read().offset().y, 0, "鼠标进入 Edit 前必须归零");
            }
            _ => {}
        }
        frame += 1;
    });
}
