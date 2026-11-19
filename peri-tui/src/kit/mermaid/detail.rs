//! 图详情：保存原文、语义概览与分组展开；由 `panel_registry` 打开。
//!
//! 详情只持有围栏原文与派生概览，不缓存消息区布局；Esc 关闭后按实例身份
//! 把 entry 焦点还给原消息。

use std::sync::Arc;

use ratatui_kit::prelude::Atom as AtomStatic;

use crate::app::panel_types::PanelKind;
use crate::kit::atoms::{FOCUSED_ENTRY, VIEW_MODELS};
use crate::kit::markdown::{MermaidFence, MermaidFenceKey};
use crate::kit::tui_render_unit::TuiAssistantBubble;

use super::overview::{self, Overview};

#[derive(Debug, Clone)]
pub(crate) struct MermaidDetail {
    pub(crate) key: MermaidFenceKey,
    pub(crate) source: String,
    pub(crate) width: usize,
    pub(crate) overview: Option<Arc<Overview>>,
    pub(crate) overview_error: Option<String>,
    /// 打开详情的消息 slot 与稳定身份；仅在 Esc 返回时校验。
    pub(crate) slot: Option<usize>,
    pub(crate) vm_hash: Option<u64>,
}

pub(crate) static MERMAID_DETAIL: AtomStatic<Option<MermaidDetail>> = AtomStatic::new(|| None);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DetailMode {
    Overview,
    Full,
    Source,
}

/// 打开一张图的详情。概览解析失败（不支持/语法错误）仍保留源码与简短原因。
pub(crate) fn open_detail(
    key: MermaidFenceKey,
    fence: &MermaidFence,
    width: usize,
    slot: Option<usize>,
    vm_hash: Option<u64>,
) {
    let source = fence.source.clone();
    tracing::debug!(
        target: "mermaid",
        open_start = key.open_start,
        session = %key.message.session_id,
        "opening diagram detail"
    );
    let (overview, overview_error) = match overview::build(&source) {
        Ok(overview) => (Some(Arc::new(overview)), None),
        Err(error) => (None, Some(error.reason().to_string())),
    };
    *MERMAID_DETAIL.state().write() = Some(MermaidDetail {
        key,
        source,
        width,
        overview,
        overview_error,
        slot,
        vm_hash,
    });
    crate::kit::panel_registry::open_panel(PanelKind::MermaidDiagram);
}

/// Esc 返回：关闭面板，并把焦点还给仍指向同一 VM 的原 entry。
pub(crate) fn close_detail_and_restore() {
    let detail = MERMAID_DETAIL.state().read().clone();
    crate::kit::panel_registry::close_panel(PanelKind::MermaidDiagram);
    *MERMAID_DETAIL.state().write() = None;
    let Some(detail) = detail else {
        return;
    };
    tracing::debug!(
        target: "mermaid",
        open_start = detail.key.open_start,
        "closing diagram detail"
    );
    let Some(slot) = detail.slot else {
        return;
    };
    let Some(expected) = detail.vm_hash else {
        return;
    };
    let matches = {
        let handle = VIEW_MODELS.state();
        let snapshot = handle.read();
        snapshot
            .items
            .get(slot)
            .map(identity_hash)
            .is_some_and(|current| current == expected)
    };
    if matches {
        *FOCUSED_ENTRY.state().write() = Some(crate::kit::atoms::FocusedEntry { slot, key: None });
    }
}

/// 复制图源码始终取围栏正文，不经过概览或排版结果。
#[must_use]
pub(crate) fn copy_text(detail: &MermaidDetail) -> String {
    detail.source.clone()
}

pub(crate) fn copy_to_clipboard(detail: &MermaidDetail) {
    let text = copy_text(detail);
    std::thread::spawn(move || {
        if let Err(error) = crate::kit::clipboard::copy_text(&text) {
            tracing::error!(error = ?error, "failed to copy Mermaid source");
        }
    });
}

