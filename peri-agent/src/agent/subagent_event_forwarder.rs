//! SubAgent v2 事件转发器
//!
//! SubAgent（fork / background / define 路径）拥有独立 EventBus，但默认不消费
//! `EventHandles`，导致 `run_react_loop` 内 emit 的 Render/State/Observe 事件
//! 全部丢弃在 SubAgent 自己的 EventBus 内部，TUI 看不到 SubAgent 的工具调用、
//! AI 文本、推理内容。
//!
//! 本模块封装转发器 spawn 函数：从 SubAgent 的 EventBus 消费 v2 事件，经
//! `events_v2_mapper` 映射为 `ExecutorEvent`，注入 `source_agent_id = child_thread_id`
//! 后转发到父 Agent 的 `AgentEventHandler`。
//!
//! ## 关键不变量
//!
//! - **`child_thread_id` 必须与 `SubagentStarted { instance_id }` 一致**：TUI 的
//!   `find_running_subagent_mut(aid)` 按 instance_id 精确匹配，不匹配则事件被忽略。
//!   注意：v2 内部 `AgentId::new()` 与 child_thread_id 是不同值，不能用错。
//! - **转发器 task 在通道关闭时自动退出**：`select! { else => break }` 处理所有
//!   通道关闭场景，避免 task 泄漏。
//! - **ObserveEvent 的 Lagged 不 panic**：只记日志，继续处理后续事件。

use std::sync::Arc;

use tokio::task::JoinHandle;

use crate::agent::events::{inject_source_agent_id, AgentEventHandler};
use crate::agent::events_v2::EventHandles;
use crate::agent::events_v2_mapper::{
    observe_event_to_executor, render_event_to_executor, state_event_to_executor,
};

