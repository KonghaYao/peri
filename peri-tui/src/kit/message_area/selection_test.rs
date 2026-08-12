//! Tests

use super::*;
use ratatui_kit::ratatui::text::Span;

fn make_line(s: &str) -> Line<'static> {
    Line::from(vec![Span::raw(s.to_string())])
}

// ── concat_wrap_maps ──

fn make_wrap_entry(logical: usize, start: usize, end: usize, slot: usize) -> WrappedLineInfo {
    WrappedLineInfo {
        logical_idx: logical,
        visual_start: start,
        visual_end: end,
        slot_index: slot,
    }
}

#[test]
fn test_concat_wrap_maps_empty_input_returns_empty() {
    let result = concat_wrap_maps(&[]);
    assert!(result.is_empty());
}

#[test]
fn test_concat_wrap_maps_single_slot_preserves_entries() {
    // 单个分片：偏移为 0，logical_idx 不变
    let slot = vec![
        make_wrap_entry(0, 0, 1, 0),
        make_wrap_entry(1, 1, 3, 0),
        make_wrap_entry(2, 3, 4, 0),
    ];
    let result = concat_wrap_maps(&[(&slot, 0, 0)]);
    assert_eq!(
        result,
        vec![
            make_wrap_entry(0, 0, 1, 0),
            make_wrap_entry(1, 1, 3, 0),
            make_wrap_entry(2, 3, 4, 0),
        ]
    );
}

#[test]
fn test_concat_wrap_maps_multi_slots_accumulates_offsets() {
    // 3 个分片，各自内部 visual_row 从 0 起；拼接后累加 visual_offset 和 logical_idx
    // slot0: 2 行（visual 0-2），lines_start=0，slot_index=0
    // slot1: 1 行（visual 0-1），lines_start=2（slot0 占用 2 个 logical line），slot_index=1
    // slot2: 1 行（visual 0-1），lines_start=3，slot_index=2
    let slot0 = vec![make_wrap_entry(0, 0, 1, 0), make_wrap_entry(1, 1, 2, 0)];
    let slot1 = vec![make_wrap_entry(0, 0, 1, 0)];
    let slot2 = vec![make_wrap_entry(0, 0, 1, 0)];
    let result = concat_wrap_maps(&[(&slot0, 0, 0), (&slot1, 2, 1), (&slot2, 3, 2)]);
    assert_eq!(
        result,
        vec![
            // slot0 原样
            make_wrap_entry(0, 0, 1, 0),
            make_wrap_entry(1, 1, 2, 0),
            // slot1: visual += 2, logical += 2
            make_wrap_entry(2, 2, 3, 1),
            // slot2: visual += 3, logical += 3
            make_wrap_entry(3, 3, 4, 2),
        ]
    );
}

#[test]
fn test_concat_wrap_maps_supports_multi_visual_rows_per_line() {
    // 单条逻辑行 wrap 成多视觉行：分片 1 一条 line 占 visual 0-3
    let slot0 = vec![make_wrap_entry(0, 0, 3, 0)];
    let slot1 = vec![make_wrap_entry(0, 0, 1, 0), make_wrap_entry(1, 1, 2, 0)];
    let result = concat_wrap_maps(&[(&slot0, 0, 0), (&slot1, 1, 1)]);
    assert_eq!(
        result,
        vec![
            make_wrap_entry(0, 0, 3, 0),
            // slot1 第一行 visual_start += 3, logical_idx += 1
            make_wrap_entry(1, 3, 4, 1),
            // slot1 第二行 visual_start/end += 3, logical_idx += 1
            make_wrap_entry(2, 4, 5, 1),
        ]
    );
}

// ── wrap_byte_starts ──

#[test]
fn test_wrap_byte_starts_ascii_no_wrap() {
    let line = make_line("abcdef");
    assert_eq!(wrap_byte_starts(&line, "abcdef", 10), vec![0, 6]);
}

#[test]
fn test_wrap_byte_starts_ascii_wrap() {
    let line = make_line("abcdef");
    // width=3: 行 0="abc", 行 1="def"
    assert_eq!(wrap_byte_starts(&line, "abcdef", 3), vec![0, 3, 6]);
}