/// Tab 在一条回复的多张图之间按文档顺序循环。
#[must_use]
pub(crate) fn next_action(count: usize, current: Option<usize>) -> Option<usize> {
    if count == 0 {
        return None;
    }
    match current {
        None => Some(0),
        Some(index) if index + 1 < count => Some(index + 1),
        Some(_) => Some(0),
    }
}

/// 详情面板内容。概览/源码/全图共用同一套行，滚动由面板 ScrollView 负责。
pub(crate) fn view_lines(
    detail: &MermaidDetail,
    mode: DetailMode,
    selected_group: usize,
    expanded: &[String],
    full: Option<&Arc<Vec<String>>>,
    width: usize,
) -> Vec<String> {
    let width = width.max(1);
    match mode {
        DetailMode::Source => {
            let mut lines = vec!["Mermaid 源码（原文，y 复制）".to_string(), String::new()];
            lines.extend(detail.source.lines().map(str::to_string));
            lines
        }
        DetailMode::Full => {
            let mut lines = vec!["Mermaid 全图（←/→ 水平滚动）".to_string(), String::new()];
            match full {
                Some(full) => lines.extend(full.iter().cloned()),
                None => lines.push("按 f 生成全图".to_string()),
            }
            lines
        }
        DetailMode::Overview => {
            let Some(overview) = detail.overview.as_deref() else {
                let mut lines = vec![
                    format!(
                        "语义概览不可用：{}",
                        detail.overview_error.as_deref().unwrap_or("unknown")
                    ),
                    String::new(),
                ];
                lines.extend(detail.source.lines().map(str::to_string));
                return lines;
            };
            let mut lines = vec![overview_header(overview, width)];
            lines.extend(overview.lines(width, 21));
            lines.push(String::new());
            lines.push(format!("跨组依赖 {} 条", overview.cross_edges.len()));
            lines.extend(overview.cross_lines(width));
            if !overview.ungrouped.is_empty() {
                let names: Vec<&str> = overview
                    .ungrouped
                    .iter()
                    .map(|node| node.label.as_str())
                    .collect();
                lines.push(format!(
                    "未归组 {} 节点: {}",
                    overview.ungrouped.len(),
                    names.join(" ")
                ));
            }
            lines.push(String::new());
            lines.push("分组（Tab 切换，Enter 展开）".to_string());
            for (index, group) in overview.groups.iter().enumerate() {
                let marker = if index == selected_group { "▸" } else { " " };
                let open = if expanded.iter().any(|id| id == &group.id) {
                    "▾"
                } else {
                    "▸"
                };
                lines.push(format!(
                    "{marker}{open} {}（{} 节点 · {} 组内边）",
                    group.label,
                    group.node_ids.len(),
                    group.intra_edges.len()
                ));
            }
            for group in &overview.groups {
                if expanded.iter().any(|id| id == &group.id) {
                    lines.push(String::new());
                    lines.extend(overview.group_detail_lines(&group.id, width));
                }
            }
            lines
        }
    }
}

fn overview_header(overview: &Overview, width: usize) -> String {
    let header = format!(
        "Mermaid · {} 节点 · {} 边 · {} 组",
        overview.node_count,
        overview.edge_count,
        overview.groups.len()
    );
    if header.chars().count() <= width {
        header
    } else {
        header
            .chars()
            .take(width.saturating_sub(1))
            .collect::<String>()
            + "…"
    }
}

fn identity_hash(vm: &crate::kit::tui_render_unit::TuiRenderUnit) -> u64 {
    match vm {
        crate::kit::tui_render_unit::TuiRenderUnit::TuiAssistantBubble(bubble) => {
            TuiAssistantBubble::stable_identity_hash(&bubble.text, bubble.reasoning.as_ref())
        }
        _ => vm.content_hash(),
    }
}

#[cfg(test)]
#[path = "detail_test.rs"]
mod tests;
