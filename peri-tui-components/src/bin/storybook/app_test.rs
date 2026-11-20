use std::time::{Duration, Instant};

use super::{App, dump_buffer, dump_screen, handle_mouse};
use crate::stories::{
    ACCENT, BG, DocumentContent, STORIES, StoryState, document_h_scroll_max, document_line_count,
    document_lines, document_max_scroll, handle_story_mouse, place_caret,
};
use ratatui::crossterm::event::{
    KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::Rect;
use ratatui::style::Modifier;

use peri_tui_components::mermaid::frame::{render_final, render_source};
use peri_tui_components::mermaid::stream::detect_mode;

fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
    MouseEvent {
        kind,
        column,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

#[test]
fn every_story_variant_renders() {
    for (story_index, story) in STORIES.iter().enumerate() {
        for (variant_index, variant) in story.variants.iter().enumerate() {
            let mut app = App::new(story_index);
            app.select_variant(variant_index);
            let screen = dump_screen(&mut app, 100, 32);
            assert!(screen.contains("peri-tui storybook"));
            assert!(screen.contains(story.title));
            assert!(screen.contains(variant.key));
            assert!(screen.lines().count() >= 10);
        }
    }
}

#[test]
fn minimum_size_renders_help_instead_of_panicking() {
    let mut app = App::new(0);
    assert!(dump_screen(&mut app, 30, 8).contains("Terminal too small"));
    assert!(dump_screen(&mut app, 40, 10).contains("peri-tui storybook"));
}

#[test]
fn wide_characters_are_not_padded_in_dumps() {
    let mut app = App::new(0);
    app.select_variant(2);
    assert!(dump_screen(&mut app, 100, 32).contains("你好，世界"));
}

#[test]
fn textarea_click_focuses_and_maps_display_column_to_char() {
    let mut app = App::new(0);
    app.select_variant(0);
    let area = Rect::new(0, 10, 40, 10);
    let click = mouse(MouseEventKind::Down(MouseButton::Left), 2, 10);
    assert!(handle_story_mouse(&mut app.state, &click, area));
    let StoryState::TextArea { focused, .. } = &app.state else {
        panic!("expected textarea state");
    };
    assert!(*focused);

    app.select_variant(2);
    let click = mouse(MouseEventKind::Down(MouseButton::Left), 4, 10);
    assert!(handle_story_mouse(&mut app.state, &click, area));
    let StoryState::TextArea { editor, .. } = &app.state else {
        panic!("expected textarea state");
    };
    // "你好，世界": display column 4 lands after the second CJK character.
    assert_eq!(editor.cursor, 2);
}

#[test]
fn list_click_selects_item_and_excludes_scrollbar_column() {
    let mut app = App::new(1);
    app.select_variant(0);
    let area = Rect::new(0, 10, 40, 12);
    let click = mouse(MouseEventKind::Down(MouseButton::Left), 5, 14);
    assert!(handle_story_mouse(&mut app.state, &click, area));
    let StoryState::List { selected, .. } = &app.state else {
        panic!("expected list state");
    };
    assert_eq!(*selected, 3);

    // 换一个可见条目行点滚动条列：若未排除，这里会选中第 2 项。
    let bar_click = mouse(MouseEventKind::Down(MouseButton::Left), 39, 12);
    assert!(handle_story_mouse(&mut app.state, &bar_click, area));
    let StoryState::List { selected, .. } = &app.state else {
        panic!("expected list state");
    };
    assert_eq!(*selected, 3);
}

#[test]
fn list_wheel_scrolls_view_without_moving_selection() {
    let mut app = App::new(1);
    app.select_variant(0);
    // 12 rows − header − footer → 10 item rows; 60 items → max scroll 50.
    let area = Rect::new(0, 10, 40, 12);
    let wheel = mouse(MouseEventKind::ScrollDown, 5, 10);
    assert!(handle_story_mouse(&mut app.state, &wheel, area));
    let StoryState::List {
        selected, scroll, ..
    } = &app.state
    else {
        panic!("expected list state");
    };
    assert_eq!((*selected, *scroll), (0, 3));

    // Clicks map through the scrolled viewport.
    let click = mouse(MouseEventKind::Down(MouseButton::Left), 5, 11);
    assert!(handle_story_mouse(&mut app.state, &click, area));
    let StoryState::List {
        selected, scroll, ..
    } = &app.state
    else {
        panic!("expected list state");
    };
    assert_eq!((*selected, *scroll), (3, 3));

    // Header and footer rows stay inert.
    for row in [area.y, area.bottom() - 1] {
        let inert = mouse(MouseEventKind::Down(MouseButton::Left), 5, row);
        assert!(handle_story_mouse(&mut app.state, &inert, area));
        let StoryState::List { selected, .. } = &app.state else {
            panic!("expected list state");
        };
        assert_eq!(*selected, 3);
    }

    // The wheel clamps at the last page and at the top.
    for _ in 0..40 {
        handle_story_mouse(&mut app.state, &wheel, area);
    }
    let StoryState::List { scroll, .. } = &app.state else {
        panic!("expected list state");
    };
    assert_eq!(*scroll, 50);
    let wheel_up = mouse(MouseEventKind::ScrollUp, 5, 10);
    for _ in 0..40 {
        handle_story_mouse(&mut app.state, &wheel_up, area);
    }
    let StoryState::List { scroll, .. } = &app.state else {
        panic!("expected list state");
    };
    assert_eq!(*scroll, 0);
}

#[test]
fn list_last_variant_reveals_the_selection_at_the_real_viewport() {
    let mut app = App::new(1);
    app.select_variant(2); // last
    let screen = dump_screen(&mut app, 40, 10);
    let StoryState::List {
        selected,
        scroll,
        reveal_last,
        ..
    } = &app.state
    else {
        panic!("expected list state");
    };
    // 40x10 的预览只有 3 个条目行：60 - 3 = 57 才是真正的底部偏移。
    assert_eq!(*selected, 59);
    assert_eq!(*scroll, 57, "初始偏移按真实视口计算");
    assert!(!*reveal_last, "贴底只应用一次");
    assert!(
        screen.contains("tests/panel_"),
        "选中项必须在 18 列预览内可见: {screen}"
    );

    // 之后滚轮可以离开底部，重绘不会把它拉回来。
    let area = app.metrics.story_area;
    let wheel_up = mouse(MouseEventKind::ScrollUp, area.x, area.y);
    assert!(handle_story_mouse(&mut app.state, &wheel_up, area));
    let _ = dump_screen(&mut app, 40, 10);
    let StoryState::List { scroll, .. } = &app.state else {
        panic!("expected list state");
    };
    assert_eq!(*scroll, 54, "首帧之后滚轮自由移动");
}

#[test]
fn list_scrollbar_thumb_reaches_the_track_end_only_at_max_scroll() {
    let mut app = App::new(1);
    app.select_variant(0);
    let _ = dump_screen(&mut app, 100, 32);
    let area = app.metrics.story_area;
    let bar_x = area.right() - 1;
    let thumb_bottom = |app: &mut App| -> u16 {
        let buffer = dump_buffer(app, 100, 32);
        (area.y..area.bottom())
            .rev()
            .find(|&y| buffer[(bar_x, y)].bg == ACCENT)
            .expect("滚动条 thumb 可见")
    };

    // 100x32 有 25 个条目行：max_scroll = 35。offset 33 仍有 2 行可滚。
    let wheel = mouse(MouseEventKind::ScrollDown, area.x, area.y);
    for _ in 0..11 {
        assert!(handle_story_mouse(&mut app.state, &wheel, area));
    }
    let _ = dump_screen(&mut app, 100, 32);
    let StoryState::List { scroll, .. } = &app.state else {
        panic!("expected list state");
    };
    assert_eq!(*scroll, 33);
    assert!(
        thumb_bottom(&mut app) < area.bottom() - 2,
        "offset 33 时 thumb 不应贴到轨道底"
    );

    assert!(handle_story_mouse(&mut app.state, &wheel, area));
    let _ = dump_screen(&mut app, 100, 32);
    let StoryState::List { scroll, .. } = &app.state else {
        panic!("expected list state");
    };
    assert_eq!(*scroll, 35, "列表滚到最后一页");
    assert_eq!(
        thumb_bottom(&mut app),
        area.bottom() - 2,
        "max_scroll 时 thumb 贴到轨道底"
    );
}

#[test]
fn scrollbar_wheel_moves_position() {
    let mut app = App::new(2);
    app.select_variant(1);
    let area = Rect::new(0, 10, 40, 10);
    let wheel = mouse(MouseEventKind::ScrollDown, 1, 10);
    assert!(handle_story_mouse(&mut app.state, &wheel, area));
    let StoryState::Scrollbar {
        position, total, ..
    } = &app.state
    else {
        panic!("expected scrollbar state");
    };
    assert_eq!(*position, 53);
    assert_eq!(*total, 100);

    // Pressing empty track jumps to the centred offset and starts a drag.
    let click = mouse(MouseEventKind::Down(MouseButton::Left), 0, 12);
    assert!(handle_story_mouse(&mut app.state, &click, area));
    let StoryState::Scrollbar { position, grab, .. } = &app.state else {
        panic!("expected scrollbar state");
    };
    assert_eq!(*position, 13, "track cell 1 of 7 maps to 13/90");
    assert_eq!(*grab, Some(0), "track press grabs the thumb centre");
}

#[test]
fn scrollbar_thumb_press_keeps_offset_and_drag_reaches_bottom() {
    let mut app = App::new(2);
    app.select_variant(1); // offset 50 / content 100 / viewport 10 → thumb at track cell 4
    let area = Rect::new(0, 10, 40, 10);
    let press = mouse(MouseEventKind::Down(MouseButton::Left), 0, 15);
    assert!(handle_story_mouse(&mut app.state, &press, area));
    let StoryState::Scrollbar { position, grab, .. } = &app.state else {
        panic!("expected scrollbar state");
    };
    assert_eq!(*position, 50, "grabbing the thumb must not jump");
    assert_eq!(*grab, Some(0));

    let drag = mouse(MouseEventKind::Drag(MouseButton::Left), 30, 18);
    assert!(handle_story_mouse(&mut app.state, &drag, area));
    let StoryState::Scrollbar { position, .. } = &app.state else {
        panic!("expected scrollbar state");
    };
    assert_eq!(*position, 90, "last movable cell is the real bottom");
}

#[test]
fn scrollbar_release_outside_preview_clears_grab() {
    let mut app = App::new(2);
    app.select_variant(1);
    let area = Rect::new(0, 10, 40, 10);
    let press = mouse(MouseEventKind::Down(MouseButton::Left), 0, 15);
    assert!(handle_story_mouse(&mut app.state, &press, area));
    let StoryState::Scrollbar { grab, .. } = &app.state else {
        panic!("expected scrollbar state");
    };
    assert_eq!(*grab, Some(0), "press inside the thumb starts the drag");

    let release = mouse(MouseEventKind::Up(MouseButton::Left), 90, 2);
    assert!(
        handle_story_mouse(&mut app.state, &release, area),
        "outside release must be handled while grabbing"
    );
    let StoryState::Scrollbar { grab, .. } = &app.state else {
        panic!("expected scrollbar state");
    };
    assert_eq!(*grab, None, "release outside the preview clears the grab");

    // 普通的外部点击仍然忽略，避免误吞其他区域的事件。
    let outside_click = mouse(MouseEventKind::Down(MouseButton::Left), 90, 2);
    assert!(!handle_story_mouse(&mut app.state, &outside_click, area));
}

#[test]
fn textarea_click_past_last_visual_row_keeps_cursor() {
    let mut app = App::new(0);
    app.select_variant(2);
    let area = Rect::new(0, 10, 40, 10);
    let StoryState::TextArea { editor, .. } = &app.state else {
        panic!("expected textarea state");
    };
    let before = editor.cursor;

    let click = mouse(MouseEventKind::Down(MouseButton::Left), 4, 18);
    assert!(handle_story_mouse(&mut app.state, &click, area));
    let StoryState::TextArea { editor, .. } = &app.state else {
        panic!("expected textarea state");
    };
    assert_eq!(editor.cursor, before);
}

#[test]
fn textarea_click_outside_narrow_viewport_is_ignored() {
    let mut app = App::new(0);
    app.select_variant(4);
    // The narrow variant reserves 14 columns; clicks to its right are not its input.
    let area = Rect::new(0, 10, 40, 10);
    let click = mouse(MouseEventKind::Down(MouseButton::Left), 30, 10);
    assert!(!handle_story_mouse(&mut app.state, &click, area));
}

#[test]
fn nav_click_switches_story_and_variant() {
    let mut app = App::new(0);
    let _ = dump_screen(&mut app, 100, 32);
    let story_rows = app.metrics.story_rows;
    assert!(story_rows.height >= 2);
    handle_mouse(
        &mut app,
        mouse(
            MouseEventKind::Down(MouseButton::Left),
            story_rows.x + 1,
            story_rows.y + 1,
        ),
    );
    assert_eq!(app.story, 1);
    assert_eq!(app.variant, 0);

    let _ = dump_screen(&mut app, 100, 32);
    let variant_rows = app.metrics.variant_rows;
    assert!(variant_rows.height >= 2);
    handle_mouse(
        &mut app,
        mouse(
            MouseEventKind::Down(MouseButton::Left),
            variant_rows.x + 1,
            variant_rows.y + 1,
        ),
    );
    assert_eq!(app.variant, 1);
}

#[test]
fn header_keeps_the_story_count_when_the_right_label_does_not_fit() {
    let mut app = App::new(0);
    let count = format!("1 / {}", STORIES.len());

    let narrow = dump_screen(&mut app, 40, 10);
    let first = narrow.lines().next().expect("header row");
    assert!(first.contains(&count), "窄终端计数被覆盖: {first:?}");
    assert!(
        !first.contains("peri-tui-components"),
        "40 列放不下右侧标签: {first:?}"
    );

    let wide = dump_screen(&mut app, 100, 32);
    let first = wide.lines().next().expect("header row");
    assert!(first.contains(&count), "100 列计数可见: {first:?}");
    assert!(
        first.contains("peri-tui-components"),
        "100 列两段都放得下: {first:?}"
    );
}

#[test]
fn narrow_sidebar_rows_stay_inside_the_border_and_keep_the_variant_count() {
    let mut app = App::new(0);
    let buffer = dump_buffer(&mut app, 40, 10);
    let rows = app.metrics.story_rows;
    let border_x = app.metrics.nav_area.right() - 1;
    assert!(rows.height > 0, "40x10 仍显示 story 行");
    for y in rows.y..rows.bottom() {
        let story = &STORIES[app.metrics.story_first + (y - rows.y) as usize];
        assert_eq!(
            buffer[(border_x, y)].symbol(),
            "│",
            "story 行不得写到右边框 ({y})"
        );
        let text: String = (rows.x..rows.right())
            .map(|x| buffer[(x, y)].symbol())
            .collect();
        assert!(
            text.trim_end()
                .ends_with(&format!("{:>2}", story.variants.len())),
            "{} variant 计数必须可见: {text:?}",
            story.key
        );
    }

    // 滚轮滚出 variant 行后，这些行同样截断在边框内。
    let nav = app.metrics.nav_area;
    for _ in 0..3 {
        handle_mouse(
            &mut app,
            mouse(MouseEventKind::ScrollDown, nav.x + 1, nav.y + 1),
        );
    }
    let _ = dump_screen(&mut app, 40, 10);
    let rows = app.metrics.variant_rows;
    assert!(rows.height > 0, "滚轮后显示 variant 行");
    let buffer = dump_buffer(&mut app, 40, 10);
    for y in rows.y..rows.bottom() {
        assert_eq!(
            buffer[(border_x, y)].symbol(),
            "│",
            "variant 行不得写到右边框 ({y})"
        );
    }
}

fn story_index(key: &str) -> usize {
    STORIES
        .iter()
        .position(|story| story.key == key)
        .expect("story registered")
}

#[test]
fn rich_text_document_covers_markdown_features() {
    let mut app = App::new(story_index("markdown"));
    app.select_variant(0); // rich-text
    let screen = dump_screen(&mut app, 100, 200);
    for needle in [
        "Peri 富文本渲染",
        "inline code",
        "Peri 文档 (https://example.com/docs)",
        "Peri 组件文档 (https://example.com/components)",
        "& < >",
        "*不是斜体*",
        "Ctrl+C",
        "Setext 二级标题",
        "• 嵌套三级",
        "3. 有序列表从 3 开始编号",
        "│ fn render_markdown",
        "│ {",
        "左对齐",
        "────",
        "[Image: 本地架构图] (docs/architecture.png)",
        "[Remote image: 远程示意图] (https://example.com/diagram.png)",
        "组件预览",
    ] {
        assert!(screen.contains(needle), "missing {needle}: {screen}");
    }
}

#[test]
fn markdown_table_renders_unicode_grid() {
    let mut app = App::new(story_index("markdown"));
    app.select_variant(4); // table
    let screen = dump_screen(&mut app, 100, 40);
    for drawing in ["┌", "┬", "├", "┼", "└"] {
        assert!(screen.contains(drawing), "missing {drawing}: {screen}");
    }
    for cell in ["左对齐", "居中", "右对齐", "1,024", "空单元格"] {
        assert!(screen.contains(cell), "missing {cell}: {screen}");
    }
}

#[test]
fn markdown_code_block_keeps_source_text() {
    let mut app = App::new(story_index("markdown"));
    app.select_variant(3); // code
    let screen = dump_screen(&mut app, 100, 45);
    assert!(
        screen.contains("fn render_markdown(source: &str)"),
        "{screen}"
    );
    assert!(
        screen.contains("parse_markdown(source, 80, palette, base_fg)"),
        "{screen}"
    );
    assert!(screen.contains("+ new line"), "{screen}");
    assert!(
        screen.contains("unknown info string falls back to plain text"),
        "{screen}"
    );
}

#[test]
fn openui_components_render_from_genui_fences() {
    let mut app = App::new(story_index("openui"));

    app.select_variant(8); // progress
    let progress = dump_screen(&mut app, 100, 32);
    assert!(progress.contains("解析 70%"), "{progress}");
    assert!(progress.contains('━'), "{progress}");

    app.select_variant(10); // button
    let button = dump_screen(&mut app, 100, 32);
    assert!(button.contains("继续"), "{button}");
    assert!(button.contains("查看文档"), "{button}");

    app.select_variant(11); // bar-chart
    let chart = dump_screen(&mut app, 100, 32);
    assert!(chart.contains('█'), "{chart}");
    assert!(chart.contains("Markdown"), "{chart}");
    assert!(chart.contains(" 42"), "{chart}");

    app.select_variant(12); // sparkline
    let sparkline = dump_screen(&mut app, 100, 32);
    assert!(sparkline.contains("延迟抖动"), "{sparkline}");
    assert!(sparkline.contains('▁'), "{sparkline}");
    assert!(sparkline.contains('█'), "{sparkline}");
}

#[test]
fn openui_button_shows_pressed_style_until_mouse_up_or_cancel() {
    let mut app = App::new(story_index("openui"));
    let variant = app
        .current()
        .variants
        .iter()
        .position(|variant| variant.key == "button")
        .unwrap();
    app.select_variant(variant);
    let pressed_index = |app: &App| match &app.state {
        StoryState::Document { pressed_button, .. } => *pressed_button,
        _ => panic!("应为文档预览"),
    };

    let before = dump_buffer(&mut app, 100, 32);
    let area = app.metrics.story_area;
    let (x, y) = (area.top()..area.bottom())
        .flat_map(|y| (area.left()..area.right()).map(move |x| (x, y)))
        .find(|&(x, y)| before[(x, y)].symbol() == "继")
        .expect("主按钮必须出现在预览中");
    let normal = &before[(x, y)];
    assert!(normal.modifier.contains(Modifier::REVERSED));
    assert_eq!(pressed_index(&app), None);

    let down = mouse(MouseEventKind::Down(MouseButton::Left), x, y);
    handle_mouse(&mut app, down);
    assert_eq!(pressed_index(&app), Some(0));
    let held = dump_buffer(&mut app, 100, 32);
    assert_eq!(held[(x, y)].symbol(), normal.symbol());
    assert_ne!(held[(x, y)].style(), normal.style());
    assert!(held[(x, y)].modifier.contains(Modifier::BOLD));
    assert!(!held[(x, y)].modifier.contains(Modifier::REVERSED));
    assert_eq!(held[(x + 5, y)], before[(x + 5, y)]);

    handle_mouse(
        &mut app,
        mouse(MouseEventKind::Drag(MouseButton::Left), x, area.bottom()),
    );
    assert_eq!(pressed_index(&app), Some(0), "拖拽期间保留按下态");
    handle_mouse(&mut app, mouse(MouseEventKind::Moved, x, area.bottom()));
    assert_eq!(
        pressed_index(&app),
        None,
        "终端丢失 release 后 Moved 必须清除按下态"
    );
    assert_eq!(dump_buffer(&mut app, 100, 32), before);
    handle_mouse(
        &mut app,
        mouse(MouseEventKind::Down(MouseButton::Left), x, y),
    );
    assert_eq!(pressed_index(&app), Some(0));
    handle_mouse(
        &mut app,
        mouse(MouseEventKind::Up(MouseButton::Left), x, area.bottom()),
    );
    assert_eq!(pressed_index(&app), None);
    assert_eq!(dump_buffer(&mut app, 100, 32), before);

    for cancel in [
        mouse(MouseEventKind::Down(MouseButton::Left), x + 5, y),
        mouse(MouseEventKind::Down(MouseButton::Left), x, area.bottom()),
        mouse(MouseEventKind::ScrollDown, x, y),
    ] {
        handle_mouse(&mut app, down);
        assert_eq!(pressed_index(&app), Some(0));
        handle_mouse(&mut app, cancel);
        assert_eq!(pressed_index(&app), None);
        assert_eq!(dump_buffer(&mut app, 100, 32), before);
    }
}

#[test]
fn document_button_hit_accounts_for_segment_gap_card_and_scroll() {
    let mut app = App::new(story_index("openui"));
    app.state = StoryState::Document {
        content: DocumentContent::Markdown(
            "正文\n\n```gen-ui\nroot = Card([Button(\"继续\", @ToAssistant(\"go\"))])\n```\n\n结尾",
        ),
        scroll: 0,
        h_scroll: 0,
        pressed_button: None,
        h_drag: None,
        layout: None,
    };
    let area = Rect::new(4, 7, 25, 4);
    let wheel = mouse(MouseEventKind::ScrollDown, area.x, area.y);
    assert!(handle_story_mouse(&mut app.state, &wheel, area));
    let mut buffer = ratatui::buffer::Buffer::empty(area);
    crate::stories::render_story(&mut app.state, area, &mut buffer);
    let (x, y) = (area.top()..area.bottom())
        .flat_map(|y| (area.left()..area.right()).map(move |x| (x, y)))
        .find(|&(x, y)| buffer[(x, y)].symbol() == "继")
        .expect("滚动后应看得到卡片里的按钮");
    assert_eq!(x, area.x + 3, "卡片边框、卡片留白与按钮留白各占一列");
    assert!(handle_story_mouse(
        &mut app.state,
        &mouse(MouseEventKind::Down(MouseButton::Left), x, y),
        area,
    ));
    assert!(matches!(
        app.state,
        StoryState::Document {
            pressed_button: Some(0),
            scroll: 1..,
            ..
        }
    ));
    crate::stories::render_story(&mut app.state, area, &mut buffer);
    assert!(buffer[(x, y)].modifier.contains(Modifier::BOLD));
    assert!(!buffer[(x, y)].modifier.contains(Modifier::REVERSED));
}

#[test]
fn document_wheel_scrolls_three_rows_and_clamps() {
    let mut app = App::new(story_index("markdown"));
    app.select_variant(2); // lists: taller than the viewport
    let area = Rect::new(0, 10, 40, 6);

    let StoryState::Document {
        content: DocumentContent::Markdown(source),
        scroll,
        ..
    } = &app.state
    else {
        panic!("expected document state");
    };
    assert_eq!(*scroll, 0);
    let source = *source;
    let max_scroll = document_max_scroll(source, area.width as usize, area.height as usize);
    assert!(max_scroll > 3, "fixture must overflow the viewport");

    let wheel = mouse(MouseEventKind::ScrollDown, 5, 10);
    assert!(handle_story_mouse(&mut app.state, &wheel, area));
    let StoryState::Document { scroll, .. } = &app.state else {
        panic!("expected document state");
    };
    assert_eq!(*scroll, 3);

    for _ in 0..40 {
        handle_story_mouse(&mut app.state, &wheel, area);
    }
    let StoryState::Document { scroll, .. } = &app.state else {
        panic!("expected document state");
    };
    assert_eq!(*scroll, max_scroll);

    let wheel_up = mouse(MouseEventKind::ScrollUp, 5, 10);
    for _ in 0..40 {
        handle_story_mouse(&mut app.state, &wheel_up, area);
    }
    let StoryState::Document { scroll, .. } = &app.state else {
        panic!("expected document state");
    };
    assert_eq!(*scroll, 0);

    // Left clicks inside the document are consumed but stay inert.
    let click = mouse(MouseEventKind::Down(MouseButton::Left), 5, 10);
    assert!(handle_story_mouse(&mut app.state, &click, area));
    let StoryState::Document { scroll, .. } = &app.state else {
        panic!("expected document state");
    };
    assert_eq!(*scroll, 0);
}

#[test]
fn rich_text_applies_inline_styles() {
    let initial = STORIES[story_index("markdown")].variants[0].initial;
    let StoryState::Document {
        content: DocumentContent::Markdown(source),
        ..
    } = initial()
    else {
        panic!("expected document state");
    };
    let lines = document_lines(source, 68);

    // Wrapping splits styled runs into per-grapheme spans, so resolve the style of
    // the span that covers the first character of the needle.
    let style_at = |needle: &str| {
        for line in &lines {
            let text: String = line
                .spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect();
            if let Some(byte_index) = text.find(needle) {
                let char_index = text[..byte_index].chars().count();
                let mut offset = 0;
                for span in &line.spans {
                    offset += span.content.chars().count();
                    if char_index < offset {
                        return span.style;
                    }
                }
            }
        }
        panic!("missing styled text {needle:?}");
    };

    assert!(style_at("粗体").add_modifier.contains(Modifier::BOLD));
    assert!(style_at("斜体").add_modifier.contains(Modifier::ITALIC));
    let bold_italic = style_at("粗斜体").add_modifier;
    assert!(bold_italic.contains(Modifier::BOLD) && bold_italic.contains(Modifier::ITALIC));
    assert!(
        style_at("删除线")
            .add_modifier
            .contains(Modifier::CROSSED_OUT)
    );
    assert!(
        style_at("Peri 文档")
            .add_modifier
            .contains(Modifier::UNDERLINED)
    );
    assert!(style_at("inline code").fg.is_some());
    assert!(
        style_at("Peri 富文本渲染")
            .add_modifier
            .contains(Modifier::BOLD)
    );

    let rule = "─".repeat(68);
    assert!(
        lines
            .iter()
            .any(|line| line.spans.iter().any(|span| span.content == rule))
    );
}

fn variant_index(story: usize, key: &str) -> usize {
    STORIES[story]
        .variants
        .iter()
        .position(|variant| variant.key == key)
        .expect("variant registered")
}

fn streaming_cursor(app: &App) -> (usize, &'static str) {
    let StoryState::Streaming(document) = &app.state else {
        panic!("expected streaming state");
    };
    (document.cursor(), document.source())
}

#[test]
fn streaming_variants_dump_the_same_mid_stream_frame() {
    for (story_key, note) in [
        (
            "markdown",
            "逐块到达的 Markdown 渐进渲染；dump 固定在中段。",
        ),
        (
            "openui",
            "逐字符到达的 GenUI 围栏：闭合前按文本降级，闭合后渲染组件。",
        ),
    ] {
        let story = story_index(story_key);
        let variant = variant_index(story, "streaming");
        let mut first = App::new(story);
        first.select_variant(variant);
        let mid = dump_screen(&mut first, 100, 32);
        let mut second = App::new(story);
        second.select_variant(variant);
        assert_eq!(
            mid,
            dump_screen(&mut second, 100, 32),
            "{story_key}: 固定中段必须逐字节一致"
        );
        assert!(mid.contains(note), "{story_key}: note 可见");

        let (cursor, source) = streaming_cursor(&first);
        assert!(source.is_char_boundary(cursor));
        if story_key == "markdown" {
            let percent = cursor * 100 / source.len();
            assert!(
                (45..=60).contains(&percent),
                "markdown 初始进度 {percent}% 应落在 45%~60%"
            );
            assert!(mid.contains("代码块"), "已渲染的完整块应可见");
            assert!(mid.contains("fn s"), "正在增长的代码块应可见");
        } else {
            assert!(mid.contains("流式卡片"), "半截围栏应解析出已到达的组件");
            assert!(mid.contains("gen-ui:"), "未闭合参数应按文本降级");
        }
    }
}

#[test]
fn streaming_auto_plays_to_the_complete_document() {
    let cases = [("markdown", "整篇文档抵达末尾"), ("openui", "生成进度")];
    for (story_key, tail) in cases {
        let mut app = streaming_app(story_key);
        // 先渲染一次建立布局缓存：起播后 cursor 变化必须让它失效，画面才会增长。
        let mid = dump_screen(&mut app, 100, 32);
        let area = app.metrics.story_area;
        let mid_body = body_text(&mid, area);
        let auto = crate::stories::streaming::test_progress_layout(area)
            .0
            .expect("100x32 下 auto 按钮可见");
        let (from, source) = streaming_cursor(&app);
        assert!(!crate::stories::streaming::is_animating(&app.state));

        let click = mouse(MouseEventKind::Down(MouseButton::Left), auto.x + 1, auto.y);
        assert!(handle_story_mouse(&mut app.state, &click, area));
        assert_eq!(streaming_cursor(&app).0, from, "点击按钮本身不 seek");
        let play = crate::stories::streaming::autoplay(&app.state).expect("点击后开始播放");
        assert_eq!(play.from, from, "{story_key}: 从当前 cursor 起播");
        assert!(crate::stories::streaming::is_animating(&app.state));
        let up = mouse(MouseEventKind::Up(MouseButton::Left), auto.x + 1, auto.y);
        assert!(handle_story_mouse(&mut app.state, &up, area));

        // 1.5 秒 → 约一半；误差不超过源码长度的 2%。
        crate::stories::streaming::tick_at(
            &mut app.state,
            play.start + Duration::from_millis(1500),
        );
        let (half, _) = streaming_cursor(&app);
        assert!(
            from < half && half < source.len(),
            "{story_key}: 中点应在两端之间（{from} < {half} < {}）",
            source.len()
        );
        let expected = from + (source.len() - from) / 2;
        let tolerance = source.len() * 2 / 100;
        assert!(
            half.abs_diff(expected) <= tolerance,
            "{story_key}: 1.5s 偏差 {} 超过 2% 源码长度（{}）",
            half.abs_diff(expected),
            tolerance
        );
        assert!(
            crate::stories::streaming::is_animating(&app.state),
            "{story_key}: 播放中"
        );
        // 同一 100x32 尺寸下只比正文（去掉进度行），证明新文本真的进了正文。
        let half_screen = dump_screen(&mut app, 100, 32);
        let half_body = body_text(&half_screen, area);
        assert_ne!(
            mid_body, half_body,
            "{story_key}: 播放期间正文随 cursor 增长"
        );

        // 播放中再点一次：以当前 cursor 重新计时 3 秒。
        let click = mouse(MouseEventKind::Down(MouseButton::Left), auto.x + 1, auto.y);
        assert!(handle_story_mouse(&mut app.state, &click, area));
        let replay = crate::stories::streaming::autoplay(&app.state).expect("重新播放");
        assert_eq!(replay.from, half, "{story_key}: 再次点击从当前 cursor 重播");

        crate::stories::streaming::tick_at(&mut app.state, replay.start + Duration::from_secs(3));
        assert_eq!(streaming_cursor(&app), (source.len(), source));
        assert!(
            !crate::stories::streaming::is_animating(&app.state),
            "{story_key}: 到点清空播放态"
        );
        // 到点后的 tick 无副作用。
        crate::stories::streaming::tick_at(&mut app.state, replay.start + Duration::from_secs(30));
        assert_eq!(streaming_cursor(&app), (source.len(), source));

        // 完成后的正文继续增长；滚到底后在同一 100x32 画面里应看到结尾文本。
        let done = dump_screen(&mut app, 100, 32);
        assert_ne!(
            half_body,
            body_text(&done, area),
            "{story_key}: 完成后正文再次增长"
        );
        let wheel = mouse(MouseEventKind::ScrollDown, area.x, area.y);
        for _ in 0..40 {
            handle_story_mouse(&mut app.state, &wheel, area);
        }
        let end_body = body_text(&dump_screen(&mut app, 100, 32), area);
        assert!(
            end_body.contains(tail),
            "{story_key}: 结尾文本 {tail:?} 应在同一尺寸的正文中"
        );
        if story_key == "openui" {
            assert!(!end_body.contains("gen-ui:"), "围栏闭合后不再降级");
            assert!(end_body.contains("继续"), "闭合后渲染出真实组件");
        }
    }
}

#[test]
fn mermaid_static_variants_render_diagrams() {
    let story = story_index("mermaid");
    let mut static_variants = 0;
    for (index, variant) in STORIES[story].variants.iter().enumerate() {
        let mut app = App::new(story);
        app.select_variant(index);
        let source = match &app.state {
            StoryState::Document {
                content: DocumentContent::Mermaid { source, .. },
                ..
            } => *source,
            _ => {
                assert_eq!(
                    variant.key, "streaming",
                    "{} 不是 Mermaid 文档",
                    variant.key
                );
                continue;
            }
        };
        static_variants += 1;
        // 每个静态变体都必须画出字符图：既不能退回源码，也不能走大图概览。
        let mode = detect_mode(source, true);
        assert!(
            mode.renders_when_closed(),
            "{} 的图型 {mode:?} 不可渲染",
            variant.key
        );
        let lines = render_source(source, 200)
            .unwrap_or_else(|error| panic!("{}: {error}", variant.key))
            .into_lines();
        assert!(
            lines.iter().any(|line| {
                line.contains('┌')
                    || line.contains('╭')
                    || line.contains('├')
                    || line.contains('└')
                    || line.contains('●')
                    || line.contains('█')
                    || line.contains('│')
            }),
            "{} 没有画出字符图:\n{}",
            variant.key,
            lines.join("\n")
        );
        assert!(
            !lines.iter().any(|line| line.contains("节点")),
            "{} 退回了概览:\n{}",
            variant.key,
            lines.join("\n")
        );
    }
    assert_eq!(static_variants, 17, "静态图型覆盖");
}

/// 从已完成文档里取 Mermaid 原文。
fn mermaid_source(state: &StoryState) -> &'static str {
    match state {
        StoryState::Document {
            content:
                DocumentContent::Mermaid {
                    source,
                    terminal: true,
                },
            ..
        } => source,
        _ => panic!("expected a finished Mermaid document"),
    }
}

fn h_scroll(state: &StoryState) -> usize {
    match state {
        StoryState::Document { h_scroll, .. } => *h_scroll,
        _ => panic!("expected document state"),
    }
}

fn row_text(buf: &ratatui::buffer::Buffer, area: Rect, row: u16) -> String {
    (area.left()..area.right())
        .map(|x| buf[(x, row)].symbol())
        .collect()
}

#[test]
fn mermaid_diagrams_keep_their_rendered_width_instead_of_wrapping() {
    let story = story_index("mermaid");
    for (index, variant) in STORIES[story].variants.iter().enumerate() {
        if variant.key == "streaming" {
            continue;
        }
        let mut app = App::new(story);
        app.select_variant(index);
        let source = mermaid_source(&app.state);
        let content = DocumentContent::Mermaid {
            source,
            terminal: true,
        };
        // 足够高，纵向滚动条不会参与；行数只能来自渲染器本身。
        let (width, height) = (80, 200);
        let frame = render_final(source, detect_mode(source, true), width)
            .unwrap_or_else(|error| panic!("{}: {error}", variant.key));
        assert_eq!(
            document_line_count(content, width, height),
            frame.lines().len(),
            "{} 的字符图不能折行",
            variant.key
        );
        let widest = frame
            .lines()
            .iter()
            .map(|line| ratatui::text::Line::from(line.as_str()).width())
            .max()
            .unwrap_or(0);
        assert_eq!(
            document_h_scroll_max(content, width, height),
            widest.saturating_sub(width),
            "{} 的横向溢出量应等于最宽行与列宽之差",
            variant.key
        );
    }
}

#[test]
fn mermaid_horizontal_scroll_uses_wheel_and_draggable_bar() {
    let mut app = App::new(story_index("mermaid"));
    let area = Rect::new(0, 0, 60, 20);
    let content = match &app.state {
        StoryState::Document { content, .. } => *content,
        _ => panic!("expected document state"),
    };
    let max = document_h_scroll_max(content, area.width as usize, area.height as usize);
    assert!(max > 20, "官方流程图应远超 60 列视口");

    let mut buf = ratatui::buffer::Buffer::empty(area);
    crate::stories::render_story(&mut app.state, area, &mut buf);
    let bar_row = row_text(&buf, area, area.bottom() - 1);
    assert!(
        bar_row.contains('◄') && bar_row.contains('►'),
        "底部应有横向滚动条: {bar_row:?}"
    );
    let top_border = row_text(&buf, area, area.y + 1);

    // 横向滚轮按列推进，两端 clamp；内容真的按列左移。
    let right = mouse(MouseEventKind::ScrollRight, 2, 2);
    assert!(handle_story_mouse(&mut app.state, &right, area));
    assert_eq!(h_scroll(&app.state), 8);
    let mut shifted = ratatui::buffer::Buffer::empty(area);
    crate::stories::render_story(&mut app.state, area, &mut shifted);
    assert_ne!(
        row_text(&shifted, area, area.y + 1),
        top_border,
        "滚动后同一行应显示不同的列"
    );
    for _ in 0..40 {
        handle_story_mouse(&mut app.state, &right, area);
    }
    assert_eq!(h_scroll(&app.state), max);
    let left = mouse(MouseEventKind::ScrollLeft, 2, 2);
    for _ in 0..40 {
        handle_story_mouse(&mut app.state, &left, area);
    }
    assert_eq!(h_scroll(&app.state), 0);

    // Shift+滚轮等价于横向滚轮：逐列推进而不是纵向滚动，两端同样 clamp。
    let shift_down = MouseEvent {
        kind: MouseEventKind::ScrollDown,
        column: 2,
        row: 2,
        modifiers: KeyModifiers::SHIFT,
    };
    assert!(handle_story_mouse(&mut app.state, &shift_down, area));
    assert_eq!(h_scroll(&app.state), 8, "Shift+滚轮应横向平移");
    for _ in 0..40 {
        handle_story_mouse(&mut app.state, &shift_down, area);
    }
    assert_eq!(h_scroll(&app.state), max);
    let shift_up = MouseEvent {
        kind: MouseEventKind::ScrollUp,
        column: 2,
        row: 2,
        modifiers: KeyModifiers::SHIFT,
    };
    for _ in 0..40 {
        handle_story_mouse(&mut app.state, &shift_up, area);
    }
    assert_eq!(h_scroll(&app.state), 0);

    // 按在轨道右端：thumb 跳到最右并开始拖拽；拖回左端回到 0。
    let bar_y = area.bottom() - 1;
    let press = mouse(
        MouseEventKind::Down(MouseButton::Left),
        area.right() - 3,
        bar_y,
    );
    assert!(handle_story_mouse(&mut app.state, &press, area));
    assert_eq!(h_scroll(&app.state), max);
    let drag = mouse(MouseEventKind::Drag(MouseButton::Left), area.x + 1, bar_y);
    assert!(handle_story_mouse(&mut app.state, &drag, area));
    assert_eq!(h_scroll(&app.state), 0);
    // 拖出左边界仍停在 0；松开后拖拽态结束，散落的 Drag 不再消费。
    let drag_out = mouse(MouseEventKind::Drag(MouseButton::Left), area.x, bar_y);
    assert!(handle_story_mouse(&mut app.state, &drag_out, area));
    assert_eq!(h_scroll(&app.state), 0);
    let up = mouse(MouseEventKind::Up(MouseButton::Left), area.x, bar_y);
    assert!(handle_story_mouse(&mut app.state, &up, area));
    let stray = mouse(MouseEventKind::Drag(MouseButton::Left), area.x + 10, bar_y);
    assert!(!handle_story_mouse(&mut app.state, &stray, area));

    // 终端丢失 release 后只剩 Moved：按下态与 thumb 拖拽一起清掉。
    let press = mouse(
        MouseEventKind::Down(MouseButton::Left),
        area.right() - 3,
        bar_y,
    );
    assert!(handle_story_mouse(&mut app.state, &press, area));
    assert_eq!(h_scroll(&app.state), max);
    let moved = mouse(MouseEventKind::Moved, area.x + 20, bar_y);
    assert!(!handle_story_mouse(&mut app.state, &moved, area));
    let stray = mouse(MouseEventKind::Drag(MouseButton::Left), area.x + 1, bar_y);
    assert!(!handle_story_mouse(&mut app.state, &stray, area));
    assert_eq!(h_scroll(&app.state), max, "Moved 之后散落的 Drag 不得移动");
}

#[test]
fn one_row_mermaid_pans_horizontally_without_a_scrollbar() {
    let mut app = App::new(story_index("mermaid"));
    let area = Rect::new(0, 0, 20, 1);
    let content = match &app.state {
        StoryState::Document { content, .. } => *content,
        _ => panic!("expected document state"),
    };
    let max = document_h_scroll_max(content, area.width as usize, area.height as usize);
    assert!(max > 20, "20 列视口应明显放不下官方流程图");

    let mut buf = ratatui::buffer::Buffer::empty(area);
    crate::stories::render_story(&mut app.state, area, &mut buf);
    assert!(
        !row_text(&buf, area, area.y).contains('◄'),
        "一行高的视口没有空间渲染横向滚动条"
    );

    // 没有条也要能平移，否则被裁掉的列永远看不到。
    let right = mouse(MouseEventKind::ScrollRight, 2, 0);
    assert!(handle_story_mouse(&mut app.state, &right, area));
    assert_eq!(h_scroll(&app.state), 8);
    let left = mouse(MouseEventKind::ScrollLeft, 2, 0);
    assert!(handle_story_mouse(&mut app.state, &left, area));
    assert_eq!(h_scroll(&app.state), 0);
    let shift_down = MouseEvent {
        kind: MouseEventKind::ScrollDown,
        column: 2,
        row: 0,
        modifiers: KeyModifiers::SHIFT,
    };
    assert!(handle_story_mouse(&mut app.state, &shift_down, area));
    assert_eq!(h_scroll(&app.state), 8, "Shift+滚轮同样不受滚动条影响");
}

#[test]
fn markdown_documents_stay_wrapped_without_a_horizontal_bar() {
    let mut app = App::new(story_index("markdown"));
    app.select_variant(0);
    let area = Rect::new(0, 0, 40, 6);
    let content = match &app.state {
        StoryState::Document { content, .. } => *content,
        _ => panic!("expected document state"),
    };
    assert_eq!(document_h_scroll_max(content, 40, 6), 0);
    let right = mouse(MouseEventKind::ScrollRight, 2, 2);
    assert!(!handle_story_mouse(&mut app.state, &right, area));
    // 没有横向条时 Shift+滚轮退回纵向滚动，横向偏移保持 0。
    let shift_down = MouseEvent {
        kind: MouseEventKind::ScrollDown,
        column: 2,
        row: 2,
        modifiers: KeyModifiers::SHIFT,
    };
    assert!(handle_story_mouse(&mut app.state, &shift_down, area));
    assert_eq!(h_scroll(&app.state), 0);

    let mut buf = ratatui::buffer::Buffer::empty(area);
    crate::stories::render_story(&mut app.state, area, &mut buf);
    let bottom = row_text(&buf, area, area.bottom() - 1);
    assert!(!bottom.contains('◄'), "Markdown 不应出现横向滚动条");
}

#[test]
fn mermaid_streaming_shows_tail_and_uses_shared_mouse_playback() {
    use crate::stories::streaming::{autoplay, is_animating, test_progress_layout, tick_at};

    let mut app = streaming_app("mermaid");
    let initial = dump_screen(&mut app, 100, 32);
    assert_eq!(initial, dump_screen(&mut streaming_app("mermaid"), 100, 32));
    assert!(initial.contains("客户端"), "已确认的边应显示字符图");
    assert!(initial.contains("未确认源码"));
    assert!(initial.contains("Gateway --> Worker[工作线程]"));
    assert!(initial.contains("未闭合"));

    let area = app.metrics.story_area;
    let (auto, bar, _) = test_progress_layout(area);
    let auto = auto.expect("默认宽度显示 auto 按钮");
    let click = mouse(MouseEventKind::Down(MouseButton::Left), auto.x, auto.y);
    handle_mouse(&mut app, click);
    let play = autoplay(&app.state).expect("点击后开始播放");
    tick_at(&mut app.state, play.start + Duration::from_secs(3));
    let (cursor, source) = streaming_cursor(&app);
    assert_eq!(cursor, source.len());
    assert!(!is_animating(&app.state));
    let final_screen = dump_screen(&mut app, 100, 32);
    assert!(final_screen.contains("100%"));
    assert!(final_screen.contains("字符图"));
    assert!(!final_screen.contains("未确认源码"));

    handle_mouse(&mut app, click);
    assert!(is_animating(&app.state));
    handle_mouse(
        &mut app,
        mouse(MouseEventKind::Down(MouseButton::Left), bar.x, bar.y),
    );
    assert!(!is_animating(&app.state), "轨道按下立即取消播放");
    handle_mouse(
        &mut app,
        mouse(
            MouseEventKind::Drag(MouseButton::Left),
            bar.right() - 1,
            bar.y,
        ),
    );
    handle_mouse(
        &mut app,
        mouse(
            MouseEventKind::Up(MouseButton::Left),
            bar.right() - 1,
            bar.y,
        ),
    );
    assert_eq!(streaming_cursor(&app).0, source.len());
    assert_eq!(dump_screen(&mut app, 100, 32), final_screen);
}

#[test]
fn streaming_mermaid_horizontal_thumb_drags_without_touching_progress() {
    let mut app = streaming_app("mermaid");
    // 首帧渲染填充 story_area；60 列窗口的预览区只有 28 列，
    // 中途字符图会溢出，底部出现横向滚动条。
    let _ = dump_screen(&mut app, 60, 32);
    let area = app.metrics.story_area;
    let (cursor_before, source, ..) = streaming_cursor(&app);

    let mut buf = ratatui::buffer::Buffer::empty(area);
    crate::stories::render_story(&mut app.state, area, &mut buf);
    let bar_row = (area.top()..area.bottom())
        .find(|&row| row_text(&buf, area, row).contains('◄'))
        .expect("流式字符图应有横向滚动条");
    assert_eq!(streaming_h_scroll(&app), 0);

    // 按在轨道右端：thumb 跳到最右；拖回左端必须真的改变横向偏移。
    let press = mouse(
        MouseEventKind::Down(MouseButton::Left),
        area.right() - 3,
        bar_row,
    );
    handle_mouse(&mut app, press);
    let grabbed = streaming_h_scroll(&app);
    assert!(grabbed > 0, "按在轨道右端应跳到最右");
    let drag = mouse(MouseEventKind::Drag(MouseButton::Left), area.x + 1, bar_row);
    handle_mouse(&mut app, drag);
    assert!(streaming_h_scroll(&app) < grabbed, "拖回左端应减小横向偏移");
    handle_mouse(
        &mut app,
        mouse(MouseEventKind::Up(MouseButton::Left), area.x + 1, bar_row),
    );
    // 横向拖拽不掺和进度：光标一动不动。
    assert_eq!(streaming_cursor(&app), (cursor_before, source));
}

#[test]
fn streaming_tick_without_auto_never_advances() {
    for story_key in ["markdown", "openui"] {
        let mut app = streaming_app(story_key);
        let before = streaming_cursor(&app);
        assert!(!crate::stories::streaming::is_animating(&app.state));
        // 停住不动 10 秒：没有点击 auto 就不得有任何自动推进。
        crate::stories::streaming::tick_at(
            &mut app.state,
            Instant::now() + Duration::from_secs(10),
        );
        assert_eq!(
            streaming_cursor(&app),
            before,
            "{story_key}: 空闲 tick 不得自动播放"
        );
    }

    // 非 streaming 状态不看时钟也不变。
    let mut state = (STORIES[0].variants[0].initial)();
    crate::stories::streaming::tick_at(&mut state, Instant::now() + Duration::from_secs(10));
    assert!(!crate::stories::streaming::is_animating(&state));
    let StoryState::TextArea { editor, .. } = &state else {
        panic!("expected textarea state");
    };
    assert!(editor.text.is_empty());
}

#[test]
fn typing_edits_the_focused_textarea() {
    let mut state = (STORIES[0].variants[1].initial)();
    let before = match &state {
        StoryState::TextArea { editor, .. } => editor.text.clone(),
        other => panic!(
            "expected textarea state, got {}",
            std::any::type_name_of_val(other)
        ),
    };
    for key in [
        KeyEvent::new(KeyCode::Char('你'), KeyModifiers::NONE),
        KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE),
        KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE),
    ] {
        assert!(
            !super::handle_key(&mut state, key),
            "typing must not quit the storybook"
        );
    }
    // `q` is text inside the text area, never a quit key.
    assert!(!super::handle_key(
        &mut state,
        KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE)
    ));
    let StoryState::TextArea { editor, .. } = &state else {
        panic!("expected textarea state");
    };
    assert!(editor.text.ends_with("你hiq"), "text: {:?}", editor.text);
    assert!(editor.text.len() > before.len());

    // Editing keys keep working through the same path.
    assert!(!super::handle_key(
        &mut state,
        KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE)
    ));
    assert!(!super::handle_key(
        &mut state,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)
    ));
    let StoryState::TextArea { editor, .. } = &state else {
        panic!("expected textarea state");
    };
    assert!(editor.text.ends_with("你hi\n"), "text: {:?}", editor.text);

    // Exit keys still work while the text area owns the keyboard.
    assert!(super::handle_key(
        &mut state,
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)
    ));
}

