use super::*;
use crate::markdown::GenUiSegment;

const COMPONENTS: &str = r#"
root = Stack([
  Card([
    Heading("状态", 2),
    Text("**纯文本** 中文 👩‍💻 é long wrapped text", "muted"),
    List(["第一项", "第二项"], true),
    Table(["名称", "状态"], [["Agent", "运行中"], ["TUI", "完成"]], "结果"),
    KeyValue([{key: "耗时", value: 12}], "详情"),
    Progress("进度", 50, 100),
    Divider(),
    Button("继续", @ToAssistant("继续处理"), "primary")
  ], "概览"),
  Stack([Text("左侧"), Text("右侧")], "row")
], "column")
"#;

#[test]
fn schema_parses_every_component_in_declared_argument_order() {
    let library = library::library().unwrap();
    assert_eq!(library.root(), Some("Stack"));
    let segment = GenUiSegment::parse(COMPONENTS.into(), false, library);
    assert!(segment.errors.is_empty(), "{:?}", segment.errors);
    assert_eq!(segment.buttons.len(), 1);
    assert_eq!(
        segment.buttons[0].action,
        Ok(GenUiAction::ToAssistant("继续处理".into()))
    );
    assert_eq!(segment, segment.clone());
}

#[test]
fn components_render_within_40_80_120_columns() {
    let segment = GenUiSegment::parse(COMPONENTS.into(), false, library::library().unwrap());
    for theme in [
        peri_theme::builtin::dark_theme(),
        peri_theme::builtin::light_theme(),
    ] {
        for width in [40, 80, 120] {
            let rendered = render(&segment, width, &theme, None);
            let text = rendered
                .lines
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n");
            for expected in [
                "概览",
                "状态",
                "**纯文本**",
                "第一项",
                "Agent",
                "耗时: 12",
                "50%",
                " 继续 ",
                "左侧",
                "右侧",
            ] {
                assert!(
                    text.contains(expected),
                    "width={width}, missing={expected}: {text}"
                );
            }
            assert!(
                rendered.lines.iter().all(|line| line.width() <= width),
                "{text}"
            );
            let [button] = rendered.buttons.as_slice() else {
                panic!("应保留一个按钮");
            };
            assert_eq!(button.line_end, button.line_start + 1);
            assert!(
                rendered.lines[button.line_start]
                    .to_string()
                    .contains(" 继续 ")
            );
        }
    }
}

#[test]
fn partial_streaming_tree_renders_every_prefix_without_panicking() {
    let source = "root = Stack([Card([Text(\"逐步生成中文\"), Button(\"继续\", @ToAssistant(\"下一步\"))], \"标题\")])";
    for (end, _) in source
        .char_indices()
        .chain(std::iter::once((source.len(), ' ')))
    {
        let segment = GenUiSegment::parse(source[..end].into(), true, library::library().unwrap());
        let rendered = render(&segment, 40, &peri_theme::builtin::dark_theme(), None);
        assert!(rendered.lines.iter().all(|line| line.width() <= 40));
    }
    let segment = GenUiSegment::parse(
        "root = Stack([Card([Text(\"已到达\"), Text(\"未完成".into(),
        true,
        library::library().unwrap(),
    );
    let rendered = render(&segment, 40, &peri_theme::builtin::dark_theme(), None);
    assert!(
        rendered
            .lines
            .iter()
            .any(|line| line.to_string().contains("已到达"))
    );
}