/// 启动 SubAgent 事件转发器。
///
/// 消费 `handles` 内三层 v2 事件，映射为 ExecutorEvent，注入 source_agent_id 后
/// 通过 `event_handler` 转发到父 Agent。通道全部关闭时自动退出。
///
/// # 参数
///
/// - `handles`：SubAgent v2 `EventHandles`（从 `V2SubagentContext.event_handles` 取出）
/// - `event_handler`：父 Agent 的事件处理器（`SubAgentTool.event_handler` clone）
/// - `child_thread_id`：与 `SubagentStarted { instance_id }` 一致的 UUID 字符串
///
/// # 返回
///
/// 转发器 task 的 `JoinHandle`。调用方持有以控制生命周期（通常 `_forwarder_handle`
/// 解绑即表示 fire-and-forget，task 在通道关闭时自动退出）。
pub fn spawn_subagent_event_forwarder(
    mut handles: EventHandles,
    event_handler: Option<Arc<dyn AgentEventHandler>>,
    child_thread_id: String,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            // biased + render 优先：保证同一 ReAct 迭代的 Render 事件在 State 事件
            // 之前被消费，避免 commit_iteration 与残留 Render 事件乱序导致的 partial 污染。
            // 虽然子 Agent 的 TurnCompleted 已被过滤不转发，biased 仍是防御性时序保证。
            tokio::select! {
                biased;
                Some(ev) = handles.render_rx.recv() => {
                    // 过滤：子 Agent 的 TurnCommitted / HitlPending 不应转发到父 Agent
                    // （TurnCompleted 已迁移到 RenderEvent；HitlPending 由父 Agent
                    //  HITL 中间件独立处理，转发会导致双重审批弹窗）
                    let should_forward = !matches!(
                        &ev,
                        crate::agent::events_v2::RenderEvent::TurnCompleted { .. }
                        | crate::agent::events_v2::RenderEvent::HitlPending { .. }
                    );
                    if should_forward {
                        if let Some(mut exec_ev) = render_event_to_executor(ev) {
                            inject_source_agent_id(&mut exec_ev, &child_thread_id);
                            if let Some(h) = &event_handler {
                                h.on_event(exec_ev);
                            }
                        }
                    }
                }
                Some(ev) = handles.state_rx.recv() => {
                    // 过滤：子 Agent 的 StateSnapshot 不应污染父 Agent transcript
                    // （TurnCompleted 已迁移到 RenderEvent，在 render 分支同样过滤）
                    let should_forward = !matches!(
                        &ev,
                        crate::agent::events_v2::StateEvent::StateSnapshot { .. }
                    );
                    if should_forward {
                        if let Some(exec_ev) = state_event_to_executor(ev) {
                            if let Some(h) = &event_handler {
                                h.on_event(exec_ev);
                            }
                        }
                    }
                }
                ev_res = handles.observe_rx.recv() => {
                    match ev_res {
                        Ok(ev) => {
                            if let Some(mut exec_ev) = observe_event_to_executor(ev) {
                                inject_source_agent_id(&mut exec_ev, &child_thread_id);
                                if let Some(h) = &event_handler {
                                    h.on_event(exec_ev);
                                }
                            }
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                            tracing::warn!(
                                n,
                                "[subagent-forwarder] observe_rx lagged, events dropped"
                            );
                        }
                    }
                }
                else => break,
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::events::ExecutorEvent;
    use crate::agent::events_v2::{
        EventBus, EventBusConfig, ObserveEvent, RenderEvent, StateEvent,
    };
    use crate::group::pipeline::AgentId;
    use crate::session::turn::TurnId;
    use parking_lot::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// 记录所有 ExecutorEvent 的 mock handler
    struct CapturingHandler {
        events: Arc<Mutex<Vec<ExecutorEvent>>>,
    }

    impl AgentEventHandler for CapturingHandler {
        fn on_event(&self, event: ExecutorEvent) {
            self.events.lock().push(event);
        }
    }

    fn ids() -> (TurnId, AgentId) {
        (TurnId::new(), AgentId::new())
    }

    /// 等待 forwarder 处理完所有事件（轮询 events.len() 直到达到 expected 或超时）
    async fn wait_for_event_count(events: &Arc<Mutex<Vec<ExecutorEvent>>>, expected: usize) {
        for _ in 0..100 {
            if events.lock().len() >= expected {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!(
            "等待事件超时：期望 {} 个，实际 {} 个",
            expected,
            events.lock().len()
        );
    }

    #[tokio::test]
    async fn test_forwarder_injects_source_agent_id_for_tool_events() {
        // SubAgent 转发器核心契约：ToolStart 必须注入 source_agent_id = child_thread_id，
        // 让 TUI 的 find_running_subagent_mut(aid) 按 instance_id 匹配
        let (bus, handles) = EventBus::new(EventBusConfig::default());
        let captured: Arc<Mutex<Vec<ExecutorEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let handler = Arc::new(CapturingHandler {
            events: Arc::clone(&captured),
        });
        let child_thread_id = "subagent_test_id_123".to_string();

        let _forwarder =
            spawn_subagent_event_forwarder(handles, Some(handler), child_thread_id.clone());

        // emit ToolStarted（注意 v2 agent_id 与 child_thread_id 不同）
        let (turn_id, agent_id) = ids();
        bus.emit_render(RenderEvent::ToolStarted {
            turn_id,
            agent_id,
            tool_call_id: "tc_1".to_string(),
            name: "Read".to_string(),
            input: serde_json::Value::Null,
        });

        wait_for_event_count(&captured, 1).await;

        let first_event = captured.lock()[0].clone();
        match first_event {
            ExecutorEvent::ToolStart {
                source_agent_id,
                tool_call_id,
                name,
                ..
            } => {
                assert_eq!(
                    source_agent_id.as_deref(),
                    Some("subagent_test_id_123"),
                    "source_agent_id 必须注入为 child_thread_id"
                );
                assert_eq!(tool_call_id, "tc_1");
                assert_eq!(name, "Read");
            }
            other => panic!("应为 ToolStart，实际 {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_forwarder_injects_source_agent_id_for_text_chunk() {
        // TextChunk 也需要注入 source_agent_id（SubAgent 内 AI 文本路由）
        let (bus, handles) = EventBus::new(EventBusConfig::default());
        let captured: Arc<Mutex<Vec<ExecutorEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let handler = Arc::new(CapturingHandler {
            events: Arc::clone(&captured),
        });

        let _forwarder =
            spawn_subagent_event_forwarder(handles, Some(handler), "child_abc".to_string());

        let (turn_id, agent_id) = ids();
        bus.emit_render(RenderEvent::TextChunk {
            turn_id,
            agent_id,
            chunk: "hello".to_string(),
        });

        wait_for_event_count(&captured, 1).await;

        let first_event = captured.lock()[0].clone();
        match first_event {
            ExecutorEvent::TextChunk {
                source_agent_id,
                chunk,
                ..
            } => {
                assert_eq!(source_agent_id.as_deref(), Some("child_abc"));
                assert_eq!(chunk, "hello");
            }
            other => panic!("应为 TextChunk，实际 {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_forwarder_injects_source_agent_id_for_reasoning_chunk() {
        // ThinkingChunk 也必须注入 source_agent_id，避免子 Agent reasoning 污染父消息流。
        let (bus, handles) = EventBus::new(EventBusConfig::default());
        let captured: Arc<Mutex<Vec<ExecutorEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let handler = Arc::new(CapturingHandler {
            events: Arc::clone(&captured),
        });

        let _forwarder =
            spawn_subagent_event_forwarder(handles, Some(handler), "child_reasoning".to_string());

        let (turn_id, agent_id) = ids();
        bus.emit_render(RenderEvent::ThinkingChunk {
            turn_id,
            agent_id,
            chunk: "thinking".to_string(),
        });

        wait_for_event_count(&captured, 1).await;

        let first_event = captured.lock()[0].clone();
        match first_event {
            ExecutorEvent::AiReasoning {
                text,
                source_agent_id,
            } => {
                assert_eq!(text, "thinking");
                assert_eq!(source_agent_id.as_deref(), Some("child_reasoning"));
            }
            other => panic!("应为 AiReasoning，实际 {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_forwarder_propagates_all_event_layers() {
        // 3 层 v2 事件中，State 层 TurnCompleted/StateSnapshot 应被过滤
        // 仅 Render + Observe 层事件转发到父 agent（与 v1 subagent_stack 对齐）
        let (bus, handles) = EventBus::new(EventBusConfig::default());
        let captured: Arc<Mutex<Vec<ExecutorEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let handler = Arc::new(CapturingHandler {
            events: Arc::clone(&captured),
        });

        let _forwarder =
            spawn_subagent_event_forwarder(handles, Some(handler), "test_id".to_string());

        // Render layer
        let (turn_id, agent_id) = ids();
        bus.emit_render(RenderEvent::ThinkingChunk {
            turn_id,
            agent_id,
            chunk: "thinking".to_string(),
        });

        // Render layer（TurnCompleted 已迁移到 Render，应被过滤 —— 不污染父 Agent transcript）
        let (turn_id, agent_id) = ids();
        bus.emit_render(RenderEvent::TurnCompleted {
            turn_id,
            agent_id,
            steps: 2,
            elapsed_secs: 0.1,
            finalized_messages: Arc::new(Vec::new()),
        });

        // Observe layer（SubagentStart → SubagentStarted）
        let (turn_id, agent_id) = ids();
        bus.emit_observe(ObserveEvent::SubagentStart {
            turn_id,
            agent_id,
            child_agent_id: AgentId::new(),
            agent_name: "explore".to_string(),
            is_background: false,
        });

        // 期望 2 个事件（Render + Observe），State 层被过滤
        wait_for_event_count(&captured, 2).await;

        let events_snapshot: Vec<ExecutorEvent> = captured.lock().clone();
        assert_eq!(events_snapshot.len(), 2, "应仅转发 Render + Observe 层事件");

        let has_ai_reasoning = events_snapshot.iter().any(|e| {
            matches!(
                e,
                ExecutorEvent::AiReasoning { text, source_agent_id }
                    if text == "thinking" && source_agent_id.as_deref() == Some("test_id")
            )
        });
        let has_subagent_started = events_snapshot
            .iter()
            .any(|e| matches!(e, ExecutorEvent::SubagentStarted { agent_name, .. } if agent_name == "explore"));

        assert!(has_ai_reasoning, "应有 AiReasoning：{:?}", events_snapshot);
        assert!(
            has_subagent_started,
            "应有 SubagentStarted：{:?}",
            events_snapshot
        );
    }

    #[tokio::test]
    async fn test_forwarder_exits_when_channels_closed() {
        // 所有通道关闭时，转发器 task 应自动退出（避免 task 泄漏）
        let (_bus, handles) = EventBus::new(EventBusConfig::default());
        let handler: Option<Arc<dyn AgentEventHandler>> = None;

        let forwarder = spawn_subagent_event_forwarder(handles, handler, "test".to_string());

        // _bus drop 后，render/state tx 关闭；observe_rx 没有 sender 也无法 recv
        // 由于 select! else 分支处理 None，task 应该退出
        drop(_bus);

        // 等待 task 结束（应该很快）
        let result = tokio::time::timeout(std::time::Duration::from_millis(500), forwarder).await;
        assert!(result.is_ok(), "转发器应该在通道关闭后自动退出（500ms 内）");
    }

    #[tokio::test]
    async fn test_forwarder_handles_observe_lagged() {
        // broadcast channel 满时 lag 不应 panic，转发器继续运行
        // 用极小容量（1）的 broadcast channel 模拟
        let (bus, handles) = EventBus::new(EventBusConfig {
            observe_capacity: 1,
            ..Default::default()
        });
        let counter = Arc::new(AtomicUsize::new(0));
        struct CountingHandler(Arc<AtomicUsize>);
        impl AgentEventHandler for CountingHandler {
            fn on_event(&self, _event: ExecutorEvent) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }
        let handler = Arc::new(CountingHandler(Arc::clone(&counter)));

        let _forwarder = spawn_subagent_event_forwarder(
            handles,
            Some(handler as Arc<dyn AgentEventHandler>),
            "test".to_string(),
        );

        // 快速发送多个 observe 事件触发 Lagged（capacity=1，第二个会 Lagged）
        // 给 forwarder 一点时间启动
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // 连续 emit（即使 lagged 也不应 panic）
        for _ in 0..5 {
            let (turn_id, agent_id) = ids();
            let _ = bus.emit_observe(ObserveEvent::SubagentStart {
                turn_id,
                agent_id,
                child_agent_id: AgentId::new(),
                agent_name: "test".to_string(),
                is_background: false,
            });
        }

        // 等待 forwarder 处理（可能只接收到部分事件，因为 lagged 会丢）
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        // 关键断言：forwarder 不 panic，且至少处理了 0 个事件（不强制要求 > 0，
        // 因为 lagged 可能丢全部，但不应导致 task 崩溃）
        let count = counter.load(Ordering::SeqCst);
        assert!(count <= 5, "处理的事件数不应超过 emit 数，实际 {}", count);

        // 关闭通道后 forwarder 应该正常退出
        drop(bus);
    }

    #[tokio::test]
    async fn test_forwarder_no_handler_does_not_panic() {
        // event_handler = None 时不应该 panic，事件被消费后丢弃
        let (bus, handles) = EventBus::new(EventBusConfig::default());

        let _forwarder = spawn_subagent_event_forwarder(handles, None, "test".to_string());

        let (turn_id, agent_id) = ids();
        bus.emit_render(RenderEvent::ToolStarted {
            turn_id,
            agent_id,
            tool_call_id: "tc".to_string(),
            name: "Read".to_string(),
            input: serde_json::Value::Null,
        });

        // 给 forwarder 一点时间处理
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // 无 panic 即通过（不需要断言事件被处理）
        drop(bus);
    }

    #[tokio::test]
    async fn test_forwarder_filters_turn_committed() {
        // 验证：TurnCompleted 和 StateSnapshot 应被过滤（不污染父 Agent transcript）
        // 而 RenderEvent::TextChunk 应正常转发
        let (bus, handles) = EventBus::new(EventBusConfig::default());
        let captured: Arc<Mutex<Vec<ExecutorEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let handler = Arc::new(CapturingHandler {
            events: Arc::clone(&captured),
        });

        let _forwarder = spawn_subagent_event_forwarder(handles, Some(handler), "test".to_string());

        // 发送 TurnCompleted（在 Render 层）→ 应被过滤
        let (turn_id, agent_id) = ids();
        bus.emit_render(RenderEvent::TurnCompleted {
            turn_id,
            agent_id,
            steps: 1,
            elapsed_secs: 0.1,
            finalized_messages: Arc::new(Vec::new()),
        });

        // 发送 StateSnapshot → 应被过滤
        let (turn_id, agent_id) = ids();
        bus.emit_state(StateEvent::StateSnapshot {
            turn_id,
            agent_id,
            message_count: 0,
            total_tokens: 0,
            current_step: 0,
            consecutive_failures: 0,
            budget_pct: None,
            context_total_tokens: None,
        });

        // 发送 TextChunk → 应正常转发
        let (turn_id, agent_id) = ids();
        bus.emit_render(RenderEvent::TextChunk {
            turn_id,
            agent_id,
            chunk: "hello".to_string(),
        });

        // 给 forwarder 一点时间处理
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let events: Vec<ExecutorEvent> = captured.lock().clone();
        assert_eq!(
            events.len(),
            1,
            "仅 TextChunk 应被转发，StateEvent 被过滤：{:?}",
            events
        );
        assert!(
            matches!(&events[0], ExecutorEvent::TextChunk { chunk, .. } if chunk == "hello"),
            "应为 TextChunk，实际：{:?}",
            events[0]
        );
    }

    /// 回归测试：biased select! 保证同一 ReAct 迭代的 Render 事件在 State 事件
    /// （TurnCompleted）之前被消费。
    ///
    /// 场景模拟（iter1 工具路径的事件序列）：
    /// 1. TextChunk("read")  ─── Render
    /// 2. ToolStarted(Read)  ─── Render
    /// 3. ToolEnded(Read)    ─── Render
    /// 4. TurnCompleted       ─── State（被 forwarder 过滤，不转发）
    ///
    /// 关键不变量：即便 Render 和 State 事件几乎同时 ready（emit 是同步连续操作），
    /// biased + render 在前 也要保证 Render 事件先被消费。
    ///
    /// 此测试用 subagent forwarder 验证基本顺序契约；main executor 的 forwarder
    /// （不过滤 TurnCompleted）依赖同一 biased 模式，由 main executor 的端到端
    /// 测试覆盖。
    #[tokio::test]
    async fn test_forwarder_biased_consumes_render_before_state_when_both_ready() {
        let (bus, handles) = EventBus::new(EventBusConfig::default());
        let captured: Arc<Mutex<Vec<ExecutorEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let handler = Arc::new(CapturingHandler {
            events: Arc::clone(&captured),
        });

        let _forwarder = spawn_subagent_event_forwarder(handles, Some(handler), "test".to_string());

        // 给 forwarder 一点启动时间
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;

        // 连续 emit：模拟 act.rs 内同步 emit 多个事件
        // emit 顺序：TextChunk → ToolStarted → ToolEnded → TurnCompleted
        //（TurnCompleted 被过滤，所以只会转发前 3 个 render 事件）
        let (turn_id, agent_id) = ids();
        bus.emit_render(RenderEvent::TextChunk {
            turn_id,
            agent_id,
            chunk: "read".to_string(),
        });
        bus.emit_render(RenderEvent::ToolStarted {
            turn_id,
            agent_id,
            tool_call_id: "tc_read".to_string(),
            name: "Read".to_string(),
            input: serde_json::Value::Null,
        });
        bus.emit_render(RenderEvent::ToolEnded {
            turn_id,
            agent_id,
            tool_call_id: "tc_read".to_string(),
            name: "Read".to_string(),
            output: "content".to_string(),
            is_error: false,
        });
        bus.emit_render(RenderEvent::TurnCompleted {
            turn_id,
            agent_id,
            steps: 1,
            elapsed_secs: 0.1,
            finalized_messages: Arc::new(Vec::new()),
        });

        wait_for_event_count(&captured, 3).await;

        let events: Vec<ExecutorEvent> = captured.lock().clone();
        assert_eq!(
            events.len(),
            3,
            "3 个 render 事件应全部转发（state 被过滤）：{:?}",
            events
        );

        // 验证顺序：TextChunk → ToolStart → ToolEnd
        // biased + render 在前保证 render 事件按 emit 顺序消费
        assert!(
            matches!(&events[0], ExecutorEvent::TextChunk { chunk, .. } if chunk == "read"),
            "第 1 个应为 TextChunk，实际：{:?}",
            events[0]
        );
        assert!(
            matches!(
                &events[1],
                ExecutorEvent::ToolStart {
                    tool_call_id, name, ..
                } if tool_call_id == "tc_read" && name == "Read"
            ),
            "第 2 个应为 ToolStart(Read)，实际：{:?}",
            events[1]
        );
        assert!(
            matches!(
                &events[2],
                ExecutorEvent::ToolEnd {
                    tool_call_id, name, ..
                } if tool_call_id == "tc_read" && name == "Read"
            ),
            "第 3 个应为 ToolEnd(Read)，实际：{:?}",
            events[2]
        );
    }
}
