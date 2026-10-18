//! MCP 详情四段表格测试：行数契约、区块/行渲染、代理派生、订阅开关与宽度边界。
//!
//! 与通用详情布局测试（`tests.rs`）分开，避免单文件越过 STD-SIZE-001 上限；
//! 复用其行文本/命中行 helper。

use super::detail::{DetailAction, SkillEntry, build_detail_lines, detail_btn_row};
use super::tests::{TEST_WIDTH, detail_lines, detail_lines_with_skills, line_text, rendered_row};
use crate::i18n;
use crate::kit::atoms::{
    McpResourceSummary, McpServerSummary, McpSubscriptionsSummary, McpToolSummary,
};
use fluent_bundle::FluentValue;
use unicode_width::UnicodeWidthStr;

/// 代表数据：工具溢出投影、agent 与非 agent 资源、非空订阅、错误与 URL 全开。
fn rich_summary() -> McpServerSummary {
    McpServerSummary {
        name: "demo".into(),
        url: Some("https://example.com/mcp".into()),
        error_summary: Some("boom".into()),
        tools_count: 70,
        tools: vec![
            McpToolSummary {
                name: "search".into(),
                description: Some("Find things".into()),
            },
            McpToolSummary {
                name: "alpha".into(),
                description: None,
            },
        ],
        resources_count: 3,
        resources: vec![
            McpResourceSummary {
                uri: "agent://builtin/reviewer/agent.md".into(),
                name: Some("Reviewer".into()),
                description: Some("Reviews code".into()),
            },
            McpResourceSummary {
                uri: "agent://plugin/market/helper/agent.md".into(),
                name: None,
                description: None,
            },
            McpResourceSummary {
                uri: "file:///notes/todo.md".into(),
                name: Some("todo".into()),
                description: None,
            },
        ],
        subscriptions: Some(McpSubscriptionsSummary {
            resources: vec!["file:///notes/1.md".into()],
            tools_list_changed: true,
            prompts_list_changed: false,
            resources_list_changed: true,
        }),
        ..McpServerSummary::default()
    }
}

fn rich_skills() -> Vec<SkillEntry> {
    vec![
        SkillEntry {
            name: "demo:zeta".into(),
            description: "Zeta skill".into(),
        },
        SkillEntry {
            name: "demo:alpha".into(),
            description: "Alpha skill".into(),
        },
    ]
}

/// 行数契约：四段全非空时 detail_btn_row 必须等于真实渲染的按钮行。
#[test]
fn detail_tables_row_count_matches_rendered_lines() {
    let summary = rich_summary();
    let skills = rich_skills();
    let lines = detail_lines_with_skills(&summary, &skills, 0);
    let back = DetailAction::Back.text(&i18n::tr("panel-mcp-detail-btn-back"));
    assert_eq!(
        rendered_row(&lines, &back),
        detail_btn_row(&summary, skills.len(), TEST_WIDTH) as usize,
        "表格行数推导必须与渲染一致"
    );
    assert_eq!(rendered_row(&lines, &back), lines.len() - 3);
}

/// 四个区块标题固定渲染；工具/技能/代理行与订阅开关同时可见。
#[test]
fn detail_tables_render_sections_rows_and_subscriptions() {
    let summary = rich_summary();
    let skills = rich_skills();
    let text: Vec<String> = detail_lines_with_skills(&summary, &skills, 0)
        .iter()
        .map(line_text)
        .collect();
    for key in [
        "panel-mcp-table-tools",
        "panel-mcp-table-skills",
        "panel-mcp-table-agents",
        "panel-mcp-table-subscriptions",
    ] {
        let label = i18n::tr(key);
        assert!(
            text.iter().any(|line| line.contains(&label)),
            "缺少区块标题 {key}"
        );
    }
    // 工具行（名称 + 描述）与技能行（fullname）。
    assert!(
        text.iter()
            .any(|line| line.contains("search") && line.contains("Find things"))
    );
    assert!(
        text.iter()
            .any(|line| line.contains("demo:alpha") && line.contains("Alpha skill"))
    );
    // agent:// 派生代理行；非 agent 资源不进入代理表。
    assert!(
        text.iter()
            .any(|line| line.contains("reviewer") && line.contains("Reviews code"))
    );
    assert!(text.iter().any(|line| line.contains("helper")));
    assert!(!text.iter().any(|line| line.contains("todo")));
    // 订阅：资源行 + 三个标志行（开/关）。
    let on = i18n::tr("panel-mcp-sub-on");
    let off = i18n::tr("panel-mcp-sub-off");
    assert!(text.iter().any(|line| line.contains("file:///notes/1.md")));
    assert!(
        text.iter()
            .any(|line| line.contains("tools/list_changed") && line.contains(&on))
    );
    assert!(
        text.iter()
            .any(|line| line.contains("prompts/list_changed") && line.contains(&off))
    );
    // 名称列在 TEST_WIDTH（60）下为 20 列：resources/list_changed 按设计截断，
    // 用稳定可见前缀断言该行仍存在且开关值为「开」。
    assert!(
        text.iter()
            .any(|line| line.contains("resources/list_") && line.contains(&on))
    );
    // 投影溢出：more 行给出剩余条目数。
    let more = i18n::tr_args(
        "panel-mcp-table-more",
        &[("count".to_string(), FluentValue::from(68i64))],
    );
    assert!(text.iter().any(|line| line.contains(&more)));
}

