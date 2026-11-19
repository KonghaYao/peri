//! 第 4 部分验收：概览派生、计数、长链摘要与图详情。

use unicode_width::UnicodeWidthStr;

use super as overview;
use super::OverviewError;

const COLLABORATION: &str = include_str!("testdata/collaboration.mmd");
const LONG_CHAIN: &str = include_str!("testdata/long_workflow.mmd");
const DENSE: &str = include_str!("testdata/dense.mmd");

const NESTED: &str = r#"flowchart TD
  subgraph Outer["外层"]
    A["甲"]
    subgraph Inner["内层"]
      B["乙"]
      C["丙"]
    end
    D["丁"]
  end
  E["戊"]
  A --> B
  A --> B
  A --> C
  B --> C
  C --> B
  C --> C
  A --> A
  B --> D
  D --> E
  E --> A
"#;

fn max_width(lines: &[String]) -> usize {
    lines.iter().map(|line| line.width()).max().unwrap_or(0)
}

fn total_intra(overview: &overview::Overview) -> usize {
    overview
        .groups
        .iter()
        .map(|group| group.intra_edges.len())
        .sum()
}

#[test]
fn collaboration_overview_fits_29x10_and_keeps_all_cross_dependencies() {
    let overview = overview::build(COLLABORATION).expect("collaboration parses");
    assert_eq!(overview.node_count, 36);
    assert_eq!(overview.edge_count, 38);
    assert_eq!(overview.groups.len(), 6);
    assert_eq!(overview.cross_edges.len(), 8, "保留全部跨组依赖");
    assert_eq!(total_intra(&overview), 30, "组内边不能被概览吞掉");

    let lines = overview.lines(29, 10);
    assert!(max_width(&lines) <= 29, "概览超出 29 列: {lines:#?}");
    assert!(lines.len() <= 10, "概览超出 10 行: {lines:#?}");
    let text = lines.join("\n");
    for label in [
        "范围移交",
        "计划下达",
        "依赖确认",
        "提交送审",
        "质量放行",
        "构件清单",
        "运行交接",
        "风险通告",
    ] {
        assert!(text.contains(label), "概览漏掉跨组边 {label}: {text}");
    }
}

#[test]
fn dense_overview_fits_33x21_and_counts_all_edges() {
    let overview = overview::build(DENSE).expect("dense parses");
    assert_eq!(overview.node_count, 144);
    assert_eq!(overview.edge_count, 175);
    assert_eq!(overview.groups.len(), 12);
    assert_eq!(overview.cross_edges.len(), 19);
    assert_eq!(total_intra(&overview), 156, "12 组 × 13 条组内边");

    let lines = overview.lines(33, 21);
    assert!(
        max_width(&lines) <= 33,
        "概览超出 33 列（实际 {}）: {lines:#?}",
        max_width(&lines)
    );
    assert!(lines.len() <= 21, "概览超出 21 行: {lines:#?}");

    // 19 条跨组边按方向、两端和标签逐条核对；概览按原图顺序引用。
    let cross = overview.cross_lines(80);
    assert_eq!(cross.len(), 19);
    for (index, relation) in overview.cross_edges.iter().enumerate() {
        let line = &cross[index];
        assert!(
            line.contains(&relation.to.label),
            "第 {index} 条跨组边缺少目标 {}: {line}",
            relation.to.label
        );
        assert!(
            line.contains(&relation.from.label),
            "第 {index} 条跨组边缺少来源 {}: {line}",
            relation.from.label
        );
        if let Some(label) = relation.label.as_deref() {
            assert!(line.contains(label), "第 {index} 条跨组边缺少标签: {line}");
        }
    }
}

