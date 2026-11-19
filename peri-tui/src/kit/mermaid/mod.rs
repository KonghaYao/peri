//! Mermaid 边生成边预览：可信中间帧、闭合定稿与视口调度。
//!
//! 渲染只接收已确认的源码前缀；未确认部分始终以源码尾部显示。排版任务只由
//! 消息区按真实视口相交发起，结果按源码/列宽/引擎参数共享几何。

mod detail;
mod display;
mod frame;
mod layout;
mod overview;
mod preview;
mod store;
mod stream;

pub(crate) use detail::{
    DetailMode, MERMAID_DETAIL, MermaidDetail, close_detail_and_restore, copy_to_clipboard,
    next_action, open_detail, view_lines,
};
pub(crate) use frame::render_full;

use ratatui_kit::prelude::Atom as AtomStatic;

use crate::kit::markdown::MermaidFenceKey;

/// Tab 选中的「查看图」入口；仅用于高亮，不影响图实例身份。
pub(crate) static MERMAID_ACTION_SELECTED: AtomStatic<Option<MermaidFenceKey>> =
    AtomStatic::new(|| None);
/// 选中变化驱动所属消息重新渲染（布局版本之外的最小失效）。
pub(crate) static MERMAID_ACTION_REVISION: AtomStatic<u64> = AtomStatic::new(|| 0);

pub(crate) fn select_action(key: Option<MermaidFenceKey>) {
    *MERMAID_ACTION_SELECTED.state().write() = key;
    MERMAID_ACTION_REVISION.set(MERMAID_ACTION_REVISION.get().wrapping_add(1));
}

#[must_use]
pub(crate) fn action_selected(key: &MermaidFenceKey) -> bool {
    MERMAID_ACTION_SELECTED
        .state()
        .read()
        .as_ref()
        .is_some_and(|selected| selected == key)
}
pub(crate) use display::{
    FenceSpan, PreviewScope, display_lines, display_lines_peek, vm_might_have_mermaid,
};
#[cfg(test)]
pub(crate) use store::{drain_preview_jobs_now, reset_for_test};
pub(crate) use store::{evict_geometry_to, init_preview_worker, retract_unseen};
#[cfg(test)]
pub(crate) use store::{layout_count, retained_geometry_bytes};

/// 消息级预览版本；`None` 表示该 VM 不含 Mermaid 围栏。
#[must_use]
pub(crate) fn message_revision_for_vm(
    vm: &crate::kit::tui_render_unit::TuiRenderUnit,
) -> Option<u64> {
    let crate::kit::tui_render_unit::TuiRenderUnit::TuiAssistantBubble(bubble) = vm else {
        return None;
    };
    if !vm_might_have_mermaid(vm) {
        return None;
    }
    let scope = PreviewScope::for_bubble(
        crate::kit::atoms::ACTIVE_SESSION_ID.state().read().clone(),
        bubble.message_id.as_deref(),
        bubble.started_at,
        bubble.content_hash,
    );
    let store_revision = store::message_revision(&scope.message_identity());
    Some(
        store_revision
            .wrapping_mul(31)
            .wrapping_add(MERMAID_ACTION_REVISION.get()),
    )
}

/// 可见围栏调度：只对与视口相交的围栏发起排版。
pub(crate) fn observe_span(span: &FenceSpan) {
    let _ = store::observe(span.key.clone(), &span.fence, span.width, span.frozen);
}

/// 第 4 部分（概览/详情）的展示模式接口：此处只声明选择口径，避免把
/// 「视口高度只影响展示、不影响几何」的约定散落在调用方。
#[allow(dead_code)] // 第 4 部分接入概览渲染时消费。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DiagramPresentation {
    Inline,
    Overview,
}

#[allow(dead_code)] // 第 4 部分接入概览渲染时消费。
impl DiagramPresentation {
    /// 图行数不超过视口正文行数时内联，否则交给概览。当前消息区只实现内联，
    /// 概览渲染由第 4 部分接入。
    #[must_use]
    pub(crate) fn for_rows(diagram_rows: usize, viewport_rows: usize) -> Self {
        if viewport_rows > 0 && diagram_rows > viewport_rows {
            Self::Overview
        } else {
            Self::Inline
        }
    }
}
