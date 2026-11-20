//! Tests for status_bar

use super::Line;
use super::Paragraph;
use super::Span;
use super::Style;
use super::atoms;

#[cfg(test)]
use serial_test::serial;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui_kit::prelude::element;

#[test]
fn test_permission_mode_display() {
    assert_eq!(super::permission_mode_display(""), "Initializing…");
    assert_eq!(super::permission_mode_display("default"), "Don't Ask");
    assert_eq!(super::permission_mode_display("accept-edit"), "Accept Edit");
    assert_eq!(super::permission_mode_display("auto-mode"), "Auto Mode");
    assert_eq!(super::permission_mode_display("bypass"), "Bypass");
    assert_eq!(super::permission_mode_display("unknown"), "Don't Ask");
}

#[test]
fn test_preparing_label_only_while_preparing() {
    // 会话建立期间（用户提交了输入、会话还没建好）在别处没有任何可见投影，
    // 状态栏必须明确说明正在准备，而不是保持提交前的样子。
    assert_eq!(
        super::preparing_label(true).as_deref(),
        Some("Preparing session…"),
        "准备在途时状态栏应显示正在准备会话"
    );
    assert_eq!(
        super::preparing_label(false),
        None,
        "准备结束后不得留下提示"
    );
}

#[test]
fn test_permission_mode_color() {
    assert_eq!(
        super::permission_mode_color("accept-edit"),
        super::statusbar().mode_accept_edit
    );
    assert_eq!(
        super::permission_mode_color("auto-mode"),
        super::statusbar().mode_auto
    );
    assert_eq!(
        super::permission_mode_color("bypass"),
        super::statusbar().mode_bypass
    );
}

#[test]
fn test_cwd_basename_simple() {
    assert_eq!(super::cwd_basename("/Users/foo/project"), "project");
    assert_eq!(super::cwd_basename("/tmp"), "tmp");
    assert_eq!(super::cwd_basename("/"), "/");
}

#[test]
fn test_cwd_basename_empty() {
    assert_eq!(super::cwd_basename(""), "");
}

#[test]
fn test_model_segment_parts_full() {
    // alias + model + effort 三段
    assert_eq!(
        super::model_segment_parts("opus", "claude-opus-4-20250514", "high"),
        vec!["opus", "claude-opus-4-20250514", "high"]
    );
}

#[test]
fn test_model_segment_parts_no_effort() {
    assert_eq!(
        super::model_segment_parts("opus", "claude-opus-4-20250514", ""),
        vec!["opus", "claude-opus-4-20250514"]
    );
}

#[test]
fn test_model_segment_parts_model_has_effort_suffix() {
    // 模型名尾部已含 effort 后缀 → 不重复追加
    assert_eq!(
        super::model_segment_parts("opus", "gpt-5.6-luna high", "high"),
        vec!["opus", "gpt-5.6-luna high"]
    );
}

#[test]
fn test_model_segment_parts_alias_equals_model() {
    // 配置回退到 alias（model_name 为空或等于 alias）→ 只显示一次
    assert_eq!(
        super::model_segment_parts("haiku", "haiku", "medium"),
        vec!["haiku", "medium"]
    );
    assert_eq!(
        super::model_segment_parts("haiku", "", "medium"),
        vec!["haiku", "medium"]
    );
}

#[test]
fn test_model_segment_parts_empty_all() {
    assert!(super::model_segment_parts("", "", "").is_empty());
}

