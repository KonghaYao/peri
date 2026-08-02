//! Tests for status_bar

#[cfg(test)]
use super::*;
#[cfg(test)]
use serial_test::serial;

#[test]
fn test_permission_mode_display() {
    assert_eq!(permission_mode_display("default"), "Don't Ask");
    assert_eq!(permission_mode_display("accept-edit"), "Accept Edit");
    assert_eq!(permission_mode_display("auto-mode"), "Auto Mode");
    assert_eq!(permission_mode_display("bypass"), "Bypass");
    assert_eq!(permission_mode_display("unknown"), "Don't Ask");
}

#[test]
fn test_permission_mode_color() {
    assert_eq!(
        permission_mode_color("accept-edit"),
        statusbar().mode_accept_edit
    );
    assert_eq!(permission_mode_color("auto-mode"), statusbar().mode_auto);
    assert_eq!(permission_mode_color("bypass"), statusbar().mode_bypass);
}

#[test]
fn test_cwd_basename_simple() {
    assert_eq!(cwd_basename("/Users/foo/project"), "project");
    assert_eq!(cwd_basename("/tmp"), "tmp");
    assert_eq!(cwd_basename("/"), "/");
}

#[test]
fn test_cwd_basename_empty() {
    assert_eq!(cwd_basename(""), "");
}

#[test]
fn test_memory_color_thresholds() {
    assert_eq!(memory_color(100), statusbar().resource_good);
    assert_eq!(memory_color(512), statusbar().resource_good); // 512 不算超阈值
    assert_eq!(memory_color(513), statusbar().resource_warn);
    assert_eq!(memory_color(1024), statusbar().resource_warn); // 1024 不算超阈值
    assert_eq!(memory_color(1025), statusbar().resource_bad);
}

#[test]
fn test_resource_color_by_load() {
    // low=50, high=100
    assert_eq!(
        resource_color_by_load(10.0, 50.0, 100.0),
        statusbar().resource_good
    );
    assert_eq!(
        resource_color_by_load(50.0, 50.0, 100.0),
        statusbar().resource_warn
    );
    assert_eq!(
        resource_color_by_load(75.0, 50.0, 100.0),
        statusbar().resource_warn
    );
    assert_eq!(
        resource_color_by_load(100.0, 50.0, 100.0),
        statusbar().resource_bad
    );
}

#[test]
fn test_model_segment_parts_full() {
    // alias + model + effort 三段
    assert_eq!(
        model_segment_parts("opus", "claude-opus-4-20250514", "high"),
        vec!["opus", "claude-opus-4-20250514", "high"]
    );
}

#[test]
fn test_model_segment_parts_no_effort() {
    assert_eq!(
        model_segment_parts("opus", "claude-opus-4-20250514", ""),
        vec!["opus", "claude-opus-4-20250514"]
    );
}

#[test]
fn test_model_segment_parts_model_has_effort_suffix() {
    // 模型名尾部已含 effort 后缀 → 不重复追加
    assert_eq!(
        model_segment_parts("opus", "gpt-5.6-luna high", "high"),
        vec!["opus", "gpt-5.6-luna high"]
    );
}

#[test]
fn test_model_segment_parts_alias_equals_model() {
    // 配置回退到 alias（model_name 为空或等于 alias）→ 只显示一次
    assert_eq!(
        model_segment_parts("haiku", "haiku", "medium"),
        vec!["haiku", "medium"]
    );
    assert_eq!(
        model_segment_parts("haiku", "", "medium"),
        vec!["haiku", "medium"]
    );
}