#[test]
fn test_wrap_byte_starts_cjk_no_wrap() {
    let line = make_line("你好");
    assert_eq!(wrap_byte_starts(&line, "你好", 10), vec![0, 6]);
}

#[test]
fn test_wrap_byte_starts_mixed_cjk_word_wrap() {
    // ratatui WordWrapper 行为：CJK 整段算一个 word，超宽不拆分
    // plain="abc你好你好" w=5 → 行 0="abc你"(byte 0..6), 行 1="好你好"(byte 6..15, 溢出但保留)
    let line = make_line("abc你好你好");
    assert_eq!(wrap_byte_starts(&line, "abc你好你好", 5), vec![0, 6, 15]);
}

#[test]
fn test_wrap_byte_starts_mixed_cjk_wider() {
    // w=4: 行 0="abc", 行 1="你好"(trim:false 下 WordWrapper 在 word 边界处拆，但 CJK 无空格
    // 整段 word "abc你好你好" 超过 4 列。观察：abc 你好 / 你好
    let line = make_line("abc你好你好");
    let starts = wrap_byte_starts(&line, "abc你好你好", 4);
    assert_eq!(starts, vec![0, 3, 9, 15]);
}

#[test]
fn test_wrap_byte_starts_empty() {
    let line = make_line("");
    assert_eq!(wrap_byte_starts(&line, "", 10), vec![0]);
}

#[test]
fn test_wrap_byte_starts_single_cjk() {
    let line = make_line("你");
    assert_eq!(wrap_byte_starts(&line, "你", 10), vec![0, 3]);
}

#[test]
fn test_wrap_byte_starts_row_count_matches_ratatui() {
    // 关键不变量：wrap_byte_starts 推算的视觉行数必须与 ratatui Paragraph::line_count
    // 一致——否则鼠标点击的视觉行号与提取算法的视觉行号错位
    for (text, width) in [
        ("abc你好你好", 5u16),
        ("abc你好你好", 4),
        ("abc你好", 4),
        ("你好世界", 3),
        ("你好世界", 5),
        ("abcdef", 3),
        ("hello world", 5),
    ] {
        let line = make_line(text);
        let line_count = Paragraph::new(ratatui_kit::ratatui::text::Text::from(line.clone()))
            .wrap(Wrap { trim: false })
            .line_count(width);
        let our_rows = wrap_byte_starts(&line, text, width)
            .len()
            .saturating_sub(1)
            .max(1);
        assert_eq!(
            our_rows, line_count,
            "wrap_byte_starts 行数 {} 与 ratatui line_count {} 不一致（text={:?}, width={}）",
            our_rows, line_count, text, width
        );
    }
}

// ── extract_visual_range ──

#[test]
fn test_extract_cjk_same_visual_row_double_width_right_half() {
    // ratatui w=5: 行 0="abc你", 行 1="好你好"（WordWrapper 不拆 word，溢出保留）
    // 用户拖 col 0 到 col 3（'你' 右半，含 '你'）→ 期望 "好你"
    let lines = vec![make_line("abc你好你好")];
    let (_, wrap_map) = build_wrap_map(&lines, 5);
    let slots = vec![Arc::new(lines)];
    let offsets = vec![0usize];
    let result = extract_visual_range(&slots, &offsets, &wrap_map, (1, 0), (1, 3), 5, None, None);
    assert_eq!(result.as_deref(), Some("好你"));
}

#[test]
fn test_extract_cjk_same_visual_row_left_half_excludes_char() {
    // 同 row 1，拖 col 0 到 col 2（'你' 左半，不含 '你'）→ "好"
    let lines = vec![make_line("abc你好你好")];
    let (_, wrap_map) = build_wrap_map(&lines, 5);
    let slots = vec![Arc::new(lines)];
    let offsets = vec![0usize];
    let result = extract_visual_range(&slots, &offsets, &wrap_map, (1, 0), (1, 2), 5, None, None);
    assert_eq!(result.as_deref(), Some("好"));
}