#[test]
#[serial]
fn test_status_bar_row_renders_without_panic() {
    crate::kit::atoms::init_atoms();
    // 写入测试数据
    *atoms::SERVICE_SNAPSHOT.state().write() = atoms::ServiceSnapshot {
        cwd: "/home/user/test-project".into(),
        provider_name: "anthropic".into(),
        model_alias: "sonnet".into(),
        model_name: "claude-sonnet-4-20250514".into(),
        effort: "high".into(),
        permission_mode: "accept-edit".into(),
        memory_mb: 256,
        cpu_percent: 12.5,
        ..Default::default()
    };
    // 辅助函数应能正确处理这些值
    let snap = atoms::SERVICE_SNAPSHOT.state().read().clone();
    assert_eq!(snap.cwd, "/home/user/test-project");
    assert_eq!(super::cwd_basename(&snap.cwd), "test-project");
    assert_eq!(
        super::permission_mode_display(&snap.permission_mode),
        "Accept Edit"
    );
    // 模型段三段式：alias + model + effort
    assert_eq!(
        super::model_segment_parts(&snap.model_alias, &snap.model_name, &snap.effort),
        vec!["sonnet", "claude-sonnet-4-20250514", "high"]
    );
}

#[test]
#[serial]
fn test_status_bar_handles_empty_provider_model() {
    crate::kit::atoms::init_atoms();
    *atoms::SERVICE_SNAPSHOT.state().write() = atoms::ServiceSnapshot {
        cwd: "/tmp".into(),
        provider_name: "".into(),
        model_alias: "".into(),
        model_name: "".into(),
        permission_mode: "default".into(),
        memory_mb: 0,
        cpu_percent: 0.0,
        ..Default::default()
    };
    let snap = atoms::SERVICE_SNAPSHOT.state().read().clone();
    // 空 provider/model 应被渲染逻辑跳过（不在 Row1 中显示）
    assert!(snap.provider_name.is_empty());
    assert!(snap.model_alias.is_empty());
    assert!(snap.model_name.is_empty());
    // Default mode → Don't Ask 标签
    assert_eq!(
        super::permission_mode_display(&snap.permission_mode),
        "Don't Ask"
    );
    // 0% CPU 应被跳过
    assert_eq!(snap.cpu_percent, 0.0);
}

// ── 共享布局：可见行与命中区间一次产出 ───────────────────────────────────────
//
// 绘制端直接渲染 `layout.lines`（不再交给 Paragraph 折行），命中也从同一份
// 可见单元格推导，因此不存在「绘制行」与「命中行」两套计算。

/// 构造与 Row1 布局一致的 spans：模型段前后都有内容（尾部 bg 任务在模型段之后）。
/// 返回 (spans, 模型段 span 范围)；模型段包含其前置分隔符（与组件内 model_start 一致）。
fn row1_spans() -> (Vec<Span<'static>>, std::ops::Range<usize>) {
    let mut spans: Vec<Span<'static>> = vec![
        Span::styled(" Accept Edit", Style::default()), // mode
        super::separator(),
        Span::styled("project", Style::default()), // cwd
        super::separator(),
        super::separator(),
        Span::styled("opus claude-opus-4-20250514", Style::default()), // 模型段 head
        Span::styled(" high", Style::default()),                       // effort
    ];
    let model_end = spans.len();
    // 尾部：bg 任务（模型段后紧跟一个 sep）
    spans.push(super::separator());
    spans.push(Span::styled("2 agent", Style::default()));
    (spans, 4..model_end)
}

fn layout(
    spans: &[Span<'static>],
    width: u16,
    model: Option<std::ops::Range<usize>>,
    goal: Option<std::ops::Range<usize>>,
) -> super::StatusBarLayout {
    super::layout_status_bar(spans, width, super::ROW1_MAX_LINES, model, goal)
}

fn layout_text(layout: &super::StatusBarLayout) -> Vec<String> {
    layout
        .lines
        .iter()
        .map(|line: &Line<'static>| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect()
}

/// 与组件绘制端同构：渲染布局行，不启用任何折行。
fn render_layout(layout: &super::StatusBarLayout, width: u16, height: u16) -> Buffer {
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height))
        .expect("TestBackend 可创建");
    terminal
        .draw(|f| {
            f.render_widget(Paragraph::new(layout.lines.clone()), f.area());
        })
        .expect("渲染成功");
    terminal.backend().buffer().clone()
}