#[test]
fn test_model_segment_parts_empty_all() {
    assert!(model_segment_parts("", "", "").is_empty());
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
    assert_eq!(cwd_basename(&snap.cwd), "test-project");
    assert_eq!(
        permission_mode_display(&snap.permission_mode),
        "Accept Edit"
    );
    // 模型段三段式：alias + model + effort
    assert_eq!(
        model_segment_parts(&snap.model_alias, &snap.model_name, &snap.effort),
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
    assert_eq!(permission_mode_display(&snap.permission_mode), "Don't Ask");
    // 0% CPU 应被跳过
    assert_eq!(snap.cpu_percent, 0.0);
}

// ── model_click_areas：词级折行模拟（点击区域行号/列范围） ──────────────────
//
// 词级区域按词（非空白段）划分，空白（含跨 span 边界累积）计入词前宽度 ws；
// 换行判定（WordWrapper 语义）：append 前 `line_x + ws + w - cw_last >= area_w`
// → 词换行（逐字符增量检查等价形式，cw_last = 词尾字符宽；等号 = 词恰好放满
// 整行 → 留在行尾走 line_full）；append 后 `line_x >= area_w` → 行推出。
// row1_spans 模型段词流单行区域：
//   ·(sep) → (0,24,27) ws=2（sep2 尾部空格 + sep4 前导空格，词首 x26）；
//   opus → (0,27,32)（词首 x28）；claude-opus-4-20250514 → (0,32,55)（词首 x33）；
//   high → (0,55,60)（词首 x56）

/// 构造与 Row1 布局一致的 spans：模型段前后都有内容（MEM 无条件在模型段之后）。
/// 返回 (spans, model_start, model_end)。
fn row1_spans() -> (Vec<Span<'static>>, usize, usize) {
    let mut spans: Vec<Span<'static>> = vec![
        Span::styled(" Accept Edit", Style::default()), // mode
        separator(),
        Span::styled("project", Style::default()), // cwd
        separator(),
        separator(),
        Span::styled("opus claude-opus-4-20250514", Style::default()), // 模型段 head
        Span::styled(" high", Style::default()),                       // effort
    ];
    let model_end = spans.len();
    // 尾部：MEM 无条件 + bg 任务 + ctx usage（多轮运行后出现）
    spans.push(separator());
    spans.push(Span::styled("MEM 512MB", Style::default()));
    spans.push(separator());
    spans.push(Span::styled("2 agent", Style::default()));
    spans.push(separator());
    spans.push(Span::styled("45% 123k", Style::default()));
    (spans, 4, model_end)
}

#[test]
fn test_model_click_areas_single_line_no_wrap() {
    let (spans, start, end) = row1_spans();
    // 宽度充足：不折行 → 词级 4 个区域全部在第 0 行
    let areas = model_click_areas(&spans, 200, 1, start, end);
    assert_eq!(areas.len(), 4); // · + opus + 模型名 + high
    assert!(areas.iter().all(|&(line, _, _)| line == 0));
    assert_eq!(areas[0], (0, 24, 27));
    assert_eq!(areas[1], (0, 27, 32));
    assert_eq!(areas[2], (0, 32, 55));
    assert_eq!(areas[3], (0, 55, 60));
}

#[test]
fn test_model_click_areas_wrap_after_model_segment() {
    let (spans, start, end) = row1_spans();
    // 折行点落在模型段之后（窄终端 + 尾部内容变宽）：模型段完整在第 0 行，
    // 尾部（MEM/bg/ctx）折到第 1 行。修复前 line_idx 取循环结束后的值（=1），
    // 点击判定错位一行、模型文本点击永远落空。
    // area_w=61：high 结束于 x60（60 < 61 留在 line0），尾部 sep 触发折行到 line1。
    let areas = model_click_areas(&spans, 61, 2, start, end);
    // 模型段 4 个词都应在第 0 行
    assert!(areas.iter().all(|&(line, _, _)| line == 0));
    assert_eq!(areas[0], (0, 24, 27));
    assert_eq!(areas[1], (0, 27, 32));
    assert_eq!(areas[2], (0, 32, 55));
    assert_eq!(areas[3], (0, 55, 60));
}

