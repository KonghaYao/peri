//! `spawn_eventbus_forwarder` 协议化前分支测试（L4 验收）。
//!
//! 验证：观测事件在主事件流**协议化前**分支给 Langfuse bridge——
//! 同一 v2 事件既经 `*_event_to_executor` 映射为 `ExecutorEvent` 走协议化路径
//! （`on_event`），又在 mapper 之前分支给 `LangfuseBridge`（旁路观测）。
//! bridge 消费不阻塞、不改变主链路事件（fire-and-forget + 同步入队）。

use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use peri_acp_types::event::ExecutorEvent;
use peri_acp_types::event_v2::{EventBus, EventBusConfig, ObserveEvent, RenderEvent};
use peri_acp_types::identity::AgentId;
use peri_acp_types::session::TurnId;
use peri_controller::langfuse::bridge::LangfuseBridge;
use peri_controller::langfuse::config::LangfuseConfig;
use peri_controller::langfuse::fake_session::FakeLangfuseSession;
use peri_controller::langfuse::tracer::LangfuseTracer;

use crate::event::spawn_eventbus_forwarder;

/// 轮询等待条件成立（forwarder 是 fire-and-forget task，无 JoinHandle 可 await）。
async fn wait_until(mut cond: impl FnMut() -> bool) -> bool {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while tokio::time::Instant::now() < deadline {
        if cond() {
            return true;
        }
        tokio::task::yield_now().await;
    }
    cond()
}

fn make_config() -> LangfuseConfig {
    LangfuseConfig {
        public_key: None,
        secret_key: None,
        host: "https://cloud.langfuse.com".to_string(),
        trace_sampling: 1.0,
        error_span_always: true,
        batch_max_events: 50,
        batch_flush_interval_secs: 10,
        user_id: None,
    }
}

/// render/observe 事件经 forwarder 时：
/// 1. bridge（旁路）收到同一事件并写入 Langfuse 观测（fake session 快照可见）；
/// 2. mapper 输出经 `on_event` 到达协议化路径。
#[tokio::test]
async fn test_forwarder_branches_to_bridge_before_mapper() {
    let session = FakeLangfuseSession::new("sess_forwarder");
    let tracer = Arc::new(Mutex::new(LangfuseTracer::new(
        session.clone(),
        "sess_forwarder".to_string(),
        make_config(),
    )));
    let main_id = AgentId::new();
    let bridge = LangfuseBridge::new(
        tracer.clone(),
        "test-provider".to_string(),
        Some(main_id.to_string()),
    );

    let (bus, handles) = EventBus::new(EventBusConfig::default());
    let turn_id = TurnId::new();
    let collected: Arc<Mutex<Vec<ExecutorEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let collected_for_task = Arc::clone(&collected);
    spawn_eventbus_forwarder(
        handles,
        move |_source, ev| collected_for_task.lock().push(ev),
        Some(bridge),
    );

    // 生产路径中 turn 开始时由 spawn_event_pump 头调用 on_turn_start
    // （创建 agent-run observation，stage/generation 的 parent 锚点）。
    tracer.lock().on_turn_start("input");

    // ── render 层：TextChunk / ToolStarted / ToolEnded ──────────────────────
    bus.emit_render(RenderEvent::ToolStarted {
        turn_id,
        agent_id: main_id,
        tool_call_id: "call_1".to_string(),
        name: "Read".to_string(),
        input: serde_json::json!({"path": "/tmp/x"}),
    });
    bus.emit_render(RenderEvent::TextChunk {
        turn_id,
        agent_id: main_id,
        message_id: peri_acp_types::messages::MessageId::new(),
        chunk: "hello".to_string(),
    });
    bus.emit_render(RenderEvent::ToolEnded {
        turn_id,
        agent_id: main_id,
        tool_call_id: "call_1".to_string(),
        name: "Read".to_string(),
        output: "内容".to_string(),
        is_error: false,
    });

    // ── observe 层：LlmCallStart / LlmCallEnd（StageStarted 为 tracer-only）───
    bus.emit_observe(ObserveEvent::LlmCallStart {
        turn_id,
        agent_id: main_id,
        step: 0,
        messages: Arc::new(vec![]),
        tools: vec![],
    });
    bus.emit_observe(ObserveEvent::LlmCallEnd {
        turn_id,
        agent_id: main_id,
        step: 0,
        model: "test-model".to_string(),
        output: "out".to_string(),
        input_tokens: 10,
        output_tokens: 5,
        cache_creation_input_tokens: 0,
        cache_read_input_tokens: 0,
        request_id: None,
    });

    // 关闭全部通道 → forwarder 三通道 Closed/None 后自动退出
    drop(bus);
    assert!(
        wait_until(|| collected.lock().len() == 5).await,
        "mapper 应输出 5 个映射事件（观测分支不影响主链路）"
    );
    // bridge 侧为同步入队（batcher），task 消费后快照即可见
    assert!(
        wait_until(|| !session.events_snapshot().is_empty()).await,
        "bridge 应从事件流收到观测事件（非空）"
    );

    // 断言 1：协议化路径收到映射后的 ExecutorEvent（mapper 输出不被观测影响）
    let mapped = collected.lock().clone();
    assert!(
        mapped
            .iter()
            .any(|e| matches!(e, ExecutorEvent::ToolStart { name, .. } if name == "Read")),
        "ToolStarted 应映射为 ExecutorEvent::ToolStart"
    );
    assert!(
        mapped
            .iter()
            .any(|e| matches!(e, ExecutorEvent::TextChunk { chunk, .. } if chunk == "hello")),
        "TextChunk 应映射为 ExecutorEvent::TextChunk"
    );
    assert!(
        mapped
            .iter()
            .any(|e| matches!(e, ExecutorEvent::ToolEnd { name, .. } if name == "Read")),
        "ToolEnded 应映射为 ExecutorEvent::ToolEnd"
    );
    assert!(
        mapped
            .iter()
            .any(|e| matches!(e, ExecutorEvent::LlmCallStart { step: 0, .. })),
        "LlmCallStart 应映射为 ExecutorEvent::LlmCallStart"
    );
    assert!(
        mapped.iter().any(|e| matches!(
            e,
            ExecutorEvent::LlmCallEnd { model, usage, .. }
                if model == "test-model" && usage.as_ref().map(|u| u.output_tokens) == Some(5)
        )),
        "LlmCallEnd 应映射为 ExecutorEvent::LlmCallEnd（含 usage）"
    );

    // 断言 2：bridge 旁路收到同一事件（Langfuse 观测快照可见）
    let events = session.events_snapshot();
    assert!(!events.is_empty(), "bridge 应从事件流收到观测事件（非空）");
    assert!(
        events.iter().any(|e| {
            if let langfuse_client::IngestionEvent::GenerationCreate { body, .. } = e {
                // generation name = step-{n}（tracer 契约，见 tracer/mod.rs on_llm_end）
                body.name.as_deref() == Some("step-0")
            } else {
                false
            }
        }),
        "LlmCallEnd 应经 bridge 生成 GenerationCreate（旁路观测可达）"
    );
}

