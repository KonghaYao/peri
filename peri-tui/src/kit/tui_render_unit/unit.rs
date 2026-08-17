use super::{
    TuiAskUserBlock, TuiAssistantBubble, TuiCollapsedGroup, TuiDivider, TuiSubAgentGroup,
    TuiSystemNote, TuiTodoSummary, TuiToolCard, TuiUserBubble,
};

// ---------------------------------------------------------------------------
// Top-level enum
// ---------------------------------------------------------------------------

/// Discriminated-union TuiRenderUnit consumed by the TUI renderer.
#[derive(Debug, Clone, PartialEq)]
pub enum TuiRenderUnit {
    TuiUserBubble(TuiUserBubble),
    TuiAssistantBubble(TuiAssistantBubble),
    TuiToolCard(TuiToolCard),
    TuiSystemNote(TuiSystemNote),
    TuiSubAgentGroup(TuiSubAgentGroup),
    TuiCollapsedGroup(TuiCollapsedGroup),
    TuiDivider(TuiDivider),
    TuiAskUserBlock(TuiAskUserBlock),
    /// §6.9 活动 turn 的 todo 进度摘要行（`3/7 tasks · Running tests`），
    /// 由 push_view_models 从 `TODO_ITEMS` 派生，插在最终回答之前。
    TuiTodoSummary(TuiTodoSummary),
}

impl TuiRenderUnit {
    /// 返回该 VM 内部存储的 content_hash。
    /// 供按 VM 分片的渲染缓存作为 key 使用——hash 不变时直接 Arc::clone 复用渲染结果。
    pub fn content_hash(&self) -> u64 {
        match self {
            Self::TuiUserBubble(d) => d.content_hash,
            Self::TuiAssistantBubble(d) => d.content_hash,
            Self::TuiToolCard(d) => d.content_hash,
            Self::TuiSystemNote(d) => d.content_hash,
            Self::TuiSubAgentGroup(d) => d.content_hash,
            Self::TuiCollapsedGroup(d) => d.content_hash,
            Self::TuiDivider(d) => d.content_hash,
            Self::TuiAskUserBlock(d) => d.content_hash,
            Self::TuiTodoSummary(d) => d.content_hash,
        }
    }

    /// 该 VM 是否渲染运行中动画符号（tool running / subagent running /
    /// reasoning running，§8.2）——渲染缓存需按动画帧强制重建，使 braille
    /// 动画随壁钟 tick 推进（hash 可能跨秒才变化，不足以驱动 10Hz 动画）。
    pub fn is_animating(&self) -> bool {
        match self {
            Self::TuiToolCard(d) => d.is_running,
            Self::TuiSubAgentGroup(d) => d.is_running,
            Self::TuiAssistantBubble(d) => d.reasoning.as_ref().is_some_and(|r| r.is_running),
            _ => false,
        }
    }
}