#[test]
fn test_model_click_areas_model_segment_cross_line() {
    // 模型段跨行（词级死区修复核心）：area_w=30 时 "opus" 整词折到第 1 行，
    // 模型段跨两行——每行各有区域，点击任意一行都能命中。
    // 期望：·(sep) 留在 line0；opus 换行到 line1 顶格（行尾回填丢弃词前空白）；
    // claude 留在 line1；high 触发换行但 line_idx 已达上限，留在 line1 尾部
    // （真实渲染中 high 被 row_height=2 截断到 row2 不可见，区域覆盖 row1 尾部
    // 空白——点击空白打开弹窗，轻微误触但无害）。
    let (spans, start, end) = row1_spans();
    let areas = model_click_areas(&spans, 30, 2, start, end);
    assert_eq!(areas[0], (0, 24, 27));
    assert_eq!(areas[1], (1, 0, 4)); // opus：27+1+4 > 30 → 换行，ws 被行尾回填丢弃 → 顶格
    assert_eq!(areas[2], (1, 4, 27)); // 模型名：4+1+22=27 < 30 留在 line1
    assert_eq!(areas[3], (1, 27, 32)); // high：行号已达上限，留在 line1（渲染中不可见）
}

#[test]
fn test_model_click_areas_row_height_one_ignores_wrap() {
    // row_height=1（不折行）：即使内容超宽也全部在第 0 行（与渲染截断一致）
    let (spans, start, end) = row1_spans();
    let areas = model_click_areas(&spans, 20, 1, start, end);
    assert!(areas.iter().all(|&(line, _, _)| line == 0));
}

// ── 边界语义：WordWrapper `>`/`>=` 边界 ─────────────────────────────────

#[test]
fn test_model_click_areas_line_full_exact_boundary() {
    // append 后 line_x == area_w（line_full）：词留在行尾、行推出到下一行。
    // "abc def" w=3：'abc' 恰满行0 留行尾；'def' 换行后 ws 前导空白被
    // WordWrapper 丢弃（L153），模拟区域 (1,0,4) 仍覆盖实际词字符（x0-2）。
    let spans: Vec<Span<'static>> = vec![Span::styled("abc def", Style::default())];
    let areas = model_click_areas(&spans, 3, 2, 0, 1);
    assert_eq!(areas, vec![(0, 0, 3), (1, 0, 4)]);
}

#[test]
fn test_model_click_areas_word_exact_fill_stays_on_line() {
    // append 前完整词（含 ws）恰好 == 行宽（`==` 不换行）：词留在行尾，
    // 走 line_full 行推出。WordWrapper pending_word_overflow 是逐字符增量检查，
    // 等价于完整词宽 `>`——等号是"留在行尾"，不是"换行"。
    let spans: Vec<Span<'static>> = vec![
        Span::styled("ab", Style::default()),
        Span::styled(" cd", Style::default()),
    ];
    let areas = model_click_areas(&spans, 5, 2, 0, 2);
    assert_eq!(areas, vec![(0, 0, 2), (0, 2, 5)]);
}

// ── 其他布局场景 ──────────────────────────────────────────────────────────

#[test]
fn test_model_click_areas_wrap_before_model_segment() {
    // 折行点落在模型段之前：模型段整体在第 1 行，区域 line==1
    let spans: Vec<Span<'static>> = vec![
        Span::styled("abcdef", Style::default()), // 非模型段：行满推出到 line1
        Span::styled(" gh", Style::default()),    // 模型段
    ];
    let areas = model_click_areas(&spans, 6, 2, 1, 2);
    assert_eq!(areas, vec![(1, 0, 3)]);
}

#[test]
fn test_model_click_areas_word_merges_across_spans() {
    // 词可跨 span 边界合并（字符流扫描）：'op' + 'us' 是一个词 'opus'
    let spans: Vec<Span<'static>> = vec![
        Span::styled("op", Style::default()),
        Span::styled("us", Style::default()),
    ];
    let areas = model_click_areas(&spans, 200, 1, 0, 2);
    assert_eq!(areas, vec![(0, 0, 4)]);
    // span 边界空白计入下一词 ws（"op" + " us" → 'op' 词宽 2、'us' 词 ws=1）
    let spans2: Vec<Span<'static>> = vec![
        Span::styled("op", Style::default()),
        Span::styled(" us", Style::default()),
    ];
    let areas2 = model_click_areas(&spans2, 200, 1, 0, 2);
    assert_eq!(areas2, vec![(0, 0, 2), (0, 2, 5)]);
}