#[test]
fn unfocused_textarea_does_not_swallow_the_quit_key() {
    let mut state = (STORIES[0].variants[0].initial)();
    assert!(matches!(state, StoryState::TextArea { focused: false, .. }));
    assert!(
        super::handle_key(
            &mut state,
            KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE)
        ),
        "未聚焦的 TextArea 不得吞掉 q"
    );
    let StoryState::TextArea { editor, .. } = &state else {
        panic!("expected textarea state");
    };
    assert!(editor.text.is_empty(), "未聚焦时 q 不应插入文本");

    assert!(super::handle_key(
        &mut state,
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)
    ));
    assert!(super::handle_key(
        &mut state,
        KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)
    ));

    // 聚焦时 q 仍是文本；Esc/Ctrl-C 之外只有编辑键。
    let mut focused = (STORIES[0].variants[1].initial)();
    assert!(
        !super::handle_key(
            &mut focused,
            KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE)
        ),
        "聚焦的 TextArea 中 q 是文本"
    );
    let StoryState::TextArea { editor, .. } = &focused else {
        panic!("expected textarea state");
    };
    assert!(editor.text.ends_with('q'));
}

#[test]
fn navigation_keys_do_not_switch_stories() {
    let mut state = (STORIES[1].variants[1].initial)();
    for key in [
        KeyCode::Left,
        KeyCode::Right,
        KeyCode::Up,
        KeyCode::Down,
        KeyCode::Char('1'),
        KeyCode::Char('g'),
        KeyCode::Tab,
    ] {
        assert!(
            !super::handle_key(&mut state, KeyEvent::new(key, KeyModifiers::NONE)),
            "{key:?} must not quit"
        );
    }
    assert!(super::handle_key(
        &mut state,
        KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE)
    ));
    assert!(super::handle_key(
        &mut state,
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)
    ));
    assert!(super::handle_key(
        &mut state,
        KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)
    ));
}