#[test]
fn test_extract_cjk_first_row_partial() {
    // 行 0="abc你"：col 0 到 col 4（'你' 右半，含）→ "abc你"
    let lines = vec![make_line("abc你好你好")];
    let (_, wrap_map) = build_wrap_map(&lines, 5);
    let slots = vec![Arc::new(lines)];
    let offsets = vec![0usize];
    let result = extract_visual_range(&slots, &offsets, &wrap_map, (0, 0), (0, 4), 5, None, None);
    assert_eq!(result.as_deref(), Some("abc你"));
}

#[test]
fn test_extract_cjk_cross_visual_row() {
    // 跨视觉行：行 0 col 4（'你' 右半，含）→ 行 1 col 1（'好' 右半，含）
    // 行 0 含 '你'，行 1 含 '好'（行 1 第 1 个字符）
    // 期望 '你' + '好' = "你好"
    let lines = vec![make_line("abc你好你好")];
    let (_, wrap_map) = build_wrap_map(&lines, 5);
    let slots = vec![Arc::new(lines)];
    let offsets = vec![0usize];
    let result = extract_visual_range(&slots, &offsets, &wrap_map, (0, 4), (1, 1), 5, None, None);
    assert_eq!(result.as_deref(), Some("你好"));
}

#[test]
fn test_extract_ascii_same_row() {
    let lines = vec![make_line("abcdef")];
    let (_, wrap_map) = build_wrap_map(&lines, 10);
    let slots = vec![Arc::new(lines)];
    let offsets = vec![0usize];
    let result = extract_visual_range(&slots, &offsets, &wrap_map, (0, 1), (0, 3), 10, None, None);
    assert_eq!(result.as_deref(), Some("bc"));
}

#[test]
fn test_extract_ascii_cross_visual_row() {
    // 视觉行 0="abc" col 2，视觉行 1="def" col 1 → "cd"
    let lines = vec![make_line("abcdef")];
    let (_, wrap_map) = build_wrap_map(&lines, 3);
    let slots = vec![Arc::new(lines)];
    let offsets = vec![0usize];
    let result = extract_visual_range(&slots, &offsets, &wrap_map, (0, 2), (1, 1), 3, None, None);
    assert_eq!(result.as_deref(), Some("cd"));
}

#[test]
fn test_extract_cross_logical_row() {
    // 跨逻辑行：(0,1) → (1,2) = "bc" + "de"（用 \n 连接）
    let lines = vec![make_line("abc"), make_line("def")];
    let (_, wrap_map) = build_wrap_map(&lines, 10);
    let slots = vec![Arc::new(lines)];
    let offsets = vec![0usize];
    let result = extract_visual_range(&slots, &offsets, &wrap_map, (0, 1), (1, 2), 10, None, None);
    assert_eq!(result.as_deref(), Some("bc\nde"));
}

#[test]
fn test_extract_swapped_start_end_normalizes() {
    // 反向拖拽：vis_start > vis_end 应规范化
    let lines = vec![make_line("abcdef")];
    let (_, wrap_map) = build_wrap_map(&lines, 10);
    let slots = vec![Arc::new(lines)];
    let offsets = vec![0usize];
    let result = extract_visual_range(&slots, &offsets, &wrap_map, (0, 3), (0, 1), 10, None, None);
    assert_eq!(result.as_deref(), Some("bc"));
}

// ── highlight_line_in_selection（字符级高亮）──

use ratatui_kit::ratatui::style::{Color, Style};

const TEST_BG: Color = Color::Rgb(1, 2, 3);

/// 提取 line 中带 `bg` 背景的 span 内容拼接，用于断言"实际被高亮的字符"。
fn highlighted_text(line: &Line<'_>, bg: Color) -> String {
    line.spans
        .iter()
        .filter(|s| s.style.bg == Some(bg))
        .map(|s| s.content.as_ref())
        .collect::<Vec<&str>>()
        .concat()
}