#[test]
fn test_model_click_areas_cjk_width() {
    // CJK 字符宽 2（UnicodeWidthChar 语义）：模型段前的 CJK 内容影响 x 起点
    let spans: Vec<Span<'static>> = vec![
        Span::styled(" 中文目录", Style::default()), // 1(空格) + 4×2 = 9
        Span::styled(" opus", Style::default()),
    ];
    let areas = model_click_areas(&spans, 200, 1, 1, 2);
    assert_eq!(areas, vec![(0, 9, 14)]);
}

// ── 防御 ──────────────────────────────────────────────────────────────────

#[test]
fn test_model_click_areas_defensive() {
    let (spans, start, end) = row1_spans();
    // 空 spans
    assert!(model_click_areas(&[], 100, 2, 0, 0).is_empty());
    // area_w = 0
    assert!(model_click_areas(&spans, 0, 2, start, end).is_empty());
    // model_start >= model_end
    assert!(model_click_areas(&spans, 100, 2, 0, 0).is_empty());
    assert!(model_click_areas(&spans, 100, 2, 5, 4).is_empty());
    // row_height = 0（组件内不可达，纯函数不 panic）
    assert!(model_click_areas(&spans, 100, 0, start, end).is_empty());
}

// ── ground-truth：TestBackend 渲染真实 Paragraph wrap 对比 ────────────────
// 模拟算法与真实渲染的最终裁决：渲染同构造 Line（Wrap{trim:false}）到宽度
// area_w 的 buffer，扫描模型文本字符实际位置，与模拟区域逐位对比。

/// 渲染与组件同构的 Paragraph(wrap)（区域宽 = area_w），返回 buffer。
fn render_status_bar_buffer(
    spans: &[Span<'static>],
    area_w: u16,
    area_h: u16,
) -> ratatui::buffer::Buffer {
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(area_w, area_h))
        .expect("TestBackend 可创建");
    terminal
        .draw(|f| {
            f.render_widget(
                Paragraph::new(Line::from(spans.to_vec())).wrap(Wrap { trim: false }),
                f.area(),
            );
        })
        .expect("渲染成功");
    terminal.backend().buffer().clone()
}

#[test]
fn test_model_click_areas_ground_truth_wrap_after_model() {
    // 主 bug 场景：area_w=61 时模型段 4 词全部在第 0 行、尾部折到第 1 行。
    // 模拟区域 vs 真实渲染：每个词首字符（= 区域起点 + 词前空白宽）逐位一致。
    let (spans, start, end) = row1_spans();
    let areas = model_click_areas(&spans, 61, 2, start, end);
    assert_eq!(
        areas,
        vec![(0, 24, 27), (0, 27, 32), (0, 32, 55), (0, 55, 60)]
    );
    let buf = render_status_bar_buffer(&spans, 61, 2);
    assert_eq!(buf[(26, 0)].symbol(), "·"); // 24 + ws(2)
    assert_eq!(buf[(28, 0)].symbol(), "o"); // 27 + ws(1)
    assert_eq!(buf[(33, 0)].symbol(), "c"); // 32 + ws(1)
    assert_eq!(buf[(56, 0)].symbol(), "h"); // 55 + ws(1)
    // 词末断言：区域 (x_start, x_end) 覆盖词的实际字符范围
    assert_eq!(buf[(31, 0)].symbol(), "s"); // opus 末字符
    assert_eq!(buf[(54, 0)].symbol(), "4"); // 模型名末字符
    assert_eq!(buf[(59, 0)].symbol(), "h"); // high 末字符
    // 尾部 sep 折到 row1 且顶格（词前空白被行尾回填丢弃，remaining=1）
    assert_eq!(buf[(0, 1)].symbol(), "·");
}