// ---------------------------------------------------------------------------
// 流式进度条
// ---------------------------------------------------------------------------

fn streaming_app(story_key: &str) -> App {
    let story = story_index(story_key);
    let variant = variant_index(story, "streaming");
    let mut app = App::new(story);
    app.select_variant(variant);
    app
}

/// `(cursor, source, scroll, pressed_button)`；后两者用于证明进度条不掺和文档交互。
fn streaming_progress(app: &App) -> (usize, &'static str, usize, Option<usize>) {
    let StoryState::Streaming(document) = &app.state else {
        panic!("expected streaming state");
    };
    (
        document.cursor(),
        document.source(),
        document.scroll(),
        document.pressed_button(),
    )
}

fn streaming_h_scroll(app: &App) -> usize {
    let StoryState::Streaming(document) = &app.state else {
        panic!("expected streaming state");
    };
    document.h_scroll()
}

fn progress_label(cursor: usize, source: &str) -> String {
    let rounded = (cursor * 100 + source.len() / 2) / source.len();
    let percent = if cursor < source.len() {
        rounded.min(99)
    } else {
        100
    };
    format!("{percent:>4}%")
}

/// 进度条行的 dump 文本：标签在最右端，`dump_screen` 去行尾空白不会削掉它。
fn progress_line(app: &App, screen: &str) -> String {
    let bar = crate::stories::streaming::test_bar_area(app.metrics.story_area);
    screen
        .lines()
        .nth(usize::from(bar.y))
        .unwrap_or_default()
        .to_owned()
}

