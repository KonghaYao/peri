use super::{
    TuiAskUserBlock, TuiAssistantBubble, TuiCollapsedGroup, TuiDivider, TuiSubAgentGroup,
    TuiSystemNote, TuiSystemReminder, TuiTodoSummary, TuiToolCard, TuiUserBubble,
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
    TuiSystemReminder(TuiSystemReminder),
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
    pub fn is_animating(&self) -> bool {
        self.animation_period_frames() != 0
    }

    pub(crate) fn animation_period_frames(&self) -> u64 {
        match self {
            Self::TuiToolCard(data) if data.is_running => 1,
            Self::TuiSubAgentGroup(data) if data.is_running => 1,
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