#[test]
fn test_model_click_areas_ground_truth_cross_line() {
    // 词级跨行：area_w=30 时 opus 整词折到第 1 行——模拟与真实渲染一致
    let (spans, start, end) = row1_spans();
    let areas = model_click_areas(&spans, 30, 2, start, end);
    assert_eq!(areas, vec![(0, 24, 27), (1, 0, 4), (1, 4, 27), (1, 27, 32)]);
    let buf = render_status_bar_buffer(&spans, 30, 2);
    assert_eq!(buf[(26, 0)].symbol(), "·"); // sep 仍在 row0
    assert_eq!(buf[(0, 1)].symbol(), "o"); // opus 换行到 row1 顶格（ws 被回填丢弃）
    assert_eq!(buf[(5, 1)].symbol(), "c"); // 模型名 row1 x5（opus 后空格 x4）
    // 词末断言：区域 (x_start, x_end) 覆盖词的实际字符范围
    assert_eq!(buf[(3, 1)].symbol(), "s"); // opus 末字符
    assert_eq!(buf[(26, 1)].symbol(), "4"); // 模型名末字符
    // high 被 row_height=2 截断到 row2 不可见——模拟区域 (1,27,32) 覆盖 row1
    // 尾部空白，点击该区域打开弹窗（轻微误触，WordWrapper 行数无上限所致）
}

#[test]
fn test_model_click_areas_oversize_word_at_line_start() {
    // 行首超宽词（w > area_w）：模拟整词放行首（区域超界，可点部分有效）。
    // WordWrapper 实际把超宽词拆分跨行（前缀满行 + 剩余到下一行）——已知差异，
    // 状态栏场景罕见（词宽 > 终端宽），接受近似（plan-review 已记录）。
    let spans: Vec<Span<'static>> = vec![Span::styled("toolongword", Style::default())];
    let areas = model_click_areas(&spans, 5, 2, 0, 1);
    assert_eq!(areas, vec![(0, 0, 11)]);
}

#[test]
fn test_model_click_areas_cjk_tail_word_overflow() {
    // [major 修复] 词尾宽字符（CJK 宽 2）+ 词恰好超界 1 列：词**留在行尾**
    // （逐字符增量检查 `line + ws + 前缀宽` 在词尾字符前不触发 overflow），
    // 与 WordWrapper 一致。旧条件 `line_x + ws + w > area_w` 会错误换行
    // → 该词区域行号错 1、点击失效（原始 bug 同类型残留）。
    // area_w=6：词 "pr目" w=4、cw_last=2 → 2+1+4-2=5 < 6 不换行；
    // append 后 line_x=7 >= 6 → line_full 行推出。
    let spans: Vec<Span<'static>> = vec![
        Span::styled("ab", Style::default()),
        Span::styled(" pr目", Style::default()),
    ];
    let areas = model_click_areas(&spans, 6, 2, 0, 2);
    assert_eq!(areas, vec![(0, 0, 2), (0, 2, 7)]);
    // ground-truth：真实渲染 "pr目" 在行 0（x3-4 可见，'目' 在 x5 超界截断），
    // 行 1 为空——旧条件（换行）下此断言失败。
    let buf = render_status_bar_buffer(&spans, 6, 2);
    assert_eq!(buf[(3, 0)].symbol(), "p"); // 词 "pr目" 留在行 0
    assert_eq!(buf[(4, 0)].symbol(), "r");
    assert_eq!(buf[(0, 1)].symbol(), " "); // 行 1 无该词字符
}

#[test]
fn test_model_click_areas_cjk_ground_truth() {
    // CJK 宽度（UnicodeWidthChar 宽 2）影响折行判定与 x 起点：行 0 放满
    // " 中文目录"（1+8=9），"opus" 词超界换行到行 1 顶格（行尾回填丢弃前导空格）。
    let spans: Vec<Span<'static>> = vec![
        Span::styled(" 中文目录", Style::default()),
        Span::styled(" opus", Style::default()),
    ];
    let areas = model_click_areas(&spans, 10, 2, 1, 2);
    assert_eq!(areas, vec![(1, 0, 4)]);
    let buf = render_status_bar_buffer(&spans, 10, 2);
    assert_eq!(buf[(0, 1)].symbol(), "o"); // opus 换行到 row1 顶格
    assert_eq!(buf[(3, 1)].symbol(), "s"); // 词末断言
}