/// 进度条行之外的正文文本：同一尺寸下比较，避免 resize 重建布局混入结论。
fn body_text(screen: &str, area: Rect) -> String {
    let body_end = area.bottom().saturating_sub(1) as usize;
    screen
        .lines()
        .enumerate()
        .filter(|(row, _)| (area.y as usize..body_end).contains(row))
        .map(|(_, line)| line)
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn streaming_progress_bar_dump_is_deterministic() {
    for (story_key, range) in [("markdown", 50..=65), ("openui", 62..=78)] {
        let mut first = streaming_app(story_key);
        let screen = dump_screen(&mut first, 100, 32);
        let mut second = streaming_app(story_key);
        assert_eq!(
            screen,
            dump_screen(&mut second, 100, 32),
            "{story_key}: 进度条初始帧必须逐字节一致"
        );

        let (cursor, source, scroll, pressed) = streaming_progress(&first);
        assert_eq!(
            (scroll, pressed),
            (0, None),
            "{story_key}: dump 不产生交互状态"
        );
        let label = progress_label(cursor, source);
        let percent: usize = label.trim_end_matches('%').trim().parse().unwrap();
        assert!(
            range.contains(&percent),
            "{story_key}: 初始进度 {percent}% 应落在 {range:?}"
        );

        let line = progress_line(&first, &screen);
        assert!(line.contains('█'), "{story_key}: 已到达部分: {line:?}");
        assert!(line.contains('░'), "{story_key}: 未到达轨道: {line:?}");
        assert!(
            line.contains(&label),
            "{story_key}: 百分比标签 {label:?}: {line:?}"
        );
    }
}

#[test]
fn streaming_progress_bar_clicks_seek_the_source() {
    for (story_key, tail) in [("markdown", "整篇文档抵达末尾"), ("openui", "生成进度")]
    {
        let mut app = streaming_app(story_key);
        let _ = dump_screen(&mut app, 100, 32);
        let area = app.metrics.story_area;
        let bar = crate::stories::streaming::test_bar_area(area);
        assert!(bar.width > 20, "{story_key}: 轨道宽度 {}", bar.width);

        let mut previous = 0;
        for fraction in [25u16, 75, 100] {
            let column = bar.x + (bar.width * fraction / 100).min(bar.width - 1);
            let click = mouse(MouseEventKind::Down(MouseButton::Left), column, bar.y);
            assert!(
                handle_story_mouse(&mut app.state, &click, area),
                "{story_key}: 进度条行内点击应被消费"
            );
            let (cursor, source, scroll, pressed) = streaming_progress(&app);
            assert!(
                source.is_char_boundary(cursor),
                "{story_key}: {fraction}% 落在 UTF-8 边界"
            );
            assert!(
                cursor > previous,
                "{story_key}: {fraction}% 列推进 cursor（{previous} → {cursor}）"
            );
            assert_eq!(
                (scroll, pressed),
                (0, None),
                "{story_key}: 点击不改滚动/按钮态"
            );
            previous = cursor;
            let up = mouse(MouseEventKind::Up(MouseButton::Left), column, bar.y);
            assert!(handle_story_mouse(&mut app.state, &up, area));
        }

        let (cursor, source, scroll, pressed) = streaming_progress(&app);
        assert_eq!(cursor, source.len(), "{story_key}: 100% 列到达源码末尾");
        assert_eq!((scroll, pressed), (0, None));

        // 100% 时整条轨道都是已到达色，标签读数固定为 100%。
        let screen = dump_screen(&mut app, 100, 32);
        let line = progress_line(&app, &screen);
        assert!(
            !line.contains('░'),
            "{story_key}: 100% 无未到达轨道: {line:?}"
        );
        assert!(
            line.contains(" 100%"),
            "{story_key}: 标签应为 100%: {line:?}"
        );

        // 结尾文本只在完整文档出现；用高画面确认尾部确实抵达。
        let full = dump_screen(&mut app, 100, 200);
        assert!(full.contains(tail), "{story_key}: 结尾文本 {tail:?} 可见");
    }
}

#[test]
fn streaming_progress_bar_drag_tracks_the_column_until_up() {
    let mut app = streaming_app("markdown");
    let _ = dump_screen(&mut app, 100, 32);
    let area = app.metrics.story_area;
    let bar = crate::stories::streaming::test_bar_area(area);
    assert!(bar.width > 20, "轨道宽度 {}", bar.width);

    let down = mouse(
        MouseEventKind::Down(MouseButton::Left),
        bar.x + bar.width / 4,
        bar.y,
    );
    assert!(handle_story_mouse(&mut app.state, &down, area));
    let (down_cursor, _, _, _) = streaming_progress(&app);

    // 纵向离开进度条行仍按列继续拖拽。
    let drag = mouse(
        MouseEventKind::Drag(MouseButton::Left),
        bar.x + bar.width / 2,
        bar.y.saturating_sub(2),
    );
    assert!(
        handle_story_mouse(&mut app.state, &drag, area),
        "拖拽中的 Drag 应被消费"
    );
    let (drag_cursor, _, scroll, pressed) = streaming_progress(&app);
    assert!(drag_cursor > down_cursor, "拖拽推进 cursor");
    assert_eq!((scroll, pressed), (0, None));

    // 两端越界按列 clamp。
    let beyond = mouse(
        MouseEventKind::Drag(MouseButton::Left),
        bar.right() + 20,
        bar.y,
    );
    assert!(handle_story_mouse(&mut app.state, &beyond, area));
    let (end_cursor, source, _, _) = streaming_progress(&app);
    assert_eq!(end_cursor, source.len(), "右端越界 clamp 到源码末尾");
    assert!(source.is_char_boundary(end_cursor));

    let before = mouse(
        MouseEventKind::Drag(MouseButton::Left),
        bar.x.saturating_sub(10),
        bar.y,
    );
    assert!(handle_story_mouse(&mut app.state, &before, area));
    let (zero_cursor, _, _, _) = streaming_progress(&app);
    assert_eq!(zero_cursor, 0, "左端越界 clamp 到 0");

    let up = mouse(MouseEventKind::Up(MouseButton::Left), bar.x, bar.y);
    assert!(handle_story_mouse(&mut app.state, &up, area));
    let after = mouse(
        MouseEventKind::Drag(MouseButton::Left),
        bar.x + bar.width * 3 / 4,
        bar.y,
    );
    assert!(
        !handle_story_mouse(&mut app.state, &after, area),
        "未拖拽的 Drag 返回 false"
    );
    let (after_cursor, _, _, _) = streaming_progress(&app);
    assert_eq!(after_cursor, 0, "Up 之后的 Drag 不改 cursor");
}

#[test]
fn streaming_progress_row_never_presses_document_buttons() {
    let mut app = streaming_app("openui");
    let _ = dump_screen(&mut app, 100, 60);
    let area = app.metrics.story_area;
    let last_doc_row = area.bottom() - 1;
    let bar = crate::stories::streaming::test_bar_area(area);
    assert_eq!(bar.y, last_doc_row, "轨道就是预览区最后一行");

    // 拖到 100%：完整文档（含 GenUI 按钮）一次可见，热区坐标与真实布局一致。
    let seek = mouse(
        MouseEventKind::Down(MouseButton::Left),
        bar.right() - 1,
        bar.y,
    );
    assert!(handle_story_mouse(&mut app.state, &seek, area));
    let up = mouse(
        MouseEventKind::Up(MouseButton::Left),
        bar.right() - 1,
        bar.y,
    );
    assert!(handle_story_mouse(&mut app.state, &up, area));
    let (cursor, source, _, _) = streaming_progress(&app);
    assert_eq!(cursor, source.len(), "拖到 100% 显示完整文档");
    let _ = dump_screen(&mut app, 100, 60);

    // 先在文档区探出按钮热区：证明这份文档确实有可命中的按钮。
    let mut button_cell = None;
    'probe: for _ in 0..40 {
        for row in area.y..last_doc_row {
            for column in area.x..area.right().saturating_sub(1) {
                let down = mouse(MouseEventKind::Down(MouseButton::Left), column, row);
                handle_story_mouse(&mut app.state, &down, area);
                let pressed = streaming_progress(&app).3;
                let up = mouse(MouseEventKind::Up(MouseButton::Left), column, row);
                handle_story_mouse(&mut app.state, &up, area);
                if pressed.is_some() {
                    button_cell = Some((column, row));
                    break 'probe;
                }
            }
        }
        let wheel = mouse(MouseEventKind::ScrollDown, area.x, area.y);
        handle_story_mouse(&mut app.state, &wheel, area);
    }
    let (_button_column, button_row) = button_cell.expect("完整文档的按钮热区应可命中");
    assert!(button_row < last_doc_row, "按钮热区位于文档区");

    let before = streaming_progress(&app);
    let click = mouse(
        MouseEventKind::Down(MouseButton::Left),
        bar.x + bar.width / 2,
        last_doc_row,
    );
    assert!(
        handle_story_mouse(&mut app.state, &click, area),
        "进度条行点击被消费"
    );
    let after = streaming_progress(&app);
    assert_eq!(after.3, None, "进度条行点击不得命中按钮热区");
    assert_eq!(after.2, before.2, "进度条行点击不改滚动");
    assert_ne!(after.0, before.0, "进度条行点击只改进度");
}