#[test]
fn test_highlight_ascii_same_row_partial() {
    // line="abcdef" w=10：单视觉行，sel=(0,1)→(0,3) → byte 范围 [1,3)，高亮 "bc"
    let line = make_line("abcdef");
    let entry = WrappedLineInfo {
        slot_index: 0,
        logical_idx: 0,
        visual_start: 0,
        visual_end: 1,
    };
    let highlighted = highlight_line_in_selection(&line, &entry, 0, 0, 1, 3, 10, TEST_BG);
    assert_eq!(highlighted_text(&highlighted, TEST_BG), "bc");
}

#[test]
fn test_highlight_cjk_same_visual_row_double_width_right_half() {
    // line="abc你好你好" w=5：行 0="abc你", 行 1="好你好"
    // sel=(1,0)→(1,3) → '好' 占 col 0-1（右半 col 1 含），'你' 占 col 2-3（右半 col 3 含）
    // 期望高亮 "好你"（byte 6..12）
    let line = make_line("abc你好你好");
    let entry = WrappedLineInfo {
        slot_index: 0,
        logical_idx: 0,
        visual_start: 0,
        visual_end: 2,
    };
    let highlighted = highlight_line_in_selection(&line, &entry, 1, 1, 0, 3, 5, TEST_BG);
    assert_eq!(highlighted_text(&highlighted, TEST_BG), "好你");
}

#[test]
fn test_highlight_cjk_cross_visual_row_first_and_last_same_logical() {
    // 同一逻辑行的跨视觉行选区：行 0 col 4（'你' 右半含）→ 行 1 col 1（'好' 右半含）
    // sr_in_line=true (sr=0)，er_in_line=true (er=1)
    // byte 范围 [3, 9)：高亮 "你好"
    let line = make_line("abc你好你好");
    let entry = WrappedLineInfo {
        slot_index: 0,
        logical_idx: 0,
        visual_start: 0,
        visual_end: 2,
    };
    let highlighted = highlight_line_in_selection(&line, &entry, 0, 1, 4, 1, 5, TEST_BG);
    assert_eq!(highlighted_text(&highlighted, TEST_BG), "你好");
}

#[test]
fn test_highlight_first_logical_only_partial_to_end() {
    // lines=["abc","def"]：首行 "abc" sel=(0,1)→(1,2)
    // 首行 is_first=true, is_last=false → byte 范围 [1,3)，高亮 "bc"
    let line = make_line("abc");
    let entry = WrappedLineInfo {
        slot_index: 0,
        logical_idx: 0,
        visual_start: 0,
        visual_end: 1,
    };
    let highlighted = highlight_line_in_selection(&line, &entry, 0, 1, 1, 2, 10, TEST_BG);
    assert_eq!(highlighted_text(&highlighted, TEST_BG), "bc");
}

#[test]
fn test_highlight_last_logical_only_start_to_col() {
    // lines=["abc","def"]：末行 "def" sel=(0,1)→(1,2)
    // 末行 is_first=false, is_last=true → byte 范围 [0,2)，高亮 "de"
    let line = make_line("def");
    // 末行的 visual_start=1（第二逻辑行），sel_sr=0（不在该行），sel_er=1（在该行）
    let entry = WrappedLineInfo {
        slot_index: 0,
        logical_idx: 1,
        visual_start: 1,
        visual_end: 2,
    };
    let highlighted = highlight_line_in_selection(&line, &entry, 0, 1, 1, 2, 10, TEST_BG);
    assert_eq!(highlighted_text(&highlighted, TEST_BG), "de");
}

#[test]
fn test_highlight_middle_logical_full_line() {
    // lines=["abc","def","ghi"]：中间行 "def" sel=(0,0)→(2,0)
    // 中间行 is_first=false, is_last=false → 整行 [0,3)，高亮 "def"
    let line = make_line("def");
    let entry = WrappedLineInfo {
        slot_index: 0,
        logical_idx: 1,
        visual_start: 1,
        visual_end: 2,
    };
    let highlighted = highlight_line_in_selection(&line, &entry, 0, 2, 0, 0, 10, TEST_BG);
    assert_eq!(highlighted_text(&highlighted, TEST_BG), "def");
}

