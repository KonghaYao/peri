use std::time::Instant;

use peri_acp::event::TodoItemDto;

use super::{
    AgentComm, MessageState, SessionMetadata, SubAgentStatusMap, UiState,
    langfuse_state::LangfuseState,
};
use crate::thread::ThreadId;

/// 正在运行的后台 SubAgent
#[derive(Clone, Debug)]
pub struct RunningBgAgent {
    pub agent_name: String,
    pub instance_id: String,
    pub started_at: Instant,
    /// 已执行的工具调用数（由 BgToolStep 事件实时递增）
    pub tool_count: usize,
}

/// 独立聊天会话：封装一个对话的完整 UI 状态、Agent 通信状态和持久化上下文。
pub struct ChatSession {
    pub ui: UiState,
    pub messages: MessageState,
    pub metadata: SessionMetadata,
    pub agent: AgentComm,
    pub current_thread_id: Option<ThreadId>,
    pub langfuse: LangfuseState,
    pub todo_items: Vec<TodoItemDto>,
    pub background_agents: Vec<RunningBgAgent>,
    pub focused_instance_id: Option<String>,
    pub spinner_state: peri_widgets::SpinnerState,
    /// Phase 2.3: SubAgent 运行时状态映射，独立于 v2 ViewCommit 替换语义。
    /// 由 SubAgentStart / SubAgentEnd / BackgroundTaskCompleted / BgToolStep
    /// 事件实时维护；渲染时通过 `lookup(instance_id)` 覆盖 DTO 的静态字段。
    pub subagent_status: SubAgentStatusMap,
}

impl ChatSession {
    pub fn new(cwd: String, diff_enabled: bool, _streaming_mode: Option<String>) -> Self {
        Self {
            ui: UiState::new(super::build_textarea(false), &cwd, diff_enabled),
            messages: MessageState::new(),
            metadata: SessionMetadata::new(),
            agent: AgentComm::default(),
            current_thread_id: None,
            langfuse: LangfuseState::default(),
            todo_items: Vec::new(),
            background_agents: Vec::new(),
            focused_instance_id: None,
            spinner_state: peri_widgets::SpinnerState::new(peri_widgets::SpinnerMode::Idle),
            subagent_status: SubAgentStatusMap::new(),
        }
    }
}
