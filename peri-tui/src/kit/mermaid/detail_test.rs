//! 第 4 部分交互回归：详情打开/返回、源码复制与分组展开。

use std::sync::Arc;

use crate::app::panel_types::PanelKind;
use crate::kit::atoms::{ACTIVE_PANEL, FOCUSED_ENTRY, OPEN_PANELS, VIEW_MODELS};
use crate::kit::markdown::{
    FenceClose, FenceKind, MermaidFence, MermaidFenceKey, MermaidMessageIdentity, MermaidMessageRef,
};
use crate::kit::tui_render_unit::{TuiAssistantBubble, TuiRenderUnit};

use super::{
    DetailMode, MERMAID_DETAIL, close_detail_and_restore, copy_text, next_action, open_detail,
    view_lines,
};

const DENSE: &str = include_str!("testdata/dense.mmd");

fn fence(body: &str) -> MermaidFence {
    MermaidFence {
        kind: FenceKind::Backtick,
        marker_len: 3,
        language: "mermaid".to_string(),
        info: "mermaid".to_string(),
        open_start: 0,
        body_start: 10,
        body_end: 10 + body.len(),
        end: 10 + body.len() + 4,
        close: FenceClose::Closed,
        opener_confirmed: true,
        opening_line: "```mermaid".to_string(),
        raw: body.to_string(),
        source: body.to_string(),
    }
}

fn key(name: &str) -> MermaidFenceKey {
    MermaidFenceKey {
        message: MermaidMessageIdentity {
            session_id: "session".to_string(),
            message: MermaidMessageRef::Persistent(name.to_string()),
        },
        open_start: 0,
    }
}

fn bubble(text: &str, hash: u64) -> TuiRenderUnit {
    TuiRenderUnit::TuiAssistantBubble(Arc::new(TuiAssistantBubble {
        started_at: None,
        duration_ms: None,
        text: text.to_string(),
        reasoning: None,
        message_id: Some("m1".to_string()),
        content_hash: hash,
    }))
}

fn install_vm(text: &str) -> u64 {
    crate::kit::atoms::init_atoms();
    let vm = bubble(text, 4711);
    let hash = match &vm {
        TuiRenderUnit::TuiAssistantBubble(bubble) => {
            TuiAssistantBubble::stable_identity_hash(&bubble.text, bubble.reasoning.as_ref())
        }
        _ => vm.content_hash(),
    };
    *VIEW_MODELS.state().write() = crate::kit::atoms::ViewModelsSnapshot {
        generation: 1,
        items: im::Vector::from(vec![vm]),
    };
    *FOCUSED_ENTRY.state().write() = None;
    *MERMAID_DETAIL.state().write() = None;
    *OPEN_PANELS.state().write() = Vec::new();
    *ACTIVE_PANEL.state().write() = None;
    hash
}

#[test]
fn copy_uses_the_original_fence_source() {
    let source = "flowchart TD\n  A[\"甲\"] --> B[\"乙\"]\n";
    let detail = super::MermaidDetail {
        key: key("m1"),
        source: source.to_string(),
        width: 40,
        overview: None,
        overview_error: None,
        slot: None,
        vm_hash: None,
    };
    assert_eq!(copy_text(&detail), source);
}

#[test]
fn tab_selection_cycles_diagrams_in_document_order() {
    assert_eq!(next_action(0, None), None);
    assert_eq!(next_action(3, None), Some(0));
    assert_eq!(next_action(3, Some(0)), Some(1));
    assert_eq!(next_action(3, Some(1)), Some(2));
    assert_eq!(next_action(3, Some(2)), Some(0));
    assert_eq!(next_action(1, Some(0)), Some(0));
}

#[test]
#[serial_test::serial]
fn opening_detail_registers_the_panel_and_escape_restores_the_entry() {
    let text = format!("```mermaid\n{DENSE}\n```");
    let vm_hash = install_vm(&text);
    open_detail(key("m1"), &fence(DENSE), 80, Some(0), Some(vm_hash));
    assert!(
        MERMAID_DETAIL.state().read().is_some(),
        "详情必须保留源码与概览"
    );
    assert_eq!(
        *ACTIVE_PANEL.state().read(),
        Some(PanelKind::MermaidDiagram),
        "图视图必须在 panel_registry 注册并打开"
    );

    close_detail_and_restore();
    assert!(MERMAID_DETAIL.state().read().is_none());
    assert_eq!(*ACTIVE_PANEL.state().read(), None);
    assert_eq!(
        FOCUSED_ENTRY
            .state()
            .read()
            .as_ref()
            .map(|focus| focus.slot),
        Some(0),
        "Esc 应回到原围栏所在的回复"
    );
}

#[test]
#[serial_test::serial]
fn stale_identity_does_not_steal_focus_after_detail_closes() {
    let text = format!("```mermaid\n{DENSE}\n```");
    let vm_hash = install_vm(&text);
    open_detail(key("m1"), &fence(DENSE), 80, Some(0), Some(vm_hash + 1));
    close_detail_and_restore();
    assert!(
        FOCUSED_ENTRY.state().read().is_none(),
        "消息已改写时不能把焦点落回错误 entry"
    );
}

#[test]
fn overview_view_exposes_groups_cross_edges_and_expanded_intra_edges() {
    let detail = super::MermaidDetail {
        key: key("m1"),
        source: DENSE.to_string(),
        width: 80,
        overview: Some(Arc::new(super::super::overview::build(DENSE).unwrap())),
        overview_error: None,
        slot: None,
        vm_hash: None,
    };
    let lines = view_lines(&detail, DetailMode::Overview, 0, &[], None, 80);
    let text = lines.join("\n");
    assert!(text.contains("跨组依赖 19 条"), "跨组依赖计数: {text}");
    assert!(text.contains("需求移交"), "跨组边标签: {text}");
    assert!(text.contains("分组（Tab 切换，Enter 展开）"));
    assert!(text.contains("接入分诊"), "分组名: {text}");

    let expanded = view_lines(
        &detail,
        DetailMode::Overview,
        0,
        &["G01".to_string()],
        None,
        80,
    );
    let expanded_text = expanded.join("\n");
    assert!(
        expanded_text.contains("接收记录"),
        "展开组的原始节点: {expanded_text}"
    );
    for edge in &detail.overview.as_ref().unwrap().groups[0].intra_edges {
        assert!(
            expanded_text.contains(&edge.from.label) && expanded_text.contains(&edge.to.label),
            "展开组必须显示组内边 {0}→{1}",
            edge.from.label,
            edge.to.label
        );
    }
}

#[test]
fn source_view_keeps_every_original_line_and_fallback_reason() {
    let detail = super::MermaidDetail {
        key: key("m1"),
        source: DENSE.to_string(),
        width: 80,
        overview: None,
        overview_error: Some("diagram type has no semantic overview".to_string()),
        slot: None,
        vm_hash: None,
    };
    let lines = view_lines(&detail, DetailMode::Source, 0, &[], None, 80);
    let source_lines: Vec<&str> = DENSE.lines().collect();
    assert_eq!(
        lines[2..].iter().map(String::as_str).collect::<Vec<_>>(),
        source_lines,
        "源码视图必须逐行保留原文"
    );
    let fallback = view_lines(&detail, DetailMode::Overview, 0, &[], None, 80);
    assert!(
        fallback.join("\n").contains("语义概览不可用"),
        "不支持时需给简短原因"
    );
    assert!(
        fallback.join("\n").contains("G01N01"),
        "回退仍需展示完整源码"
    );
}