#[test]
fn action_descriptors_preserve_types_without_execution() {
    let source = r#"
$count = 1
job = Mutation("later", {})
root = Stack([
 Button("ask", @ToAssistant("hello")),
 Button("set", @Set($count, $count + 1)),
 Button("reset", @Reset($count)),
 Button("run", @Run(job)),
 Button("url", @OpenUrl("https://example.com"))
])"#;
    let segment = GenUiSegment::parse(source.into(), false, library::library().unwrap());
    assert_eq!(segment.parsed.state["$count"], 1);
    assert_eq!(segment.buttons.len(), 5);
    assert!(
        matches!(&segment.buttons[1].action, Ok(GenUiAction::Set(name, _)) if name == "$count")
    );
    assert_eq!(
        segment.buttons[2].action,
        Ok(GenUiAction::Reset(vec!["$count".into()]))
    );
    assert_eq!(
        segment.buttons[3].action,
        Ok(GenUiAction::Run("job".into()))
    );
    assert_eq!(
        segment.buttons[4].action,
        Ok(GenUiAction::OpenUrl("https://example.com".into()))
    );
    let rendered = render(&segment, 80, &peri_theme::builtin::dark_theme(), None);
    assert!(rendered.buttons[0].parsed.is_none());
    assert!(rendered.buttons[1].parsed.is_some());
    assert!(rendered.buttons[2].parsed.is_some());
    assert!(rendered.buttons[3].parsed.is_none());
    assert!(rendered.buttons[4].parsed.is_none());
}

#[test]
fn unsupported_actions_are_visible_muted_errors_without_hit_areas() {
    let theme = peri_theme::builtin::dark_theme();
    for action in [
        "@Missing(\"x\")",
        "Action([@ToAssistant(\"a\"), @ToAssistant(\"b\")])",
        "@ToAssistant(\"a\", {extra: 1})",
        "null",
    ] {
        let segment = GenUiSegment::parse(
            format!("root = Stack([Button(\"bad\", {action})])"),
            false,
            library::library().unwrap(),
        );
        let rendered = render(&segment, 80, &theme, None);
        assert!(rendered.buttons.is_empty());
        assert!(
            rendered
                .lines
                .iter()
                .any(|line| line.to_string().contains("gen-ui:"))
        );
        assert!(
            rendered
                .lines
                .iter()
                .flat_map(|line| &line.spans)
                .any(|span| span.style.fg == Some(theme.semantic.text.muted))
        );
    }
}

#[test]
fn text_reports_non_string_content() {
    let theme = peri_theme::builtin::dark_theme();
    let render_source = |source: &str| {
        let segment = GenUiSegment::parse(source.into(), false, library::library().unwrap());
        let rendered = render(&segment, 80, &theme, None);
        (
            rendered
                .lines
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n"),
            rendered,
        )
    };

    let (text, _) = render_source("root = Stack([Text(\"原样显示\", \"muted\")])");
    assert!(text.contains("原样显示"), "{text}");
    assert!(!text.contains("gen-ui:"), "{text}");

    for (source, expected) in [
        ("$count = 1\nroot = Stack([Text($count)])", "number"),
        ("$seven = 7\nroot = Stack([Text($seven)])", "number"),
        ("$flag = true\nroot = Stack([Text($flag)])", "boolean"),
        ("$off = false\nroot = Stack([Text($off)])", "boolean"),
        ("$obj = {key: 1}\nroot = Stack([Text($obj)])", "object"),
        ("$arr = [1, 2]\nroot = Stack([Text($arr)])", "array"),
        ("$none = null\nroot = Stack([Text($none)])", "content"),
    ] {
        let (text, rendered) = render_source(source);
        assert!(text.contains("gen-ui:"), "{source}: {text}");
        assert!(text.contains(expected), "{source}: {text}");
        assert!(rendered.lines[0].spans[0].style.fg == Some(theme.semantic.text.muted));
    }
}

#[test]
fn generation_budget_errors_render_as_genui_errors() {
    let mut source = String::from("root = Stack([])\n$s0 = \"x\"\n");
    for i in 1..=30 {
        source.push_str(&format!("$s{i} = $s{} + $s{}\n", i - 1, i - 1));
    }
    let segment = GenUiSegment::parse(source, false, library::library().unwrap());
    let rendered = render(&segment, 80, &peri_theme::builtin::dark_theme(), None);
    let text = rendered
        .lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("gen-ui:"), "{text}");
    assert!(text.contains("安全预算"), "{text}");
}