/// bridge 为 None 时（遥测禁用）：协议化路径照常工作，无观测副作用。
#[tokio::test]
async fn test_forwarder_without_bridge_keeps_mapper_path() {
    let (bus, handles) = EventBus::new(EventBusConfig::default());
    let turn_id = TurnId::new();
    let agent_id = AgentId::new();
    let collected: Arc<Mutex<Vec<ExecutorEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let collected_for_task = Arc::clone(&collected);
    spawn_eventbus_forwarder(
        handles,
        move |_source, ev| collected_for_task.lock().push(ev),
        None,
    );

    bus.emit_render(RenderEvent::TextChunk {
        turn_id,
        agent_id,
        message_id: peri_acp_types::messages::MessageId::new(),
        chunk: "no-telemetry".to_string(),
    });
    drop(bus);
    assert!(
        wait_until(|| collected.lock().len() == 1).await,
        "bridge=None 时 mapper 路径不受影响"
    );

    let mapped = collected.lock().clone();
    assert_eq!(mapped.len(), 1, "bridge=None 时 mapper 路径不受影响");
    assert!(
        matches!(&mapped[0], ExecutorEvent::TextChunk { chunk, .. } if chunk == "no-telemetry")
    );
}

/// active_stage 借用的编译级不变量：forwarder 传入真实 HashMap 供 Stage 生命周期
/// 跨事件保持（与 bridge_test C4 同入口语义，此处仅验证 stage 事件可经 forwarder
/// 到达 bridge 且不 panic）。
#[tokio::test]
async fn test_forwarder_stage_events_reach_bridge() {
    let session = FakeLangfuseSession::new("sess_forwarder_stage");
    let tracer = Arc::new(Mutex::new(LangfuseTracer::new(
        session.clone(),
        "sess_forwarder_stage".to_string(),
        make_config(),
    )));
    let main_id = AgentId::new();
    let bridge = LangfuseBridge::new(
        tracer.clone(),
        "test-provider".to_string(),
        Some(main_id.to_string()),
    );

    let (bus, handles) = EventBus::new(EventBusConfig::default());
    let turn_id = TurnId::new();
    let collected: Arc<Mutex<Vec<ExecutorEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let collected_for_task = Arc::clone(&collected);
    spawn_eventbus_forwarder(
        handles,
        move |_source, ev| collected_for_task.lock().push(ev),
        Some(bridge),
    );

    // 生产路径中 turn 开始时由 spawn_event_pump 头调用 on_turn_start
    tracer.lock().on_turn_start("input");

    use peri_acp_types::event::{Stage, StageStatus};
    bus.emit_observe(ObserveEvent::StageStarted {
        turn_id,
        agent_id: main_id,
        stage: Stage::Act,
    });
    // 说明：stage span 在 on_stage_end 按真实耗时条件上报（0ms 跳过，v2 §1.2）；
    // 异步 forwarder 消费 StageStarted/StageEnded 的间隔无法在测试中控制，
    // 故此处不断言 stage span 内容（bridge_test 同步 harness 已覆盖该语义），
    // 只验证 Stage 事件经 forwarder 到达 bridge 且不 panic、不污染协议化路径。
    bus.emit_render(RenderEvent::ToolStarted {
        turn_id,
        agent_id: main_id,
        tool_call_id: "call_stage".to_string(),
        name: "Agent".to_string(),
        input: serde_json::json!({}),
    });
    bus.emit_observe(ObserveEvent::StageEnded {
        turn_id,
        agent_id: main_id,
        stage: Stage::Act,
        status: StageStatus::Done,
        duration_ms: 1,
    });

    drop(bus);
    assert!(
        wait_until(|| collected.lock().len() == 1).await,
        "协议化路径应恰好收到 ToolStarted 的映射（Stage 为 tracer-only）"
    );
    // Stage 事件为 tracer-only（不映射到 ExecutorEvent），但 bridge 侧应有观测
    assert!(
        wait_until(|| !session.events_snapshot().is_empty()).await,
        "stage 事件应到达 bridge 观测（不 panic）"
    );
    assert!(
        matches!(&collected.lock()[0], ExecutorEvent::ToolStart { name, .. } if name == "Agent")
    );
}