#[test]
fn streaming_document_area_of_one_row_skips_the_progress_bar() {
    let mut app = streaming_app("markdown");
    let area = Rect::new(0, 0, 24, 1);
    let mut buf = ratatui::buffer::Buffer::empty(area);
    crate::stories::render_story(&mut app.state, area, &mut buf);

    let mut text = String::new();
    for y in area.y..area.bottom() {
        for x in area.x..area.right() {
            text.push_str(buf[(x, y)].symbol());
        }
    }
    assert!(
        !text.contains('░') && !text.contains('█'),
        "高度 1 不画进度条: {text:?}"
    );
    assert!(!text.trim().is_empty(), "高度 1 仍渲染文档首行: {text:?}");

    // 高度 2 是边界：最后一行让给进度条，文档只剩首行。
    let mut app = streaming_app("markdown");
    let area = Rect::new(0, 0, 24, 2);
    let mut buf = ratatui::buffer::Buffer::empty(area);
    crate::stories::render_story(&mut app.state, area, &mut buf);
    let bar = crate::stories::streaming::test_bar_area(area);
    assert_eq!(bar.y, area.bottom() - 1, "高度 2 的进度条在最后一行");
    let bar_line: String = (area.x..area.right())
        .map(|x| buf[(x, bar.y)].symbol())
        .collect();
    assert!(
        bar_line.contains('█') || bar_line.contains('░'),
        "高度 2 画进度条: {bar_line:?}"
    );
    let doc_line: String = (area.x..area.right())
        .map(|x| buf[(x, area.y)].symbol())
        .collect();
    assert!(
        !doc_line.trim().is_empty(),
        "高度 2 仍渲染文档首行: {doc_line:?}"
    );
}