/// 空数据仍渲染四段（标题 + 四空格破折号占位），且表格行不越界。
#[test]
fn detail_tables_render_empty_placeholders_and_fit_width() {
    let lines = detail_lines(&McpServerSummary::default(), 0);
    let text: Vec<String> = lines.iter().map(line_text).collect();
    let empty = i18n::tr("panel-mcp-table-empty");
    for key in [
        "panel-mcp-table-tools",
        "panel-mcp-table-skills",
        "panel-mcp-table-agents",
        "panel-mcp-table-subscriptions",
    ] {
        let label = i18n::tr(key);
        let index = text
            .iter()
            .position(|line| line.contains(&label))
            .unwrap_or_else(|| panic!("缺少区块标题 {key}"));
        assert_eq!(
            text[index + 1],
            format!("    {empty}"),
            "{key} 空节必须渲染占位"
        );
    }

    let rich = detail_lines_with_skills(&rich_summary(), &rich_skills(), 0);
    let mut table_rows = 0;
    for line in &rich {
        let rendered = line_text(line);
        let trimmed = rendered.trim_start();
        if ["┌", "├", "└", "│"]
            .iter()
            .any(|border| trimmed.starts_with(border))
        {
            table_rows += 1;
            assert!(
                rendered.starts_with("  "),
                "表格行必须缩进 2 列: {rendered}"
            );
            assert!(
                UnicodeWidthStr::width(rendered.as_str()) <= TEST_WIDTH as usize,
                "表格行不得超越面板宽度: {rendered}"
            );
        }
    }
    assert!(table_rows > 0, "代表数据必须渲染出表格");
}

/// CJK/emoji 单元格按显示宽度截断补齐：所有表格行等宽、不越界，截断处有
/// 省略号（不是按字符数截断）。
#[test]
fn detail_tables_truncate_cjk_and_emoji_by_display_width() {
    let summary = McpServerSummary {
        tools_count: 1,
        tools: vec![McpToolSummary {
            name: "搜索工具🛠️".into(),
            description: Some("很长的中文描述，需要按显示宽度截断而不是按字符数。".repeat(3)),
        }],
        ..McpServerSummary::default()
    };
    let lines = detail_lines(&summary, 0);
    let table_rows: Vec<String> = lines
        .iter()
        .map(line_text)
        .filter(|line| {
            let trimmed = line.trim_start();
            ["┌", "├", "└", "│"]
                .iter()
                .any(|border| trimmed.starts_with(border))
        })
        .collect();
    assert!(table_rows.len() >= 5, "工具表必须渲染");
    let width = UnicodeWidthStr::width(table_rows[0].as_str());
    for row in &table_rows {
        assert_eq!(
            UnicodeWidthStr::width(row.as_str()),
            width,
            "表格行必须等宽: {row}"
        );
        assert!(
            width <= TEST_WIDTH as usize,
            "表格行不得越过面板宽度: {row}"
        );
    }
    let tool_row = table_rows
        .iter()
        .find(|row| row.contains("搜索工具"))
        .expect("工具名必须可见");
    assert!(tool_row.contains('…'), "超长中文描述必须被截断: {tool_row}");
}

/// 窄面板：表格总宽夹到可用宽度；低于最小可渲染宽度时退回占位行，按钮行号
/// 与渲染同步收缩（宽度参与行数推导）。
#[test]
fn detail_tables_fit_narrow_width_and_fall_back_to_placeholder() {
    let theme = peri_theme::builtin::dark_theme();
    let summary = rich_summary();
    let skills = rich_skills();
    let back = DetailAction::Back.text(&i18n::tr("panel-mcp-detail-btn-back"));
    for width in [40u16, 24, 20, 12, 10, 8] {
        let lines = build_detail_lines(&summary, &skills, 0, &theme, width);
        let text: Vec<String> = lines.iter().map(line_text).collect();
        let table_rows: Vec<&String> = text
            .iter()
            .filter(|line| {
                let trimmed = line.trim_start();
                ["┌", "├", "└", "│"]
                    .iter()
                    .any(|border| trimmed.starts_with(border))
            })
            .collect();
        if width >= 12 {
            assert!(!table_rows.is_empty(), "width={width} 必须渲染表格");
            for row in table_rows {
                assert!(
                    UnicodeWidthStr::width(row.as_str()) <= width as usize,
                    "width={width} 表格行溢出: {row}"
                );
            }
        } else {
            assert!(table_rows.is_empty(), "width={width} 过窄必须退回占位行");
        }
        assert_eq!(
            rendered_row(&lines, &back),
            detail_btn_row(&summary, skills.len(), width) as usize,
            "width={width} 按钮行号必须与渲染一致"
        );
    }
}