#[test]
fn narrow_nested_layouts_and_oversized_tables_do_not_panic() {
    let source = format!(
        "root = Stack([Table([\"x\"], [[\"{}\"]]), Button(\"非常长的按钮名称\", @ToAssistant(\"next\"))])",
        "x".repeat(20_000)
    );
    let segment = GenUiSegment::parse(source, false, library::library().unwrap());
    for width in [0, 1, 2, 3, 4, 8, 40] {
        let rendered = render(&segment, width, &peri_theme::builtin::dark_theme(), None);
        assert!(rendered.lines.iter().all(|line| line.width() <= width));
    }
}

#[test]
fn button_uses_filled_copy_style_without_brackets() {
    let theme = peri_theme::builtin::dark_theme();
    let source = r#"
$count = 0
root = Stack([
  Button("Go", @ToAssistant("go"), "primary"),
  Button("Reset", @Reset($count))
])"#;
    let segment = GenUiSegment::parse(source.into(), false, library::library().unwrap());
    assert!(segment.errors.is_empty(), "{:?}", segment.errors);
    let rendered = render(&segment, 40, &theme, None);
    let text = rendered
        .lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!text.contains('['), "{text}");
    let primary = rendered
        .lines
        .iter()
        .flat_map(|line| &line.spans)
        .find(|span| span.content.as_ref() == " Go ")
        .expect("primary button span");
    assert_eq!(primary.style.fg, Some(theme.semantic.accent));
    assert!(
        primary
            .style
            .add_modifier
            .contains(ratatui::style::Modifier::REVERSED)
    );
    let secondary = rendered
        .lines
        .iter()
        .flat_map(|line| &line.spans)
        .find(|span| span.content.as_ref() == " Reset ")
        .expect("default button span");
    assert_eq!(secondary.style.fg, Some(theme.semantic.text.muted));
    assert!(
        secondary
            .style
            .add_modifier
            .contains(ratatui::style::Modifier::REVERSED)
    );
    let button = rendered
        .buttons
        .iter()
        .find(|button| button.label == "Go")
        .expect("primary hit area");
    assert_eq!(button.x_end, 4);
}

#[test]
fn charts_render_bars_and_sparklines() {
    let source = r#"
root = Stack([
  BarChart([{label: "API", value: 80}, {label: "Web", value: 40}], "Traffic", 100),
  Sparkline([1, 3, 2, 5, 4, 7], "Requests")
])"#;
    let segment = GenUiSegment::parse(source.into(), false, library::library().unwrap());
    assert!(segment.errors.is_empty(), "{:?}", segment.errors);
    for theme in [
        peri_theme::builtin::dark_theme(),
        peri_theme::builtin::light_theme(),
    ] {
        for width in [40, 80, 120] {
            let rendered = render(&segment, width, &theme, None);
            let text = rendered
                .lines
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n");
            for expected in ["Traffic", "API", "Web", "█", "Requests", "7"] {
                assert!(
                    text.contains(expected),
                    "width={width}, missing={expected}: {text}"
                );
            }
            assert!(
                rendered.lines.iter().all(|line| line.width() <= width),
                "{text}"
            );
        }
    }
}

#[test]
fn charts_report_invalid_data() {
    let theme = peri_theme::builtin::dark_theme();
    for source in [
        "root = Stack([BarChart([])])",
        "root = Stack([BarChart([{label: \"x\", value: -1}])])",
        "root = Stack([BarChart([{label: \"x\", value: 1}], \"t\", 0)])",
        "root = Stack([Sparkline([])])",
        "root = Stack([Sparkline([1, \"x\"])])",
    ] {
        let segment = GenUiSegment::parse(source.into(), false, library::library().unwrap());
        let rendered = render(&segment, 80, &theme, None);
        let text = rendered
            .lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("gen-ui:"), "{source}: {text}");
    }
}
