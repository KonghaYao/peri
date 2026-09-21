use peri_acp_types::system_reminder::{SystemReminder, TrustedSystemReminderFactory};
use peri_agent::{
    agent::react::AgentOutput,
    error::{AgentError, AgentResult},
    middleware::capabilities::AfterAgentState,
    session::{MessageKind, MessageSource, QueuedMessage},
};

// 准入必须先于业务计数或模板构造；持有同一状态能力，统一 canonical 入队与续跑。
// 后台活动复用 Receive 等待语义（包含 Shell），不把 UI 计数或执行 scope 当作事实源。
pub(crate) struct CompletionReminder<'a> {
    state: &'a dyn AfterAgentState,
    output: &'a AgentOutput,
}

impl<'a> CompletionReminder<'a> {
    pub(crate) fn admit(state: &'a dyn AfterAgentState, output: &'a AgentOutput) -> Option<Self> {
        if output.block_continue.is_some() || state.has_active_background_tasks() {
            return None;
        }
        Some(Self { state, output })
    }

    pub(crate) fn enqueue(
        self,
        middleware: &str,
        reminder: SystemReminder,
        source: MessageSource,
        block_reason: &str,
    ) -> AgentResult<AgentOutput> {
        let reminder = TrustedSystemReminderFactory::for_producer()
            .construct(reminder)
            .map_err(|error| AgentError::MiddlewareError {
                middleware: middleware.to_string(),
                reason: error.to_string(),
            })?;
        self.state
            .enqueue_v2_message(QueuedMessage::system_reminder(
                MessageKind::Defer,
                source,
                reminder,
            ));
        let mut output = self.output.clone();
        output.block_continue = Some(block_reason.to_string());
        Ok(output)
    }
}