fn buffer_rows(buf: &Buffer) -> Vec<String> {
    (0..buf.area.height)
        .map(|y| {
            (0..buf.area.width)
                .map(|x| buf[(x, y)].symbol())
                .collect::<String>()
        })
        .collect()
}

/// 命中查询的测试包装：Row1 区域从 (0,0) 起。
fn hit(hits: &[(u16, u16, u16)], width: u16, column: u16, row: u16) -> bool {
    super::hit_test(hits, Rect::new(0, 0, width, 2), column, row).is_some()
}

#[test]
fn test_status_layout_wide_single_line_and_model_hits() {
    let (spans, model) = row1_spans();
    let layout = layout(&spans, 200, Some(model), None);

    assert_eq!(layout.lines.len(), 1, "宽屏应保持单行");
    assert_eq!(
        layout_text(&layout),
        [" Accept Edit · project ·  · opus claude-opus-4-20250514 high · 2 agent"]
    );
    // 模型段命中从它自己的前置分隔符起，到 effort 结束；不含上一段的空格与尾部空白。
    assert_eq!(layout.model_hits, [(0, 25, 60)]);
    assert!(layout.goal_hits.is_empty());

    assert!(hit(&layout.model_hits, 200, 25, 0));
    assert!(hit(&layout.model_hits, 200, 26, 0), "分隔符 · 可点击");
    assert!(!hit(&layout.model_hits, 200, 24, 0), "上一段尾部空白不响应");
    assert!(!hit(&layout.model_hits, 200, 60, 0), "段尾空白不响应");

    let buf = render_layout(&layout, 200, 1);
    assert_eq!(buf[(26, 0)].symbol(), "·");
    assert_eq!(buf[(28, 0)].symbol(), "o");
    assert_eq!(buf[(56, 0)].symbol(), "h");
}

#[test]
fn test_status_layout_narrow_long_word_hits_follow_second_line() {
    // 偏差一：宽 5 时长词被拆到第二行，旧命中却全留在第一行。
    let spans = vec![
        Span::styled("ab ", Style::default()),
        Span::styled("toolongword", Style::default()),
    ];
    let layout = layout(&spans, 5, Some(1..2), None);

    assert_eq!(
        layout_text(&layout),
        ["ab", "toolo"],
        "长词按列宽推进第二行"
    );
    assert_eq!(layout.model_hits, [(1, 0, 5)], "命中只在长词实际出现的行");
    assert!(!hit(&layout.model_hits, 5, 3, 0), "第一行没有模型字符");
    assert!(hit(&layout.model_hits, 5, 4, 1));

    let buf = render_layout(&layout, 5, 2);
    assert_eq!(buf[(0, 0)].symbol(), "a");
    assert_eq!(buf[(1, 0)].symbol(), "b");
    assert_eq!(buf[(0, 1)].symbol(), "t");
    assert_eq!(buf[(4, 1)].symbol(), "o");
}

#[test]
fn test_status_layout_clipped_third_line_has_no_phantom_hit() {
    // 偏差二：被第三行裁掉的 effort 曾在第二行尾部留下热区。
    let (spans, model) = row1_spans();
    let layout = layout(&spans, 30, Some(model), None);

    assert_eq!(
        layout_text(&layout),
        [" Accept Edit · project ·  ·", "opus claude-opus-4-20250514"]
    );
    assert_eq!(layout.model_hits, [(0, 25, 27), (1, 0, 27)]);
    assert!(
        !layout_text(&layout)
            .iter()
            .any(|line| line.contains("high")),
        "第三行内容不得出现在可见行"
    );

    let buf = render_layout(&layout, 30, 2);
    assert_eq!(buf[(26, 0)].symbol(), "·");
    assert_eq!(buf[(0, 1)].symbol(), "o");
    assert_eq!(buf[(26, 1)].symbol(), "4");
    assert_eq!(buf[(27, 1)].symbol(), " ", "可见行尾部是空白");
    for column in 27..30 {
        assert!(
            !hit(&layout.model_hits, 30, column, 1),
            "被裁掉内容的位置不应可点：x={column}"
        );
    }
}

