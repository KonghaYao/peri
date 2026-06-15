use std::sync::Arc;

use peri_agent::{
    agent::{events::AgentEvent, react::AgentInput, state::AgentState, ReActAgent, State as _},
    messages::BaseMessage,
    thread::ThreadMeta,
    tools::BaseTool,
};

use super::{build_subagent_middlewares, format_subagent_result, SourceAgentIdHandler};
use crate::{subagent::SubAgentMiddlewareConfig, tools::ArcToolWrapper};

impl super::SubAgentTool {
    pub(crate) async fn invoke_fork(
        &self,
        prompt: &str,
        cwd: &str,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let parent_msgs: Vec<BaseMessage> = match &self.parent_messages {
            Some(pm) => pm.read().clone(),
            None => return Err(
                "Error: Fork path requires parent message history, but parent_messages is not set"
                    .into(),
            ),
        };

        // Create child thread for fork mode
        let child_thread_id = uuid::Uuid::now_v7().to_string();
        if let Some(ref store) = self.thread_store {
            let snapshot_id = parent_msgs.last().map(|m| m.id().as_uuid().to_string());
            let mut child_meta = ThreadMeta::new(cwd);
            child_meta.id = child_thread_id.clone();
            child_meta.parent_thread_id = self.parent_thread_id.clone();
            child_meta.snapshot_at_message_id = snapshot_id;
            child_meta.hidden = true;
            child_meta.cancel_policy = "cascade".parse().expect("合法 cancel_policy 字符串");
            child_meta.title = Some("fork".to_string());
            store
                .create_thread(child_meta)
                .await
                .map_err(|e| format!("Failed to create child thread: {}", e))?;
        }

        let fork_directive = crate::subagent::fork::build_fork_directive(prompt);
        let mut fork_state = if let Some(ref store) = self.thread_store {
            AgentState::new(cwd).with_persistence(Arc::clone(store), child_thread_id.clone())
        } else {
            AgentState::new(cwd)
        };
        // For immediate execution, inject parent messages into state
        for msg in parent_msgs {
            fork_state.add_message(msg);
        }
        let llm = (self.llm_factory)(None);
        let mut agent_builder = ReActAgent::new(llm).max_iterations(200);
        // instance_id 统一使用 child_thread_id（UUID v7，持久化线程标识）
        let instance_id = child_thread_id.clone();

        for mw in build_subagent_middlewares(SubAgentMiddlewareConfig::for_fork(cwd)) {
            agent_builder = agent_builder.add_middleware(mw);
        }

        if let Some(ref builder) = self.system_builder {
            let system_content = builder(None, cwd);
            agent_builder = agent_builder.with_system_prompt(system_content);
        }

        for tool in self.parent_tools.iter() {
            agent_builder = agent_builder
                .register_tool(Box::new(ArcToolWrapper(Arc::clone(tool))) as Box<dyn BaseTool>);
        }

        if let Some(ref factory) = self.child_handler_factory {
            agent_builder = agent_builder.with_event_handler(factory(instance_id.clone()));
        } else if let Some(handler) = &self.event_handler {
            let tagged = Arc::new(SourceAgentIdHandler::new(
                Arc::clone(handler),
                instance_id.clone(),
            ));
            agent_builder = agent_builder.with_event_handler(tagged);
        }

        if let Some(ref handler) = self.event_handler {
            handler.on_event(AgentEvent::SubagentStarted {
                agent_name: "fork".to_string(),
                instance_id: instance_id.clone(),
                is_background: false,
            });
        }
        self.fire_subagent_lifecycle_hook(
            crate::hooks::types::HookEvent::SubagentStart,
            cwd,
            "fork",
            None,
        )
        .await;

        // Register AgentRuntime: only when thread_store is present (non-legacy path)
        // Panic-safe: DeregisterGuard ensures deregister runs on drop (panic or early return)
        //
        // child_cancel is linked to parent via child_token(): parent cancel → child_cancel fires.
        // The same child_cancel is passed to execute(), so cascade cancel works correctly.
        let child_cancel = self
            .cancel
            .as_ref()
            .map(|t| t.child_token())
            .unwrap_or_default();
        let _deregister_guard = if self.thread_store.is_some() {
            if let Some(ref register) = self.register_runtime {
                register(
                    child_thread_id.clone(),
                    child_cancel.clone(),
                    "cascade".to_string(),
                );
            }
            super::define::DeregisterGuard {
                thread_id: child_thread_id.clone(),
                deregister: self.deregister_runtime.clone(),
            }
        } else {
            super::define::DeregisterGuard {
                thread_id: child_thread_id.clone(),
                deregister: None,
            }
        };

        let fork_result = agent_builder
            .execute(
                AgentInput::text(fork_directive),
                &mut fork_state,
                Some(child_cancel),
            )
            .await;

        let (output_summary, stopped_is_error) = match &fork_result {
            Ok(output) => (output.text.chars().take(500).collect::<String>(), false),
            Err(e) => (
                format!("Error: {}", e)
                    .chars()
                    .take(500)
                    .collect::<String>(),
                true,
            ),
        };
        if let Some(ref handler) = self.event_handler {
            handler.on_event(AgentEvent::SubagentStopped {
                agent_name: "fork".to_string(),
                result: output_summary.clone(),
                is_error: stopped_is_error,
                instance_id: instance_id.clone(),
            });
        }
        self.fire_subagent_lifecycle_hook(
            crate::hooks::types::HookEvent::SubagentStop,
            cwd,
            "fork",
            Some(&output_summary),
        )
        .await;

        match fork_result {
            Ok(output) => {
                if let Some(ref store) = self.thread_store {
                    let _ = store.update_thread_status(&child_thread_id, "done").await;
                }
                let result_text = format_subagent_result(&output);
                if self.thread_store.is_some() {
                    Ok(format!(
                        "child_thread_id: {}\n{}",
                        child_thread_id, result_text
                    ))
                } else {
                    Ok(result_text)
                }
            }
            Err(peri_agent::error::AgentError::Interrupted) => {
                if let Some(ref store) = self.thread_store {
                    let _ = store
                        .update_thread_status(&child_thread_id, "cancelled")
                        .await;
                }
                Ok("Fork sub-agent execution was interrupted".to_string())
            }
            Err(e) => {
                if let Some(ref store) = self.thread_store {
                    let _ = store.update_thread_status(&child_thread_id, "error").await;
                }
                let msg = format!("Fork sub-agent execution failed: {}", e);
                Err(msg.into())
            }
        }
    }
}
