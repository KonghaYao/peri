use super::convert::wrap_styled_line;
use ratatui::style::{Color, Modifier};
use ratatui::text::Line;

fn line_text(line: &ratatui::text::Line<'static>) -> String {
    line.spans.iter().map(|s| s.content.as_ref()).collect()
}

/// 超宽 ASCII 行按宽度折为多行，每行 ≤ max_width，不丢内容。
#[test]
fn test_wrap_styled_line_ascii() {
    let line = ratatui::text::Line::from("word ".repeat(40).trim().to_string());
    let rows = wrap_styled_line(&line, 30);
    assert!(rows.len() > 1, "应折为多行");
    for r in &rows {
        assert!(r.width() <= 30, "行宽 {} 超限: {}", r.width(), line_text(r));
    }
    let joined: String = rows.iter().map(line_text).collect::<Vec<_>>().join("");
    assert_eq!(
        joined,
        "word ".repeat(40).trim().to_string(),
        "折行不丢内容"
    );
}

/// CJK 双宽字符按 display width 折行，汉字不被从中间切开。
#[test]
fn test_wrap_styled_line_cjk() {
    let line = ratatui::text::Line::from("测试文本".repeat(20)); // 每个字符宽 2
    let rows = wrap_styled_line(&line, 22); // 奇数宽度边界
    assert!(rows.len() > 1);
    for r in &rows {
        assert!(r.width() <= 22, "行宽 {} 超限", r.width());
    }
    let joined: String = rows.iter().map(line_text).collect::<Vec<_>>().join("");
    assert_eq!(joined, "测试文本".repeat(20), "CJK 折行不丢内容");
}

/// 折行保留 span 样式（行内 `**bold**` 在折行后仍是 bold）。
#[test]
fn test_wrap_styled_line_keeps_styles() {
    use ratatui::text::Span;
    let line = Line::from(vec![
        Span::styled(
            "前缀 ".repeat(10),
            ratatui::style::Style::default().fg(Color::Red),
        ),
        Span::styled(
            "BOLD".repeat(20),
            ratatui::style::Style::default().add_modifier(Modifier::BOLD),
        ),
    ]);
    let rows = wrap_styled_line(&line, 30);
    assert!(rows.len() > 1);
    let has_bold = rows.iter().any(|r| {
        r.spans
            .iter()
            .any(|s| s.style.add_modifier.contains(Modifier::BOLD))
    });
    assert!(has_bold, "折行后应保留 bold 样式");
    // 内容不丢（按 grapheme 拼接）
    let joined: String = rows
        .iter()
        .flat_map(|r| r.spans.iter())
        .map(|s| s.content.as_ref())
        .collect();
    assert_eq!(joined.len(), line_text(&line).len());
}

/// max_width == 0（极端窄屏防御）与不超宽行：原样返回。
#[test]
fn test_wrap_styled_line_zero_width_and_short() {
    let short = ratatui::text::Line::from("abc");
    assert_eq!(wrap_styled_line(&short, 10).len(), 1);
    let rows = wrap_styled_line(&short, 0);
    assert_eq!(rows.len(), 1);
    assert_eq!(line_text(&rows[0]), "abc");
}

/// 单个 grapheme 即超宽（如极窄屏下的 CJK 字符）：独占一行不丢内容。
#[test]
fn test_wrap_styled_line_single_wide_grapheme() {
    let line = ratatui::text::Line::from("汉");
    let rows = wrap_styled_line(&line, 1);
    assert_eq!(rows.len(), 1, "超宽 grapheme 独占一行");
    assert_eq!(line_text(&rows[0]), "汉");
}
