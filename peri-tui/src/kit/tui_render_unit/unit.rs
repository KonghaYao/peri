use super::{
    TuiAskUserBlock, TuiAssistantBubble, TuiCollapsedGroup, TuiDivider, TuiSubAgentGroup,
    TuiSystemNote, TuiSystemReminder, TuiTodoSummary, TuiToolCard, TuiUserBubble,
};
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Top-level enum
// ---------------------------------------------------------------------------

/// Discriminated-union TuiRenderUnit consumed by the TUI renderer.
#[derive(Debug, Clone, PartialEq)]
pub enum TuiRenderUnit {
    TuiUserBubble(TuiUserBubble),
    TuiAssistantBubble(Arc<TuiAssistantBubble>),
    TuiToolCard(TuiToolCard),
    TuiSystemNote(TuiSystemNote),
    TuiSystemReminder(TuiSystemReminder),
    TuiSubAgentGroup(TuiSubAgentGroup),
    /// 正文空档折叠组（正文段落之间的工具 / thinking / 通知等）。
    TuiCollapsedGroup(TuiCollapsedGroup),
    TuiDivider(TuiDivider),
    TuiAskUserBlock(TuiAskUserBlock),
    /// §6.9 活动 turn 的 todo 进度摘要行（`3/7 tasks · Running tests`），
    /// 由 push_view_models 从 `TODO_ITEMS` 派生，插在最终回答之前。
    TuiTodoSummary(TuiTodoSummary),
}

#[cfg(test)]
#[path = "shared_bubble_test.rs"]
mod shared_bubble_tests;

/// [test] 递归展平正文空档折叠组——测试按真实单元查找工具 / thinking / 通知。
#[cfg(test)]
pub(crate) fn flatten_units<'a>(
    items: impl IntoIterator<Item = &'a TuiRenderUnit>,
) -> Vec<&'a TuiRenderUnit> {
    let mut out = Vec::new();
    for vm in items {
        match vm {
            TuiRenderUnit::TuiCollapsedGroup(group) => {
                out.extend(flatten_units(group.view_models.iter()));
            }
            other => out.push(other),
        }
    }
    out
}

impl TuiRenderUnit {
    /// 返回该 VM 内部存储的 content_hash。
    /// 供按 VM 分片的渲染缓存作为 key 使用——hash 不变时直接 Arc::clone 复用渲染结果。
    #[must_use]
    pub fn content_hash(&self) -> u64 {
        match self {
            Self::TuiUserBubble(d) => d.content_hash,
            Self::TuiAssistantBubble(d) => d.content_hash,
            Self::TuiToolCard(d) => d.content_hash,
            Self::TuiSystemNote(d) => d.content_hash,
            Self::TuiSystemReminder(d) => d.content_hash,
            Self::TuiSubAgentGroup(d) => d.content_hash,
            Self::TuiCollapsedGroup(d) => d.content_hash,
            Self::TuiDivider(d) => d.content_hash,
            Self::TuiAskUserBlock(d) => d.content_hash,
            Self::TuiTodoSummary(d) => d.content_hash,
        }
    }

    /// 该 VM 是否需要运行中刷新：工具与子 agent 按 braille 帧刷新，
    /// reasoning 仅刷新秒级时长；具体节拍由 animation_period_frames 决定。
    #[must_use]
    pub fn is_animating(&self) -> bool {
        self.animation_period_frames() != 0
    }

    pub(crate) fn animation_period_frames(&self) -> u64 {
        match self {
            Self::TuiToolCard(data) if data.is_running => 1,
            Self::TuiSubAgentGroup(data) if data.is_running => 1,
            // 尾部工作组：组行的 Working 扫光按帧推进（phase = frame × 100ms）。
            Self::TuiCollapsedGroup(data) if data.is_working => 1,
            // 组内任一成员运行中且组已展开 → 按 1 帧刷新（成员 spinner / elapsed
            // 可见）；组行本身静态，折叠时无需逐帧重渲染。
            Self::TuiCollapsedGroup(data)
                if data.fold != super::fold::FoldState::Collapsed
                    && data.view_models.iter().any(TuiRenderUnit::is_animating) =>
            {
                1
            }
            Self::TuiAssistantBubble(data)
                if data
                    .reasoning
                    .as_ref()
                    .is_some_and(|reasoning| reasoning.is_running) =>
            {
                10
            }
            _ => 0,
        }
    }
}
