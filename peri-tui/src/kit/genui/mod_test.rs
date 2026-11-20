//! GenUI 应用包装的渲染回归：图表走 tui-chart，与 storybook、消息区同源。
//!
//! 消息区 `renderer.rs` 调用 [`super::render`]；这里从同一入口渲染 OpenUI 源码，
//! 断言 tui-chart 的坐标轴与图形字形（手写文本渲染没有这些字形）。

use super::render;
use crate::kit::markdown::GenUiSegment;

fn segment(source: &str) -> GenUiSegment {
    GenUiSegment::parse(
        source.into(),
        false,
        peri_tui_components::genui::library::library().unwrap(),
    )
}

fn lines(source: &str, width: usize) -> Vec<String> {
    let theme = peri_theme::builtin::dark_theme();
    render(&segment(source), width, &theme)
        .lines
        .iter()
        .map(ToString::to_string)
        .collect()
}

#[test]
fn bar_chart_uses_tui_chart_axes_and_bars() {
    let lines = lines(
        r#"root = BarChart([{label: "Markdown", value: 42}, {label: "OpenUI", value: 28}, {label: "交互", value: 15}], "每周渲染")"#,
        80,
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("Markdown") && line.contains("42")),
        "bar labels and values: {lines:#?}"
    );
    assert!(
        lines.iter().any(|line| line.contains('█')),
        "tui-chart bar blocks: {lines:#?}"
    );
    assert!(
        lines.iter().any(|line| line.contains('└')),
        "tui-chart x axis: {lines:#?}"
    );
}

#[test]
fn bar_chart_shows_every_value_including_zero_and_max() {
    // tui-chart 的 outside 数值标签对 value=0 和贴右边界的最长条会跳过绘制，
    // 数值列必须由 render 侧补齐，不能依赖图表库。
    let lines = lines(
        r#"root = BarChart([{label: "Zero", value: 0}, {label: "Max", value: 50}, {label: "Mid", value: 10}])"#,
        40,
    );
    for (label, value) in [("Zero", "0"), ("Max", "50"), ("Mid", "10")] {
        assert!(
            lines
                .iter()
                .any(|line| line.contains(label) && line.contains(value)),
            "{label} 行缺少数值 {value}: {lines:#?}"
        );
    }
}

#[test]
fn sparkline_uses_tui_chart_glyphs_beside_the_value() {
    let lines = lines(
        r#"root = Sparkline([1, 3, 2, 5, 4, 7, 6, 9, 8, 11], "延迟抖动")"#,
        80,
    );
    assert_eq!(lines.len(), 1);
    let line = &lines[0];
    assert!(line.contains("延迟抖动"), "{line:?}");
    assert!(
        line.chars()
            .any(|ch| ('\u{2581}'..='\u{2588}').contains(&ch)),
        "tui-chart sparkline glyphs: {line:?}"
    );
    assert!(
        line.trim_end().ends_with("11"),
        "trailing last value: {line:?}"
    );
}