#[test]
fn auto_at_full_progress_restarts_from_zero() {
    let mut app = streaming_app("markdown");
    let _ = dump_screen(&mut app, 100, 32);
    let area = app.metrics.story_area;
    let bar = crate::stories::streaming::test_bar_area(area);
    let auto = crate::stories::streaming::test_progress_layout(area)
        .0
        .expect("auto 按钮可见");

    // 先把进度拖到 100%。
    let seek = mouse(
        MouseEventKind::Down(MouseButton::Left),
        bar.right() - 1,
        bar.y,
    );
    assert!(handle_story_mouse(&mut app.state, &seek, area));
    let up = mouse(
        MouseEventKind::Up(MouseButton::Left),
        bar.right() - 1,
        bar.y,
    );
    assert!(handle_story_mouse(&mut app.state, &up, area));
    let (cursor, source) = streaming_cursor(&app);
    assert_eq!(cursor, source.len(), "拖到 100%");

    // 100% 时点击 auto：先归零，再从 0 播到 100%。
    let click = mouse(MouseEventKind::Down(MouseButton::Left), auto.x + 1, auto.y);
    assert!(handle_story_mouse(&mut app.state, &click, area));
    assert_eq!(streaming_cursor(&app).0, 0, "100% 起播先回到 0");
    let play = crate::stories::streaming::autoplay(&app.state).expect("重新播放");
    assert_eq!(play.from, 0);
    crate::stories::streaming::tick_at(&mut app.state, play.start + Duration::from_secs(3));
    assert_eq!(streaming_cursor(&app), (source.len(), source));
    assert!(!crate::stories::streaming::is_animating(&app.state));
}

#[test]
fn dragging_the_track_cancels_playback_and_seeks() {
    let mut app = streaming_app("markdown");
    let _ = dump_screen(&mut app, 100, 32);
    let area = app.metrics.story_area;
    let (auto, bar, _) = crate::stories::streaming::test_progress_layout(area);
    let auto = auto.expect("auto 按钮可见");

    let start = mouse(MouseEventKind::Down(MouseButton::Left), auto.x + 1, auto.y);
    assert!(handle_story_mouse(&mut app.state, &start, area));
    assert!(crate::stories::streaming::is_animating(&app.state));

    // 播放中按住轨道：立即取消播放并按列 seek（拖拽优先）。
    let (_, source) = streaming_cursor(&app);
    let column = bar.x + bar.width / 3;
    let down = mouse(MouseEventKind::Down(MouseButton::Left), column, bar.y);
    assert!(handle_story_mouse(&mut app.state, &down, area));
    assert!(
        !crate::stories::streaming::is_animating(&app.state),
        "按住拖拽条立刻取消播放"
    );
    let expected = source.floor_char_boundary(
        source.len() * usize::from(column - bar.x + 1) / usize::from(bar.width),
    );
    let (seeked, _, _, _) = streaming_progress(&app);
    assert_eq!(seeked, expected, "按下位置立刻生效");

    crate::stories::streaming::tick_at(&mut app.state, Instant::now() + Duration::from_secs(3));
    assert_eq!(streaming_progress(&app).0, seeked, "取消后 tick 不再推进");

    // 松开后也不会自动继续。
    let up = mouse(MouseEventKind::Up(MouseButton::Left), column, bar.y);
    assert!(handle_story_mouse(&mut app.state, &up, area));
    crate::stories::streaming::tick_at(&mut app.state, Instant::now() + Duration::from_secs(3));
    assert_eq!(streaming_progress(&app).0, seeked, "松开后不自动继续");
}

#[test]
fn auto_button_presses_with_selected_style_and_restores_on_up() {
    let mut app = streaming_app("markdown");
    let _ = dump_screen(&mut app, 100, 32);
    let area = app.metrics.story_area;
    let auto = crate::stories::streaming::test_progress_layout(area)
        .0
        .expect("auto 按钮可见");
    assert_eq!(auto.width, 8, "[ auto ] 占 8 列");

    let before = dump_buffer(&mut app, 100, 32);
    let label: String = (auto.x..auto.right())
        .map(|x| before[(x, auto.y)].symbol())
        .collect();
    assert_eq!(label, "[ auto ]", "按钮文本来自 ButtonChrome");
    let normal = before[(auto.x, auto.y)].clone();
    assert_eq!(normal.fg, ACCENT, "未按下用 accent 前景");
    assert_eq!(normal.bg, BG);
    assert!(!normal.modifier.contains(Modifier::BOLD));

    let (cursor, _) = streaming_cursor(&app);
    let auto_pressed = |app: &App| {
        let StoryState::Streaming(document) = &app.state else {
            panic!("expected streaming state");
        };
        document.auto_pressed()
    };
    assert!(!auto_pressed(&app));
    let down = mouse(MouseEventKind::Down(MouseButton::Left), auto.x + 1, auto.y);
    assert!(handle_story_mouse(&mut app.state, &down, area));
    assert!(auto_pressed(&app));
    assert_eq!(
        streaming_cursor(&app).0,
        cursor,
        "点击按钮本身不改变 cursor"
    );
    let started = crate::stories::streaming::autoplay(&app.state)
        .expect("按下 auto 后播放开始")
        .start;
    let held = dump_buffer(&mut app, 100, 32);
    let pressed = &held[(auto.x, auto.y)];
    assert_eq!(pressed.symbol(), "[");
    assert_eq!(pressed.fg, BG, "按下用反色前景");
    assert_eq!(pressed.bg, ACCENT, "按下用反色背景");
    assert!(pressed.modifier.contains(Modifier::BOLD));

    // 拖出按钮即恢复未按下态。
    let drag_out = mouse(
        MouseEventKind::Drag(MouseButton::Left),
        auto.right() + 2,
        auto.y,
    );
    assert!(handle_story_mouse(&mut app.state, &drag_out, area));
    assert!(!auto_pressed(&app), "拖出按钮恢复未按下态");
    assert_eq!(
        dump_buffer(&mut app, 100, 32),
        before,
        "拖出按钮恢复未按下态且不 seek"
    );

    // 未松开前拖回按钮：恢复按下样式，但不重启播放、不 seek。
    let drag_in = mouse(MouseEventKind::Drag(MouseButton::Left), auto.x + 1, auto.y);
    assert!(handle_story_mouse(&mut app.state, &drag_in, area));
    assert!(auto_pressed(&app), "拖回按钮恢复按下样式");
    let play = crate::stories::streaming::autoplay(&app.state).expect("播放仍在进行");
    assert_eq!(play.from, cursor, "拖出再拖回不重启播放");
    assert_eq!(play.start, started, "拖出再拖回不重启播放");
    assert_eq!(streaming_cursor(&app).0, cursor, "拖出再拖回不 seek");
    assert_eq!(
        dump_buffer(&mut app, 100, 32)[(auto.x, auto.y)].bg,
        ACCENT,
        "拖回后仍是按下样式"
    );

    let down = mouse(MouseEventKind::Down(MouseButton::Left), auto.x + 1, auto.y);
    assert!(handle_story_mouse(&mut app.state, &down, area));
    let up = mouse(MouseEventKind::Up(MouseButton::Left), auto.x + 1, auto.y);
    assert!(handle_story_mouse(&mut app.state, &up, area));
    assert!(!auto_pressed(&app), "Up 后恢复未按下态");
    assert_eq!(dump_buffer(&mut app, 100, 32), before, "Up 后恢复未按下态");
    assert_eq!(streaming_cursor(&app).0, cursor, "按钮只负责播放，不 seek");
}