#[test]
fn test_highlight_multi_span_partial_overlap_splits_span() {
    // line = [Span("hello"), Span("world")]（两个独立 span，每个 byte=5）
    // sel 覆盖 byte [3,7) → "lo" + "wo"，应拆分两个 span 各成 前缀未高亮 + 中段高亮 + 后缀
    let line = Line::from(vec![
        Span::raw("hello".to_string()),
        Span::raw("world".to_string()),
    ]);
    let entry = WrappedLineInfo {
        slot_index: 0,
        logical_idx: 0,
        visual_start: 0,
        visual_end: 1,
    };
    let highlighted = highlight_line_in_selection(&line, &entry, 0, 0, 3, 7, 20, TEST_BG);
    assert_eq!(highlighted_text(&highlighted, TEST_BG), "lowo");
    // 完整文本保留原顺序
    let full: String = highlighted
        .spans
        .iter()
        .map(|s| s.content.as_ref())
        .collect::<Vec<&str>>()
        .concat();
    assert_eq!(full, "helloworld");
}

#[test]
fn test_highlight_empty_byte_range_returns_original() {
    // line="abcdef" w=10，sel=(0,1)→(0,1)（同点）→ byte 范围空 → 返回 clone 原行
    let line = make_line("abcdef");
    let entry = WrappedLineInfo {
        slot_index: 0,
        logical_idx: 0,
        visual_start: 0,
        visual_end: 1,
    };
    let highlighted = highlight_line_in_selection(&line, &entry, 0, 0, 1, 1, 10, TEST_BG);
    // 无 span 高亮
    assert_eq!(highlighted_text(&highlighted, TEST_BG), "");
    // 完整文本仍为 "abcdef"
    let full: String = highlighted
        .spans
        .iter()
        .map(|s| s.content.as_ref())
        .collect::<Vec<&str>>()
        .concat();
    assert_eq!(full, "abcdef");
}

#[test]
fn test_highlight_preserves_existing_span_style() {
    // line = Span::styled("abc", Style::fg(red))——验证高亮叠加 bg 但保留 fg
    use ratatui_kit::ratatui::style::Color as C;
    let line = Line::from(vec![Span::styled(
        "abc".to_string(),
        Style::default().fg(C::Red),
    )]);
    let entry = WrappedLineInfo {
        slot_index: 0,
        logical_idx: 0,
        visual_start: 0,
        visual_end: 1,
    };
    let highlighted = highlight_line_in_selection(&line, &entry, 0, 0, 0, 2, 10, TEST_BG);
    // 高亮部分 "ab" 应同时保留 fg=Red + bg=TEST_BG
    let highlighted_span = highlighted
        .spans
        .iter()
        .find(|s| s.style.bg == Some(TEST_BG))
        .expect("应有被高亮的 span");
    assert_eq!(highlighted_span.style.fg, Some(C::Red));
    assert_eq!(highlighted_span.content.as_ref(), "ab");
}

// ── [D3 §9] 语义复制：extract_visual_range 带 VM 列表时剥离 UI chrome ────

use crate::kit::message_area::grid::GridSpec;
use crate::kit::message_area::render::vm_to_lines;
use crate::kit::tui_render_unit::{
    TuiAssistantBubble, TuiRenderUnit, TuiToolCard, TuiToolPresentation,
};

/// 构造单 slot 选区环境：渲染 VM 行 + wrap_map + slots（视觉行 = 逻辑行）。
#[allow(clippy::type_complexity)]
fn sel_env(
    vm: TuiRenderUnit,
    grid: &GridSpec,
) -> (
    Vec<Arc<Vec<Line<'static>>>>,
    Vec<usize>,
    Vec<WrappedLineInfo>,
    im::Vector<TuiRenderUnit>,
) {
    let lines = vm_to_lines(&vm, grid);
    // 视宽与生产一致：消息区右缘（term_width - 1，跳过滚动条列）——metadata
    // 右对齐到该列，宽于此值会在 wrap_map 二次折行。
    let (_, wm) = build_wrap_map(&lines, grid.term_width.saturating_sub(1));
    let wm: Vec<WrappedLineInfo> = wm
        .into_iter()
        .map(|mut e| {
            e.slot_index = 0;
            e
        })
        .collect();
    let slots = vec![Arc::new(lines)];
    let offsets = vec![0usize];
    (slots, offsets, wm, im::Vector::from(vec![vm]))
}

