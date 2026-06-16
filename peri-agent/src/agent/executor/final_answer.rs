use super::ReActAgent;
use crate::{
    agent::{
        events::AgentEvent,
        react::{AgentOutput, ReactLLM, Reasoning, ToolCall, ToolResult},
        state::State,
    },
    error::AgentResult,
    messages::{message::MessageId, BaseMessage},
};

/// 在消息列表中查找给定 ID 消息的索引，返回其后一个位置（即下一条新消息的起始位置）。
/// 如果未找到（不应发生），fallback 到 0。
pub(super) fn index_after_id(messages: &[BaseMessage], anchor: MessageId) -> usize {
    messages
        .iter()
        .position(|m| m.id() == anchor)
        .map(|i| i + 1)
        .unwrap_or(0)
}

/// 消费后台任务完成通知，注入到 state 中供 LLM 下一轮迭代可见。
///
/// 通知通过 StateSnapshot 写入 agent_state_messages（路径 A）。
/// TUI 侧 handle_background_task_completed（路径 B）在 executor 运行期间
/// 不再直接 push，仅在 executor 已结束时作为兜底写入。
async fn drain_notifications<L: ReactLLM, S: State>(agent: &ReActAgent<L, S>, state: &mut S) {
    if let Some(ref rx) = agent.notification_rx {
        let mut rx_lock = rx.lock().await;
        while let Ok(result) = rx_lock.try_recv() {
            let msg = BaseMessage::human(result.to_notification());
            state.add_message(msg);
        }
    }
}

/// 工具调用步骤后：发出 StateSnapshot + 消费后台通知 + 更新快照锚点
pub(crate) async fn emit_snapshot_and_drain_notifications<L: ReactLLM, S: State>(
    agent: &ReActAgent<L, S>,
    state: &mut S,
    snapshot_anchor: &mut MessageId,
) {
    // 使用 MessageId 锚点截取本轮新增消息，不受 prepend_message 的 insert(0) 影响。
    // .filter(!is_system()) 保留作为防御层。
    let start = index_after_id(state.messages(), *snapshot_anchor);
    let msgs_since_human: Vec<BaseMessage> = state.messages()[start..]
        .iter()
        .filter(|m| !m.is_system())
        .cloned()
        .collect();
    if !msgs_since_human.is_empty() {
        agent.emit(AgentEvent::StateSnapshot(msgs_since_human));
    }

    drain_notifications(agent, state).await;

    *snapshot_anchor = state.messages().last().expect("messages non-empty").id();
}

/// 处理最终回答路径，返回 AgentOutput
pub(crate) async fn handle_final_answer<L: ReactLLM, S: State>(
    agent: &ReActAgent<L, S>,
    state: &mut S,
    reasoning: &Reasoning,
    all_tool_calls: Vec<(ToolCall, ToolResult)>,
    snapshot_anchor: &mut MessageId,
    step: usize,
) -> AgentResult<AgentOutput> {
    let answer = reasoning
        .final_answer
        .clone()
        .unwrap_or_else(|| reasoning.thought.clone());

    if answer.trim().is_empty() {
        tracing::warn!(
            step,
            "LLM 返回空最终回答（无 tool_calls 且 final_answer/thought 为空）"
        );
    }

    // 优先使用带 Reasoning block 的原始消息，保留 thinking 内容
    let ai_msg = reasoning
        .source_message
        .clone()
        .unwrap_or_else(|| BaseMessage::ai(answer.as_str()));
    let ai_msg_id = ai_msg.id(); // 捕获 message_id（Copy，供 TextChunk 使用）
    let ai_msg_clone = ai_msg.clone();
    state.add_message(ai_msg);
    agent.emit(AgentEvent::MessageAdded(ai_msg_clone));

    if !reasoning.streamed {
        agent.emit(AgentEvent::TextChunk {
            message_id: ai_msg_id,
            chunk: answer.clone(),
            source_agent_id: None,
        });
    }

    let start = index_after_id(state.messages(), *snapshot_anchor);
    let msgs_since_last: Vec<BaseMessage> = state.messages()[start..]
        .iter()
        .filter(|m| !m.is_system())
        .cloned()
        .collect();
    if !msgs_since_last.is_empty() {
        agent.emit(AgentEvent::StateSnapshot(msgs_since_last));
        *snapshot_anchor = state.messages().last().expect("messages non-empty").id();
    }

    drain_notifications(agent, state).await;

    let start = index_after_id(state.messages(), *snapshot_anchor);
    let msgs_after_drain: Vec<BaseMessage> = state.messages()[start..]
        .iter()
        .filter(|m| !m.is_system())
        .cloned()
        .collect();
    if !msgs_after_drain.is_empty() {
        agent.emit(AgentEvent::StateSnapshot(msgs_after_drain));
        *snapshot_anchor = state.messages().last().expect("messages non-empty").id();
    }

    let output = AgentOutput {
        text: answer,
        steps: step + 1,
        tool_calls: all_tool_calls,
        stop_reason: None,
        block_continue: None,
    };

    tracing::info!(
        steps = output.steps,
        tool_calls = output.tool_calls.len(),
        "agent finished"
    );

    {
        let sid = state.get_context("session_id").map(|s| s.to_string());
        let rid = state.get_context("run_id").map(|s| s.to_string());
        let rss_mb = crate::metrics::current_rss_mb();

        // threshold.memory：每个阈值每轮只报一次
        if let Some(rss) = rss_mb {
            let reported_100 = state.get_context("mem_reported_100").is_some();
            let reported_200 = state.get_context("mem_reported_200").is_some();
            if rss >= 100 && !reported_100 {
                crate::metrics::emit(
                    "threshold.memory",
                    serde_json::json!({"rss_mb": rss, "level": 100}),
                    sid.as_deref(),
                    rid.as_deref(),
                );
                state.set_context("mem_reported_100", "1");
            }
            if rss >= 200 && !reported_200 {
                crate::metrics::emit(
                    "threshold.memory",
                    serde_json::json!({"rss_mb": rss, "level": 200}),
                    sid.as_deref(),
                    rid.as_deref(),
                );
                state.set_context("mem_reported_200", "1");
            }
        }

        // sample.agent_turn_end
        crate::metrics::emit(
            "sample.agent_turn_end",
            serde_json::json!({
                "rss_mb": rss_mb,
                "iterations": state.current_step(),
                "total_input_tokens": state.token_tracker().total_input_tokens,
                "total_output_tokens": state.token_tracker().total_output_tokens,
                "duration_secs": 0u64,
            }),
            sid.as_deref(),
            rid.as_deref(),
        );
    }
    match agent.chain.run_after_agent(state, output).await {
        Ok(o) => {
            let start = index_after_id(state.messages(), *snapshot_anchor);
            let msgs_after: Vec<BaseMessage> = state.messages()[start..]
                .iter()
                .filter(|m| !m.is_system())
                .cloned()
                .collect();
            if !msgs_after.is_empty() {
                agent.emit(AgentEvent::StateSnapshot(msgs_after));
            }
            Ok(o)
        }
        Err(e) => {
            agent.chain.run_on_error(state, &e).await?;
            Err(e)
        }
    }
}