#[test]
fn long_chain_summary_names_real_milestones_and_omits_ranges() {
    let overview = overview::build(LONG_CHAIN).expect("long chain parses");
    assert_eq!(overview.node_count, 72);
    assert_eq!(overview.edge_count, 71);
    assert!(overview.groups.is_empty());

    let lines = overview.lines(22, 21);
    assert!(lines.len() <= 21, "路径摘要超出首屏: {lines:#?}");
    assert!(max_width(&lines) <= 22, "路径摘要过宽: {lines:#?}");
    let text = lines.join("\n");
    assert!(text.contains("发现问题"), "缺少首节点: {text}");
    assert!(text.contains("关闭流程"), "缺少尾节点: {text}");
    assert!(text.contains("省略"), "省略区间必须标出数量: {text}");
    for label in ["接入完成", "定位完成", "发布完成"] {
        assert!(text.contains(label), "缺少真实里程碑边 {label}: {text}");
    }
    // 摘要只引用原图：每个出现的实线箭头两端必须在原始边集合中。
    for relation in overview.summary_edges() {
        assert!(
            overview.edge_exists(&relation.from.id, &relation.to.id),
            "摘要补出了原图不存在的边 {}→{}",
            relation.from.id,
            relation.to.id
        );
    }
}

#[test]
fn nested_groups_ungrouped_self_loops_and_parallel_edges_are_preserved() {
    let overview = overview::build(NESTED).expect("nested source parses");
    assert_eq!(overview.node_count, 5);
    assert_eq!(overview.edge_count, 10);

    let inner = overview
        .groups
        .iter()
        .find(|group| group.id == "Inner")
        .expect("Inner group");
    assert_eq!(inner.depth, 1);
    assert_eq!(inner.parent.as_deref(), Some("Outer"));
    assert_eq!(inner.node_ids, vec!["B", "C"]);
    assert_eq!(inner.intra_edges.len(), 3, "B→C、C→B、C 自环");
    assert!(
        inner
            .intra_edges
            .iter()
            .any(|edge| edge.from.id == "C" && edge.to.id == "C"),
        "自环必须保留在组内边"
    );
    assert!(
        inner
            .intra_edges
            .iter()
            .any(|edge| edge.from.id == "C" && edge.to.id == "B"),
        "反向边必须独立保留"
    );

    let outer = overview
        .groups
        .iter()
        .find(|group| group.id == "Outer")
        .expect("Outer group");
    assert_eq!(outer.node_ids, vec!["A", "D"], "直接成员不含嵌套组节点");
    assert_eq!(outer.child_ids, vec!["Inner"]);
    assert_eq!(outer.intra_edges.len(), 1, "A 自环属于外层");
    assert_eq!(overview.ungrouped.len(), 1);
    assert_eq!(overview.ungrouped[0].id, "E");

    let parallel = overview
        .cross_edges
        .iter()
        .filter(|edge| edge.from.id == "A" && edge.to.id == "B")
        .count();
    assert_eq!(parallel, 2, "重边不能被去重");
    assert_eq!(overview.cross_edges.len(), 6);
    assert_eq!(total_intra(&overview), 4);

    let lines = overview.lines(40, 21);
    let text = lines.join("\n");
    assert!(text.contains("未归组"), "未归组节点必须有位置: {text}");
    assert!(
        text.contains("×2") || text.contains("×3"),
        "重复关系应带数量"
    );
}

#[test]
fn overview_respects_32_40_80_120_columns_and_both_viewport_heights() {
    for (source, natural_rows) in [
        (COLLABORATION, 10usize),
        (DENSE, 21usize),
        (LONG_CHAIN, 21usize),
    ] {
        let overview = overview::build(source).expect("fixture parses");
        for width in [32usize, 40, 80, 120] {
            for viewport_rows in [12usize, 24] {
                let lines = overview.lines(width, viewport_rows);
                assert!(
                    max_width(&lines) <= width,
                    "宽度 {width} / 高度 {viewport_rows} 下溢出: {lines:#?}"
                );
                assert!(
                    lines.len() <= viewport_rows,
                    "宽度 {width} / 高度 {viewport_rows} 下高度预算溢出: {lines:#?}"
                );
            }
            let natural = overview.lines(width, natural_rows);
            assert!(
                max_width(&natural) <= width,
                "宽度 {width} 下溢出: {natural:#?}"
            );
            assert!(
                natural.len() <= natural_rows,
                "宽度 {width} 下自然高度溢出: {natural:#?}"
            );
        }
    }
}

#[test]
fn unsupported_and_invalid_sources_report_a_short_reason() {
    let block = include_str!("testdata/block_nested_anonymous.mmd");
    let error = overview::build(block).expect_err("block has no graph overview");
    assert!(matches!(error, OverviewError::Unsupported(_)));
    assert!(!error.reason().contains('\n'));

    let error = overview::build("flowchart TD\n  A -->").expect_err("incomplete statement");
    assert!(matches!(error, OverviewError::Parse(_)));
    assert!(!error.reason().contains('\n'));
}