#[test]
fn test_status_layout_goal_hits_cover_only_visible_cells() {
    let spans = vec![
        Span::styled("head", Style::default()),
        super::separator(),
        Span::styled("Goal: Active · ↻2", Style::default()),
        super::separator(),
        Span::styled("1 shell", Style::default()),
    ];
    let layout = layout(&spans, 12, None, Some(1..3));

    assert_eq!(layout_text(&layout), ["head · Goal:", "Active · ↻2"]);
    assert_eq!(layout.goal_hits, [(0, 4, 12), (1, 0, 11)]);
    assert!(hit(&layout.goal_hits, 12, 5, 0), "Goal 前置分隔符可点击");
    assert!(hit(&layout.goal_hits, 12, 10, 1), "可见的 ↻2 可点击");
    assert!(!hit(&layout.goal_hits, 12, 11, 1), "可见行尾部空白不响应");
    assert!(
        !hit(&layout.goal_hits, 12, 3, 0),
        "前置段与 Goal 段之间空白不响应"
    );

    let buf = render_layout(&layout, 12, 2);
    assert_eq!(buf[(5, 0)].symbol(), "·");
    assert_eq!(buf[(0, 1)].symbol(), "A");
}

#[test]
fn test_status_layout_overlong_model_name_splits_visible() {
    // 模型名比整行还宽：按显示列宽拆分，只对可见部分保留热区。
    let spans = vec![
        Span::styled("m", Style::default()),
        super::separator(),
        Span::styled("abcdefghijklmnopqrstuvwxyz0123456789", Style::default()),
    ];
    let layout = layout(&spans, 12, Some(1..3), None);

    assert_eq!(layout_text(&layout), ["m ·", "abcdefghijkl"]);
    assert_eq!(layout.model_hits, [(0, 1, 3), (1, 0, 12)]);
    assert!(
        layout
            .model_hits
            .iter()
            .all(|&(line, start, end)| usize::from(line) < layout.lines.len()
                && start < end
                && end <= 12)
    );
    assert!(!hit(&layout.model_hits, 12, 12, 1));
    assert!(
        layout.lines.iter().all(|line| line.width() <= 12usize),
        "可见行不得超过终端宽度"
    );
}

#[test]
fn test_status_layout_resize_recomputes_lines_and_hits() {
    let (spans, model) = row1_spans();
    let cases = [
        (120u16, 1usize, false),
        (40, 2, false),
        (30, 2, false),
        (5, 2, true),
    ];
    for (width, expected_lines, model_hidden) in cases {
        let layout = layout(&spans, width, Some(model.clone()), None);
        assert_eq!(
            layout.lines.len(),
            expected_lines,
            "宽度 {width} 的可见行数"
        );
        assert!(
            layout
                .lines
                .iter()
                .all(|line| line.width() <= usize::from(width)),
            "宽度 {width} 的可见行超宽"
        );
        assert!(
            layout.model_hits.iter().all(|&(line, start, end)| {
                usize::from(line) < layout.lines.len() && start < end && end <= width
            }),
            "宽度 {width} 的热区必须落在可见行内"
        );
        assert_eq!(
            layout.model_hits.is_empty(),
            model_hidden,
            "宽度 {width} 的模型段可见性与热区一致"
        );
    }
}

#[test]
fn test_status_layout_cjk_word_moves_whole_to_next_line() {
    // 双宽字符不拆半：整词换行，命中跟着词走（旧实现留在行尾并截断）。
    let spans = vec![
        Span::styled("ab", Style::default()),
        Span::styled(" pr目", Style::default()),
    ];
    let layout = layout(&spans, 6, Some(0..2), None);

    assert_eq!(layout_text(&layout), ["ab", "pr目"]);
    assert_eq!(layout.model_hits, [(0, 0, 2), (1, 0, 4)]);
    let buf = render_layout(&layout, 6, 2);
    assert_eq!(buf[(0, 1)].symbol(), "p");
    assert_eq!(buf[(2, 1)].symbol(), "目");
    assert_eq!(buf[(4, 1)].symbol(), " ");
}

