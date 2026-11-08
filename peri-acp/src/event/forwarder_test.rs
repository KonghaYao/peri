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

#[tokio::test]
async fn test_suspended_then_resumed_output_remains_fifo_when_forwarder_is_delayed() {
    let (bus, handles) = EventBus::new(EventBusConfig::default());
    let turn_id = peri_acp_types::session::TurnId::new();
    let agent_id = AgentId::new();
    bus.emit_render(RenderEvent::TurnSuspended { turn_id, agent_id });
    bus.emit_render(RenderEvent::TextChunk {
        turn_id,
        agent_id,
        message_id: peri_acp_types::messages::MessageId::new(),
        chunk: "resumed".into(),
    });
    drop(bus);
    let events = Arc::new(Mutex::new(Vec::new()));
    let captured = events.clone();
    let forwarder =
        spawn_eventbus_forwarder(handles, move |_, event| captured.lock().push(event), None);
    tokio::time::timeout(Duration::from_secs(3), forwarder)
        .await
        .unwrap()
        .unwrap();
    let events = events.lock();
    assert!(matches!(&events[0], ExecutorEvent::TurnSuspended { .. }));
    assert!(matches!(&events[1], ExecutorEvent::TextChunk { chunk, .. } if chunk == "resumed"));
}

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
        ..Default::default()
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
        origin_message_id: None,
        origin_readable: None,
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
        subagent_failure: None,
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
        cache_creation_input_tokens: None,
        cache_read_input_tokens: None,
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

/// bridge 内部保持 Stage 生命周期；验证 stage 事件可经 forwarder 的共享入口到达。
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
        origin_message_id: None,
        origin_readable: None,
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

/// [回归测试] 主会话观测必须借用请求快照，结束后释放来源并导出完整 fallback。
#[tokio::test]
async fn test_forwarder_shares_snapshot_until_freeze_then_releases_it() {
    use peri_acp_types::messages::BaseMessage;
    use peri_acp_types::session::TurnTelemetryOutcome;
    use peri_config::observability::LangfuseCompression;
    use peri_controller::langfuse::session::{LangfuseSession, LangfuseShutdownReport};
    use peri_controller::langfuse::session_like::LangfuseSessionLike;

    let mut server = mockito::Server::new_async().await;
    let bodies = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&bodies);
    let request = server
        .mock("POST", "/api/public/otel/v1/traces")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body_from_request(move |request| {
            captured.lock().push(
                serde_json::from_slice::<serde_json::Value>(request.body().unwrap()).unwrap(),
            );
            b"{}".to_vec()
        })
        .expect_at_least(1)
        .create_async()
        .await;
    let config = LangfuseConfig {
        public_key: Some("pk-local".into()),
        secret_key: Some("sk-local".into()),
        host: server.url(),
        compression: LangfuseCompression::None,
        spool_enabled: false,
        ..Default::default()
    };
    let session = Arc::new(
        LangfuseSession::new(config.clone(), "shared-snapshot".into(), None)
            .await
            .unwrap(),
    );
    let tracer = Arc::new(Mutex::new(LangfuseTracer::new(
        session.clone(),
        "shared-snapshot".into(),
        config,
    )));
    tracer.lock().on_turn_start("input");
    let main_id = AgentId::new();
    let bridge = LangfuseBridge::new(tracer.clone(), "local".into(), Some(main_id.to_string()));
    let messages = Arc::new(vec![BaseMessage::human("shared-context🙂".repeat(4096))]);
    let expected = serde_json::json!({"messages": messages.as_ref(), "tools": []});
    let original = Arc::downgrade(&messages);
    let (bus, handles) = EventBus::new(EventBusConfig::default());
    bus.emit_observe(ObserveEvent::LlmCallStart {
        turn_id: TurnId::new(),
        agent_id: main_id,
        step: 0,
        messages: Arc::clone(&messages),
        tools: vec![],
    });
    drop(bus);
    let forwarder = spawn_eventbus_forwarder(
        handles,
        move |_, event| {
            let ExecutorEvent::LlmCallStart { messages, .. } = event else {
                panic!("unexpected mapped event");
            };
            // 单线程 runtime 中此同步回调先于 worker 运行，原快照、ev、mapper 与冻结来源各持一份 Arc。
            assert_eq!(Arc::strong_count(&messages), 4);
            assert!(Arc::ptr_eq(&messages, &original.upgrade().unwrap()));
        },
        Some(bridge),
    );
    forwarder.await.unwrap();
    let end = tracer.lock().on_turn_end(TurnTelemetryOutcome::Completed);
    end.await.unwrap();
    assert_eq!(
        session.flush().await.unwrap(),
        langfuse_client::FlushReport::Complete
    );
    assert_eq!(Arc::strong_count(&messages), 1);
    assert_eq!(session.shutdown().await, LangfuseShutdownReport::Complete);
    request.assert_async().await;
    let captured = bodies.lock();
    let input = captured
        .iter()
        .flat_map(|body| body["resourceSpans"].as_array().unwrap())
        .flat_map(|resource| resource["scopeSpans"].as_array().unwrap())
        .flat_map(|scope| scope["spans"].as_array().unwrap())
        .find(|span| {
            span["name"]
                .as_str()
                .is_some_and(|name| name.starts_with("step-"))
        })
        .and_then(|span| span["attributes"].as_array())
        .unwrap()
        .iter()
        .find(|attribute| attribute["key"] == "langfuse.observation.input")
        .unwrap()["value"]["stringValue"]
        .as_str()
        .unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(input).unwrap(),
        expected
    );
}