#[test]
fn group_detail_lists_every_intra_edge_with_labels() {
    let overview = overview::build(DENSE).expect("dense parses");
    let group = overview
        .groups
        .iter()
        .find(|group| group.id == "G01")
        .expect("G01");
    assert_eq!(group.intra_edges.len(), 13);
    let lines = overview.group_detail_lines("G01", 120);
    let text = lines.join("\n");
    assert!(text.contains("接收记录"), "缺少源节点标签: {text}");
    assert!(text.contains("分派任务"), "缺少目标节点标签: {text}");
    for edge in &group.intra_edges {
        assert!(
            text.contains(&edge.from.label) && text.contains(&edge.to.label),
            "组详情漏掉 {0}→{1}: {text}",
            edge.from.label,
            edge.to.label
        );
    }
    let total: usize = overview
        .groups
        .iter()
        .map(|group| overview.group_detail_lines(&group.id, 120).len())
        .sum::<usize>()
        - overview.groups.len() * 2;
    assert!(total >= 156, "展开全部组应能看到 156 条组内边");
}

#[test]
fn full_render_stays_available_for_the_detail_panel() {
    let overview = overview::build(DENSE).expect("dense parses");
    let compact = overview.lines(40, 21);
    let full =
        crate::kit::mermaid::frame::render_full(DENSE, 120).expect("full layout still renders");
    assert!(
        full.lines().len() > compact.len(),
        "详情必须能回到比概览更完整的图"
    );
    assert!(
        max_width(full.lines()) > 40,
        "原图应保留超出正文列宽的几何供双轴滚动"
    );
}

#[test]
fn branch_summary_uses_real_edges_and_keeps_head_and_tail() {
    let source = r#"flowchart TD
  A["开始"]
  B["分支"]
  C["左"]
  D["右"]
  E["汇合"]
  F["结束"]
  A --> B
  B -->|左路| C
  B -->|右路| D
  C --> E
  D --> E
  E --> F
"#;
    let overview = overview::build(source).expect("branch parses");
    let lines = overview.lines(40, 21);
    let text = lines.join("\n");
    assert!(text.contains("开始") && text.contains("结束"), "{text}");
    assert!(text.contains("分支"), "分支点应在摘要中: {text}");
    assert!(text.contains("右路"), "未选中的真实分支也要列出: {text}");
    for relation in overview.summary_edges() {
        assert!(
            overview.edge_exists(&relation.from.id, &relation.to.id),
            "分支摘要补出了不存在的边 {}→{}",
            relation.from.id,
            relation.to.id
        );
    }
}

#[test]
fn too_many_groups_show_names_and_counts_before_edges() {
    let mut no_edges = String::from("flowchart TD\n");
    for index in 0..16 {
        no_edges.push_str(&format!(
            "  subgraph G{index}[\"组{index}\"]\n    N{index}\n  end\n"
        ));
    }
    let overview = overview::build(&no_edges).expect("groups parse");
    let lines = overview.lines(30, 6);
    assert!(lines.len() <= 6, "{lines:#?}");
    let text = lines.join("\n");
    assert!(text.contains("16 组"), "头部应有组数: {text}");
    assert!(
        text.contains("组0×1") || text.contains("组1×1"),
        "应按组名和数量列出: {text}"
    );

    let mut tall = String::from("flowchart TD\n");
    for index in 0..16 {
        tall.push_str(&format!(
            "  subgraph G{index}[\"组{index}\"]\n    N{index}\n  end\n"
        ));
    }
    for index in 0..15 {
        tall.push_str(&format!("  N{index} --> N{}\n", index + 1));
    }
    let overview = overview::build(&tall).expect("tall cross graph parses");
    let lines = overview.lines(30, 5);
    assert!(lines.len() <= 5, "{lines:#?}");
    let text = lines.join("\n");
    assert!(
        text.contains("另有") || text.contains("跨组"),
        "跨组依赖超高时应先给组名与剩余数量: {text}"
    );
}