/// 完整行选区（中间行）→ 语义全文（无 chrome）。
#[test]
fn test_extract_semantic_plain_middle_line() {
    crate::i18n::init(Some("en"));
    let grid = GridSpec::grid_for(120);
    let vm = TuiRenderUnit::TuiAssistantBubble(TuiAssistantBubble {
        started_at: None,
        duration_ms: None,
        text: "第一段\n\n第二段 with 中文".to_string(),
        reasoning: None,
        message_id: None,
        content_hash: 0,
    });
    let (slots, offsets, wm, vms) = sel_env(vm, &grid);
    let rendered_blank = crate::kit::text_selection::line_to_plain_text(&slots[0][2]);
    assert!(
        rendered_blank.contains('│'),
        "Markdown 段落空行应继续显示 accent 竖线，实际: {rendered_blank:?}"
    );
    // 行 0 = leading 空行；行 1 = 段落 1；行 2 = 段间空行；行 3 = 段落 2——
    // 完整选择（跨逻辑行选区，中间行）
    let text = extract_visual_range(
        &slots,
        &offsets,
        &wm,
        (0, 0),
        (3, 20),
        grid.total_width() as u16,
        Some(&vms),
        Some(grid),
    )
    .expect("选区提取");
    let lines: Vec<&str> = text.lines().collect();
    assert!(
        lines.contains(&"第一段"),
        "段落 1 无前缀 chrome，实际: {text:?}"
    );
    assert!(
        lines.contains(&"第二段 with 中文"),
        "段落 2 无前缀 chrome，实际: {text:?}"
    );
    for l in lines {
        assert!(
            !l.contains('│') && !l.starts_with(' '),
            "行无竖线/前缀空格，实际: {l:?}"
        );
    }
}

/// 跨行拖选：tool header 重建语义（label+summary）、`$ cmd` 保留。
#[test]
fn test_extract_semantic_tool_card_header_and_command() {
    crate::i18n::init(Some("en"));
    let grid = GridSpec::grid_for(120);
    let mut card = TuiToolCard {
        tool_id: "tc-1".into(),
        tool_name: "Bash".into(),
        input_summary: "cargo test -p peri-tui".into(),
        output_summary: "test result: ok. 895 passed".into(),
        is_error: false,
        is_running: false,
        running_duration_ms: None,
        completed_duration_ms: Some(37),
        diff: None,
        presentation: TuiToolPresentation::Generic,
        fold: crate::kit::tui_render_unit::FoldState::Expanded,
        user_modified: false,
        tool_calls_count: 0,
        content_hash: 0,
    };
    card.recompute_hash();
    let vm = TuiRenderUnit::TuiToolCard(card);
    let (slots, offsets, wm, vms) = sel_env(vm, &grid);
    let text = extract_visual_range(
        &slots,
        &offsets,
        &wm,
        (0, 0),
        (3, 40),
        grid.total_width() as u16,
        Some(&vms),
        Some(grid),
    )
    .expect("选区提取");
    assert!(
        text.contains("Shell"),
        "展开态 header 语义 = label（summary 在 `$` 行），实际: {text:?}"
    );
    assert!(
        text.contains("$ cargo test -p peri-tui"),
        "`$ cmd` 行保留 command，实际: {text:?}"
    );
    assert!(
        text.contains("test result: ok. 895 passed"),
        "输出行保留正文，实际: {text:?}"
    );
    assert!(
        !text.contains('✓') && !text.contains('◐') && !text.contains('│'),
        "无状态符号/竖线，实际: {text:?}"
    );
}

