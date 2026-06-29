use std::time::Instant;

use peri_acp::event::TodoItemDto;
use peri_acp_types::skill::SkillMetadataDto;

use super::{
    langfuse_state::LangfuseState, AgentComm, CommandSystem, MessageState, SessionMetadata, UiState,
};
use crate::{command::CommandRegistry, thread::ThreadId};

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
    pub commands: CommandSystem,
    pub metadata: SessionMetadata,
    pub agent: AgentComm,
    pub current_thread_id: Option<ThreadId>,
    pub langfuse: LangfuseState,
    pub todo_items: Vec<TodoItemDto>,
    pub background_agents: Vec<RunningBgAgent>,
    pub focused_instance_id: Option<String>,
    pub spinner_state: peri_widgets::SpinnerState,
}

impl ChatSession {
    pub fn new(
        cwd: String,
        command_registry: CommandRegistry,
        skills: Vec<SkillMetadataDto>,
        lc: &crate::i18n::LcRegistry,
        diff_enabled: bool,
        _streaming_mode: Option<String>,
    ) -> Self {
        let commands = CommandSystem::new(command_registry, skills.clone(), lc);
        Self {
            ui: UiState::new(super::build_textarea(false), &cwd, diff_enabled),
            messages: MessageState::new(),
            commands,
            metadata: SessionMetadata::new(),
            agent: AgentComm::default(),
            current_thread_id: None,
            langfuse: LangfuseState::default(),
            todo_items: Vec::new(),
            background_agents: Vec::new(),
            focused_instance_id: None,
            spinner_state: peri_widgets::SpinnerState::new(peri_widgets::SpinnerMode::Idle),
        }
    }
}