#[test]
fn lost_release_clears_the_auto_press_on_move() {
    let mut app = streaming_app("markdown");
    let _ = dump_screen(&mut app, 100, 32);
    let area = app.metrics.story_area;
    let auto = crate::stories::streaming::test_progress_layout(area)
        .0
        .expect("auto 按钮可见");
    let auto_pressed = |app: &App| {
        let StoryState::Streaming(document) = &app.state else {
            panic!("expected streaming state");
        };
        document.auto_pressed()
    };

    let down = mouse(MouseEventKind::Down(MouseButton::Left), auto.x + 1, auto.y);
    assert!(handle_story_mouse(&mut app.state, &down, area));
    assert!(auto_pressed(&app), "按下 auto 进入按下态");

    // 终端丢失 release：只剩 Moved 也要清掉悬空的按下态。
    let moved = mouse(MouseEventKind::Moved, auto.x + 1, auto.y);
    handle_story_mouse(&mut app.state, &moved, area);
    assert!(!auto_pressed(&app), "Moved 清除悬空的按下态");
    assert!(
        crate::stories::streaming::is_animating(&app.state),
        "只清按下态，不取消正在播放的 auto"
    );
}

#[test]
fn streaming_progress_layout_is_responsive() {
    // 先丢百分比、再丢按钮：18 列三者齐全，13 列起保留按钮，9 列起只剩标签。
    // 预览区最右一列留给文档滚动条，所以行宽 = 预览区宽 - 1。
    let geometry = |row_width: u16| {
        crate::stories::streaming::test_progress_layout(Rect::new(0, 0, row_width + 1, 2))
    };
    let (auto, bar, label) = geometry(20);
    assert!(auto.is_some(), "20 列三者齐全");
    assert_eq!((bar.x, bar.width), (9, 6), "按钮 8 列 + 1 列间隔");
    assert!(label);

    let (auto, bar, label) = geometry(18);
    assert!(auto.is_some(), "18 列仍三者齐全");
    assert_eq!((bar.x, bar.width), (9, 4));
    assert!(label);

    for width in [17, 13] {
        let (auto, bar, label) = geometry(width);
        assert!(auto.is_some(), "{width} 列先丢百分比、保留按钮");
        assert_eq!((bar.x, bar.width), (9, width - 9));
        assert!(!label, "{width} 列不再显示百分比");
    }
    for width in [12, 9] {
        let (auto, bar, label) = geometry(width);
        assert!(auto.is_none(), "{width} 列放不下按钮");
        assert_eq!(bar.width, width - 5);
        assert!(label, "{width} 列仍显示百分比");
    }
    let (auto, bar, label) = geometry(8);
    assert!(auto.is_none());
    assert_eq!(bar.width, 8);
    assert!(!label, "窄轨道不画百分比");

    // 预览区被压到 40 列总宽：行宽 17，按钮必须保留，否则无法启动播放。
    let mut app = streaming_app("markdown");
    let screen = dump_screen(&mut app, 40, 10);
    assert!(screen.contains("peri-tui storybook"));
    let area = app.metrics.story_area;
    let (auto, bar, label) = crate::stories::streaming::test_progress_layout(area);
    assert!(auto.is_some(), "40x10 的预览行仍保留 [ auto ]");
    assert!(!label, "40x10 先丢百分比标签");
    assert_eq!(bar.width, 8);
    let line = progress_line(&app, &screen);
    assert!(line.contains("[ auto ]"), "按钮可见: {line:?}");
    assert!(
        line.contains('█') || line.contains('░'),
        "进度条行存在: {line:?}"
    );
    assert!(!line.contains('%'), "窄行不再显示百分比: {line:?}");
}

#[test]
fn clicking_the_percentage_label_does_not_seek() {
    let mut app = streaming_app("markdown");
    let _ = dump_screen(&mut app, 100, 32);
    let area = app.metrics.story_area;
    let (_, bar, label) = crate::stories::streaming::test_progress_layout(area);
    assert!(label, "100 列宽应显示百分比标签");
    let before = streaming_cursor(&app).0;
    // 标签紧贴轨道右缘，第一格就是 bar.right()。
    let down = mouse(MouseEventKind::Down(MouseButton::Left), bar.right(), bar.y);
    assert!(handle_story_mouse(&mut app.state, &down, area));
    assert_eq!(
        streaming_cursor(&app).0,
        before,
        "百分比标签不是 seek 区，误点不应跳到 100%"
    );
}

/// 点击映射必须使用渲染器同一套视口窗口：文本超出高度时，屏幕首行不是源码首行。
#[test]
fn textarea_click_maps_through_scrolled_viewport() {
    let mut editor = peri_tui_components::textarea::TextAreaState::default();
    editor.text = "0\n1\n2\n3\n4\n5".to_string();
    editor.cursor = editor.text.chars().count();
    let area = Rect::new(0, 0, 20, 2);
    place_caret(
        &mut editor,
        &mouse(MouseEventKind::Down(MouseButton::Left), 0, 0),
        area,
    );
    // 光标在第 6 个视觉行，高度 2 的窗口为 [4, 6)：屏幕首行是源码行 "4"（字符索引 8）。
    assert_eq!(editor.cursor, 8);
}

/// 35 个图表变体放不进 100x32 侧栏首屏：滚轮到底后必须能点到最后一个。
#[test]
fn nav_wheel_reaches_the_last_chart_variant() {
    let charts = STORIES
        .iter()
        .position(|story| story.key == "charts")
        .expect("charts story");
    let last = STORIES[charts].variants.len() - 1;
    let mut app = App::new(charts);
    let _ = dump_screen(&mut app, 100, 32);
    assert!(app.metrics.nav_max_scroll > 0, "侧栏内容应超出视口");
    let nav = app.metrics.nav_area;
    for _ in 0..12 {
        handle_mouse(
            &mut app,
            mouse(MouseEventKind::ScrollDown, nav.x + 1, nav.y + 1),
        );
    }
    let _ = dump_screen(&mut app, 100, 32);
    let rows = app.metrics.variant_rows;
    assert_eq!(
        app.metrics.variant_first + rows.height as usize - 1,
        last,
        "滚到底后最后一行应是最后一个变体"
    );
    handle_mouse(
        &mut app,
        mouse(
            MouseEventKind::Down(MouseButton::Left),
            rows.x + 1,
            rows.y + rows.height - 1,
        ),
    );
    assert_eq!(app.variant, last);
}

/// 最小尺寸 40x10 下侧栏只放得下 story 行；滚轮后必须出现 variant 行并可点选。
#[test]
fn minimum_size_reaches_variants_with_the_wheel() {
    let mut app = App::new(0);
    let _ = dump_screen(&mut app, 40, 10);
    assert_eq!(app.metrics.variant_rows.height, 0);
    let nav = app.metrics.nav_area;
    for _ in 0..3 {
        handle_mouse(
            &mut app,
            mouse(MouseEventKind::ScrollDown, nav.x + 1, nav.y + 1),
        );
    }
    let _ = dump_screen(&mut app, 40, 10);
    let rows = app.metrics.variant_rows;
    assert!(rows.height > 1, "滚轮后至少应看到两行 variant");
    // 点第二行：variant 0 本来就是选中项，点它无法证明点击生效。
    handle_mouse(
        &mut app,
        mouse(
            MouseEventKind::Down(MouseButton::Left),
            rows.x + 1,
            rows.y + 1,
        ),
    );
    assert_eq!(app.variant, 1);
}

/// `--variant` 直接选中靠后的变体时，首帧必须把它滚进视口。
#[test]
fn selecting_a_variant_scrolls_it_into_view() {
    let charts = STORIES
        .iter()
        .position(|story| story.key == "charts")
        .expect("charts story");
    let mut app = App::new(charts);
    app.select_variant(30);
    let _ = dump_screen(&mut app, 100, 32);
    let rows = app.metrics.variant_rows;
    assert!(30 >= app.metrics.variant_first);
    assert!(30 < app.metrics.variant_first + rows.height as usize);
}

/// 侧栏滚轮被消费前必须先转发给文档清除按下态，否则按钮会停在按下样式。
#[test]
fn sidebar_wheel_clears_a_pressed_document_button() {
    let mut app = App::new(story_index("openui"));
    let variant = app
        .current()
        .variants
        .iter()
        .position(|variant| variant.key == "button")
        .expect("button variant");
    app.select_variant(variant);
    let before = dump_buffer(&mut app, 100, 32);
    let area = app.metrics.story_area;
    let (x, y) = (area.top()..area.bottom())
        .flat_map(|y| (area.left()..area.right()).map(move |x| (x, y)))
        .find(|&(x, y)| before[(x, y)].symbol() == "继")
        .expect("主按钮必须出现在预览中");
    let pressed_index = |app: &App| match &app.state {
        StoryState::Document { pressed_button, .. } => *pressed_button,
        _ => panic!("应为文档预览"),
    };

    handle_mouse(
        &mut app,
        mouse(MouseEventKind::Down(MouseButton::Left), x, y),
    );
    assert_eq!(pressed_index(&app), Some(0));

    let nav = app.metrics.nav_area;
    handle_mouse(
        &mut app,
        mouse(MouseEventKind::ScrollDown, nav.x + 1, nav.y + 1),
    );
    assert_eq!(pressed_index(&app), None, "侧栏滚轮应清除按下态");
}

/// 终端 resize 后按钮热区已重建：清掉按下态，但选中项与滚动位置不变。
#[test]
fn resize_clears_pressed_states_without_resetting_navigation() {
    let mut app = App::new(story_index("openui"));
    let variant = app
        .current()
        .variants
        .iter()
        .position(|variant| variant.key == "button")
        .expect("button variant");
    app.select_variant(variant);
    let before = dump_buffer(&mut app, 100, 32);
    let area = app.metrics.story_area;
    let (x, y) = (area.top()..area.bottom())
        .flat_map(|y| (area.left()..area.right()).map(move |x| (x, y)))
        .find(|&(x, y)| before[(x, y)].symbol() == "继")
        .expect("主按钮必须出现在预览中");
    handle_mouse(
        &mut app,
        mouse(MouseEventKind::Down(MouseButton::Left), x, y),
    );
    let StoryState::Document {
        scroll: document_scroll,
        pressed_button,
        ..
    } = &app.state
    else {
        panic!("应为文档预览");
    };
    assert_eq!(*pressed_button, Some(0));
    let document_scroll = *document_scroll;
    let selected_variant = app.variant;

    super::handle_resize(&mut app.state);
    let StoryState::Document {
        scroll,
        pressed_button,
        ..
    } = &app.state
    else {
        panic!("应为文档预览");
    };
    assert_eq!(*pressed_button, None, "resize 清除文档按钮按下态");
    assert_eq!(*scroll, document_scroll, "resize 不重置滚动位置");
    assert_eq!(app.variant, selected_variant, "resize 不重置选中 variant");

    // 流式 auto 按钮：resize 只清按下态，播放与 cursor 保留。
    let mut app = streaming_app("markdown");
    let _ = dump_screen(&mut app, 100, 32);
    let area = app.metrics.story_area;
    let auto = crate::stories::streaming::test_progress_layout(area)
        .0
        .expect("auto 按钮可见");
    assert!(handle_story_mouse(
        &mut app.state,
        &mouse(MouseEventKind::Down(MouseButton::Left), auto.x + 1, auto.y),
        area
    ));
    let auto_pressed = |app: &App| {
        let StoryState::Streaming(document) = &app.state else {
            panic!("expected streaming state");
        };
        document.auto_pressed()
    };
    assert!(auto_pressed(&app));
    let (cursor, _, scroll, _) = streaming_progress(&app);

    super::handle_resize(&mut app.state);
    assert!(!auto_pressed(&app), "resize 清除 auto 按下态");
    assert_eq!(streaming_progress(&app).0, cursor, "resize 不重置 cursor");
    assert_eq!(streaming_progress(&app).2, scroll, "resize 不重置滚动位置");
    assert!(
        crate::stories::streaming::is_animating(&app.state),
        "resize 不取消播放"
    );
}