/// diff 行：行号 gutter 剥离、patch 标记保留（§9）。
#[test]
fn test_extract_semantic_diff_strips_gutter_keeps_markers() {
    crate::i18n::init(Some("en"));
    let grid = GridSpec::grid_for(120);
    let diff_text = "\
--- a/src/main.rs
+++ b/src/main.rs
@@ -10,2 +10,2 @@
 fn main() {
-    let x = 1;
+    let x = 2;
}
";
    let mut card = TuiToolCard {
        tool_id: "tc-e".into(),
        tool_name: "Edit".into(),
        input_summary: "src/main.rs".into(),
        output_summary: diff_text.to_string(),
        is_error: false,
        is_running: false,
        running_duration_ms: None,
        completed_duration_ms: Some(20),
        diff: crate::kit::diff_parser::parse_unified_diff(diff_text, Some("src/main.rs")),
        presentation: TuiToolPresentation::Generic,
        fold: crate::kit::tui_render_unit::FoldState::Expanded,
        user_modified: false,
        tool_calls_count: 0,
        content_hash: 0,
    };
    card.recompute_hash();
    let vm = TuiRenderUnit::TuiToolCard(card);
    let (slots, offsets, wm, vms) = sel_env(vm, &grid);
    let text = extract_visual_range(
        &slots,
        &offsets,
        &wm,
        (0, 0),
        (20, 0),
        grid.total_width() as u16,
        Some(&vms),
        Some(grid),
    )
    .expect("选区提取");
    assert!(
        text.contains("-     let x = 1;"),
        "del 行保留 patch 标记（`- ` + 4 空格缩进），实际: {text:?}"
    );
    assert!(
        text.contains("+     let x = 2;"),
        "add 行保留 patch 标记，实际: {text:?}"
    );
    // 行号 gutter（" 11" 之类）不得出现
    for line in text.lines() {
        let t = line.trim_start();
        assert!(
            !t.starts_with("11 ") && !t.starts_with("10 "),
            "行号 gutter 被剥离，实际行: {line:?}"
        );
    }
}

/// 部分行选择（选区从行中开始）：映射到语义文本（CJK 半字符边界兼容）。
#[test]
fn test_extract_semantic_partial_row_maps_to_semantic() {
    crate::i18n::init(Some("en"));
    let grid = GridSpec::grid_for(120);
    let vm = TuiRenderUnit::TuiAssistantBubble(TuiAssistantBubble {
        started_at: None,
        duration_ms: None,
        text: "prefix 你好".to_string(),
        reasoning: None,
        message_id: None,
        content_hash: 0,
    });
    let (slots, offsets, wm, vms) = sel_env(vm, &grid);
    // 行 0 = leading 空行；行 1 = 正文（cont_prefix）——选区列 [1, 5) → 语义映射
    let text = extract_visual_range(
        &slots,
        &offsets,
        &wm,
        (1, 1),
        (1, 5),
        grid.total_width() as u16,
        Some(&vms),
        Some(grid),
    )
    .expect("选区提取");
    assert!(
        !text.starts_with(' '),
        "部分选择不包含前缀列，实际: {text:?}"
    );
    assert!(!text.contains('│'), "无竖线，实际: {text:?}");
}

/// view_models=None 时保持既有行为（无剥离）——回归防线。
#[test]
fn test_extract_without_view_models_keeps_plain() {
    crate::i18n::init(Some("en"));
    let grid = GridSpec::grid_for(120);
    let vm = TuiRenderUnit::TuiAssistantBubble(TuiAssistantBubble {
        started_at: None,
        duration_ms: None,
        text: "正文".to_string(),
        reasoning: None,
        message_id: None,
        content_hash: 0,
    });
    let (slots, offsets, wm, _vms) = sel_env(vm, &grid);
    let text = extract_visual_range(
        &slots,
        &offsets,
        &wm,
        (0, 0),
        (1, 10),
        grid.total_width() as u16,
        None,
        None,
    )
    .expect("选区提取");
    // 无剥离：行含前缀（空格/竖线）与状态符号
    assert!(
        text.contains('│') || text.contains(" \u{2502}"),
        "无 VM 时保留 chrome，实际: {text:?}"
    );
}