#[test]
fn test_status_layout_word_merges_across_spans() {
    // 词可跨 span 边界合并；命中按单元格所属 span 划分。
    let spans = vec![
        Span::styled("op", Style::default()),
        Span::styled("us rest", Style::default()),
    ];
    let layout = layout(&spans, 200, Some(0..1), None);

    assert_eq!(layout_text(&layout), ["opus rest"]);
    assert_eq!(layout.model_hits, [(0, 0, 2)], "只覆盖模型 span 的两个字符");
}

#[test]
fn test_status_layout_defensive_inputs() {
    // 空内容仍保留一行（组件高度至少 1）。
    let empty = layout(&[], 10, None, None);
    assert_eq!(empty.lines.len(), 1);
    assert_eq!(layout_text(&empty), [""]);
    assert!(empty.model_hits.is_empty());

    // 首帧宽度未知：保持单行，且不产生无法校验的热区。
    let (spans, model) = row1_spans();
    let unknown = layout(&spans, 0, Some(model), None);
    assert_eq!(unknown.lines.len(), 1);
    assert_eq!(layout_text(&unknown).len(), 1);
    assert!(unknown.model_hits.is_empty());
}

#[test]
fn test_hit_test_requires_visible_row_and_column() {
    let hits = [(0, 2, 5), (1, 0, 3)];
    let area = Rect::new(4, 10, 40, 2);
    // 行、列都换算到屏幕坐标；区间右端为开区间。
    assert_eq!(super::hit_test(&hits, area, 6, 10), Some(6));
    assert_eq!(super::hit_test(&hits, area, 8, 10), Some(6));
    assert_eq!(super::hit_test(&hits, area, 9, 10), None);
    assert_eq!(super::hit_test(&hits, area, 5, 10), None);
    assert_eq!(super::hit_test(&hits, area, 4, 11), Some(4));
    assert_eq!(super::hit_test(&hits, area, 7, 11), None);
    assert_eq!(super::hit_test(&hits, area, 6, 12), None);
}

// ── 组件接线：Row1 直接消费布局行 ────────────────────────────────────────────

struct ServiceSnapshotGuard(atoms::ServiceSnapshot);

impl ServiceSnapshotGuard {
    fn install(snapshot: atoms::ServiceSnapshot) -> Self {
        let previous = std::mem::take(&mut *atoms::SERVICE_SNAPSHOT.state().write());
        *atoms::SERVICE_SNAPSHOT.state().write() = snapshot;
        Self(previous)
    }
}

impl Drop for ServiceSnapshotGuard {
    fn drop(&mut self) {
        *atoms::SERVICE_SNAPSHOT.state().write() = std::mem::take(&mut self.0);
    }
}

#[test]
#[serial]
fn test_status_bar_row1_draws_layout_lines_at_narrow_width() {
    crate::kit::atoms::init_atoms();
    let _guard = ServiceSnapshotGuard::install(atoms::ServiceSnapshot {
        cwd: "/home/user/test-project".into(),
        provider_name: "anthropic".into(),
        model_alias: "sonnet".into(),
        model_name: "claude-sonnet-4-20250514".into(),
        effort: "high".into(),
        permission_mode: "accept-edit".into(),
        ..Default::default()
    });

    // 宽 30：Row1 折为两行，第二行只放得下模型段首词；状态栏自身不启用 Paragraph 折行。
    let buf = ratatui_kit::test_util::render_frames(element!(super::StatusBarRow1()), 30, 2, 3);
    let rows = buffer_rows(&buf);

    assert_eq!(buf[(28, 0)].symbol(), "·", "模型前置分隔符留在第一行");
    assert_eq!(rows[1].trim_end(), "sonnet", "第二行由布局产出");
    assert!(
        !rows.iter().any(|row| row.contains("claude-sonnet")),
        "第三行内容被裁掉：{rows:?}"
    );
}