fn completion_app(variant: &str) -> App {
    let story = STORIES
        .iter()
        .position(|story| story.key == "completion")
        .expect("completion story");
    let mut app = App::new(story);
    let variant = app
        .current()
        .variants
        .iter()
        .position(|entry| entry.key == variant)
        .expect("completion variant");
    app.select_variant(variant);
    app
}

#[test]
fn completion_click_maps_the_scrolled_window_to_full_index() {
    let mut app = completion_app("scrolled");
    let area = Rect::new(0, 10, 60, 12);
    // 12 行、8 行窗口 → max_scroll = 4，第一内容行对应下标 4。
    assert!(handle_story_mouse(
        &mut app.state,
        &mouse(MouseEventKind::Down(MouseButton::Left), 5, 11),
        area
    ));
    let StoryState::Completion { selected, .. } = &app.state else {
        panic!("expected completion state");
    };
    assert_eq!(*selected, 4, "第一内容行对应窗口起点 max_scroll");

    // 弹窗下边框之外不改变选择
    assert!(!handle_story_mouse(
        &mut app.state,
        &mouse(MouseEventKind::Down(MouseButton::Left), 5, 20),
        area
    ));
    let StoryState::Completion { selected, .. } = &app.state else {
        panic!("expected completion state");
    };
    assert_eq!(*selected, 4);
}

#[test]
fn completion_empty_popup_has_no_clickable_row() {
    let mut app = completion_app("empty");
    let area = Rect::new(0, 10, 60, 12);
    // 弹窗区域内点击被消费但不选中（空列表）
    assert!(handle_story_mouse(
        &mut app.state,
        &mouse(MouseEventKind::Down(MouseButton::Left), 5, 11),
        area
    ));
    let StoryState::Completion { selected, .. } = &app.state else {
        panic!("expected completion state");
    };
    assert_eq!(*selected, 0);
    assert!(dump_screen(&mut app, 100, 32).contains("No matches"));
}

#[test]
fn completion_scrolled_dump_keeps_only_the_window() {
    let mut app = completion_app("scrolled");
    let screen = dump_screen(&mut app, 100, 32);
    assert!(screen.contains("> /model:prod"), "选中行留在窗口内");
    assert!(screen.contains("/model:staging"));
    assert!(!screen.contains("/model:default"), "窗口外的候选不绘制");
    assert!(screen.contains("inline height 10"));
}

fn story_app(story: &str, variant: &str) -> App {
    let index = STORIES
        .iter()
        .position(|entry| entry.key == story)
        .unwrap_or_else(|| panic!("{story} story"));
    let mut app = App::new(index);
    let variant = app
        .current()
        .variants
        .iter()
        .position(|entry| entry.key == variant)
        .unwrap_or_else(|| panic!("{story}/{variant} variant"));
    app.select_variant(variant);
    app
}

fn overlay_state(app: &App) -> &crate::stories::overlay::OverlayState {
    match &app.state {
        StoryState::Overlay(state) => state,
        _ => panic!("expected overlay state"),
    }
}

/// 弹窗矩形来自故事自己的布局（与绘制同源）。
fn overlay_popup(app: &App, area: Rect) -> Rect {
    crate::stories::overlay::popup_bounds(overlay_state(app), area)
}

#[test]
fn overlay_confirm_click_inside_is_an_action_and_outside_is_left_to_the_host() {
    let mut app = story_app("overlay", "confirm");
    let area = Rect::new(0, 10, 60, 20);
    let popup = overlay_popup(&app, area);
    assert!(popup.width > 0 && popup.height > 0);

    // 外点：不消费、不确认（宿主继续路由）。
    assert!(!handle_story_mouse(
        &mut app.state,
        &mouse(
            MouseEventKind::Down(MouseButton::Left),
            popup.x - 2,
            popup.y + 2
        ),
        area
    ));
    assert_eq!(overlay_state(&app).confirmed, 0);
    assert_eq!(overlay_state(&app).outside, 1);

    // 内区点击 = 确认。
    assert!(handle_story_mouse(
        &mut app.state,
        &mouse(
            MouseEventKind::Down(MouseButton::Left),
            popup.x + 2,
            popup.y + 2
        ),
        area
    ));
    assert_eq!(overlay_state(&app).confirmed, 1);

    // 上边框同样属于弹窗（整窗点击）。
    assert!(handle_story_mouse(
        &mut app.state,
        &mouse(
            MouseEventKind::Down(MouseButton::Left),
            popup.x + 2,
            popup.y
        ),
        area
    ));
    assert_eq!(overlay_state(&app).confirmed, 2);

    let screen = dump_screen(&mut app, 100, 32);
    assert!(screen.contains("已确认 2 次"));
    assert!(screen.contains("outside clicks 1"));
}

#[test]
fn overlay_back_button_reports_back_instead_of_confirming() {
    let mut app = story_app("overlay", "confirm");
    let area = Rect::new(0, 10, 60, 20);
    let back = crate::stories::overlay::back_button_bounds(overlay_state(&app), area)
        .expect("confirm story renders a back button");
    assert!(back.width > 0 && back.height == 1);

    assert!(handle_story_mouse(
        &mut app.state,
        &mouse(MouseEventKind::Down(MouseButton::Left), back.x, back.y),
        area
    ));
    assert_eq!(overlay_state(&app).back, 1);
    assert_eq!(overlay_state(&app).confirmed, 0, "返回按钮不算确认");

    let screen = dump_screen(&mut app, 100, 32);
    assert!(screen.contains("确认"), "动作行使用通用 Filled 按钮");
    assert!(screen.contains("返回"));
    assert!(screen.contains("back 1"));
}

#[test]
fn overlay_download_click_consumes_without_confirming() {
    let mut app = story_app("overlay", "download");
    let area = Rect::new(0, 10, 60, 20);
    let popup = overlay_popup(&app, area);

    assert!(handle_story_mouse(
        &mut app.state,
        &mouse(
            MouseEventKind::Down(MouseButton::Left),
            popup.x + 2,
            popup.y + 2
        ),
        area
    ));
    assert_eq!(overlay_state(&app).confirmed, 0, "下载弹窗没有确认动作");
    let screen = dump_screen(&mut app, 100, 32);
    assert!(screen.contains("下载主题 3/5"));
    assert!(screen.contains("→ statusline.toml"));
}

#[test]
fn overlay_wheel_inside_is_blocked_and_outside_scrolls_the_transcript() {
    let mut app = story_app("overlay", "confirm");
    let area = Rect::new(0, 10, 60, 20);
    let popup = overlay_popup(&app, area);

    assert!(handle_story_mouse(
        &mut app.state,
        &mouse(MouseEventKind::ScrollDown, popup.x + 2, popup.y + 2),
        area
    ));
    assert_eq!(overlay_state(&app).scroll, 0, "弹窗内滚轮不滚消息区");

    assert!(handle_story_mouse(
        &mut app.state,
        &mouse(MouseEventKind::ScrollDown, popup.x - 2, popup.y + 2),
        area
    ));
    assert_eq!(
        overlay_state(&app).scroll,
        crate::stories::WHEEL_ROWS,
        "弹窗外滚轮交给宿主消息区"
    );
}

fn spinner_state(app: &App) -> &crate::stories::spinner::SpinnerState {
    match &app.state {
        StoryState::Spinner(state) => state,
        _ => panic!("expected spinner state"),
    }
}

#[test]
fn spinner_idle_frame_never_advances() {
    let mut app = story_app("spinner", "idle");
    let area = Rect::new(0, 10, 40, 10);
    assert!(handle_story_mouse(
        &mut app.state,
        &mouse(MouseEventKind::Down(MouseButton::Left), 2, 10),
        area
    ));
    assert_eq!(spinner_state(&app).frame, 0, "空闲底部固定第一帧");
    assert!(dump_screen(&mut app, 100, 32).contains("✳ 格物致知"));
}

#[test]
fn spinner_active_frame_is_host_owned_and_dump_is_reproducible() {
    let mut app = story_app("spinner", "active");
    let area = Rect::new(0, 10, 40, 10);
    let screen = dump_screen(&mut app, 100, 32);
    assert!(screen.contains("✶ 思考中… (12s · ↓ 1.5k tokens)"));
    assert!(screen.contains("host frame 3 / 16"));

    assert!(handle_story_mouse(
        &mut app.state,
        &mouse(MouseEventKind::Down(MouseButton::Left), 2, 10),
        area
    ));
    assert_eq!(spinner_state(&app).frame, 4);
}

#[test]
fn spinner_tool_glyph_matches_the_host_frame_set() {
    let mut app = story_app("spinner", "tool");
    let screen = dump_screen(&mut app, 100, 32);
    assert!(screen.contains("⠼ Bash  cargo test"));
    assert!(screen.contains("tool glyph ⠼ · host frame 4 / 10"));
}

#[test]
fn spinner_active_advances_with_the_host_clock() {
    let mut app = story_app("spinner", "active");
    assert!(crate::stories::is_animating(&app.state));
    let base = spinner_state(&app).last_tick();

    // 250ms = 两帧，余下 50ms 留到下一次。
    crate::stories::tick_at(&mut app.state, base + Duration::from_millis(250));
    assert_eq!(spinner_state(&app).frame, 5);
    let screen = dump_screen(&mut app, 100, 32);
    assert!(screen.contains("✸ 思考中…"));
    assert!(screen.contains("host frame 5 / 16"));

    // 距上一帧边界 110ms：再推进一帧，并保留 10ms 余数。
    crate::stories::tick_at(&mut app.state, base + Duration::from_millis(310));
    assert_eq!(spinner_state(&app).frame, 6);
    let screen = dump_screen(&mut app, 100, 32);
    assert!(screen.contains("✹ 思考中…"));
    assert!(screen.contains("host frame 6 / 16"));

    // 距上一帧边界 60ms：不足一帧。
    crate::stories::tick_at(&mut app.state, base + Duration::from_millis(360));
    assert_eq!(spinner_state(&app).frame, 6);
}

#[test]
fn spinner_idle_is_not_animated_by_the_host_clock() {
    let mut app = story_app("spinner", "idle");
    assert!(!crate::stories::is_animating(&app.state));
    let base = spinner_state(&app).last_tick();
    crate::stories::tick_at(&mut app.state, base + Duration::from_secs(5));
    assert_eq!(spinner_state(&app).frame, 0, "空闲底部固定第一帧");
}

#[test]
fn spinner_tool_rotates_with_the_host_clock() {
    let mut app = story_app("spinner", "tool");
    assert!(crate::stories::is_animating(&app.state));
    let base = spinner_state(&app).last_tick();
    crate::stories::tick_at(&mut app.state, base + Duration::from_millis(200));
    assert_eq!(spinner_state(&app).frame, 6);
}
