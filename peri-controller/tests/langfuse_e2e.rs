// peri-controller/tests/langfuse_e2e.rs
// e2e mock 端到端测试：验证完整 turn 序列从 tracer → batcher → HTTP 的链路。
//
// 注意：`on_turn_end()` 内部调用 `tokio::spawn`，因此需要 `#[tokio::test]`
// 提供异步运行时。

mod tests {
    use std::sync::Arc;

    use langfuse_client::types::ObservationType;
    use langfuse_client::IngestionEvent;
    use parking_lot::Mutex;
    use peri_acp_types::identity::AgentId;
    use peri_agent::agent::events::Stage;
    use peri_agent::agent::events::StageStatus;
    use peri_agent::agent::events_v2::{ObserveEvent, RenderEvent};
    use peri_agent::agent::LangfuseBridgeLike;
    use peri_agent::session::turn::TurnId;
    use peri_controller::langfuse::bridge::LangfuseBridge;
    use peri_controller::langfuse::config::LangfuseConfig;
    use peri_controller::langfuse::fake_session::FakeLangfuseSession;
    use peri_controller::langfuse::LangfuseTracer;

    fn make_config(rate: f64) -> LangfuseConfig {
        LangfuseConfig {
            public_key: None,
            secret_key: None,
            host: "https://cloud.langfuse.com".to_string(),
            trace_sampling: rate,
            error_span_always: true,
            batch_max_events: 50,
            batch_flush_interval_secs: 10,
            user_id: None,
        }
    }

    /// 构造 bridge 驱动的测试环境(生产路径:事件经 LangfuseBridge 注入,
    /// tracer 注入主 agent 身份——必须用测试实际生成的 main_id,
    /// 与事件侧 agent_id 一致,否则主 agent 事件会被判为未知 agent)。
    fn make_bridge(
        session: Arc<FakeLangfuseSession>,
    ) -> (
        LangfuseBridge,
        Arc<Mutex<LangfuseTracer>>,
        TurnId,
        AgentId,
        AgentId,
    ) {
        let config = make_config(1.0);
        let main_id = AgentId::new();
        let child_id = AgentId::new();
        let tracer = Arc::new(Mutex::new(LangfuseTracer::new(
            session.clone(),
            "sess_e2e".to_string(),
            config,
        )));
        let bridge = LangfuseBridge::new(
            tracer.clone(),
            "test-provider".to_string(),
            Some(main_id.to_string()),
        );
        let turn_id = TurnId::new();
        (bridge, tracer, turn_id, main_id, child_id)
    }

    #[tokio::test]
    async fn test_e2e_complete_turn_with_fake_session() {
        // FakeLangfuseSession::new() 已返回 Arc<Self>
        let session = FakeLangfuseSession::new("sess_e2e");
        let config = make_config(1.0);
        let mut tracer = LangfuseTracer::new(session.clone(), "sess_e2e".to_string(), config);

        tracer.on_turn_start("turn_e2e");
        tracer.on_stage_start(Stage::Receive, "turn_e2e");
        tracer.on_stage_start(Stage::Reason, "turn_e2e");
        tracer.on_llm_start("main", 0, &[], &[]);
        tracer.on_llm_end(
            "main",
            0,
            "claude-sonnet-4",
            "anthropic",
            "hello world",
            None,
            None,
        );
        let _handle = tracer.on_turn_end(None);

        tokio::task::yield_now().await;
        let events = session.events_snapshot();
        assert!(!events.is_empty(), "e2e: 完整 turn 应产生事件");

        // 验证包含 agent-run observation
        let has_agent_obs = events.iter().any(|e| {
            if let langfuse_client::IngestionEvent::ObservationCreate { body, .. } = e {
                body.name.as_deref() == Some("agent-run")
            } else {
                false
            }
        });
        assert!(has_agent_obs, "e2e: 应有 agent-run ObservationCreate");
    }

    #[tokio::test]
    async fn test_e2e_error_turn_with_zero_sampling() {
        // FakeLangfuseSession::new() 已返回 Arc<Self>
        let session = FakeLangfuseSession::new("sess_e2e_error");
        let config = make_config(0.0); // 采样率 0
        let mut tracer = LangfuseTracer::new(session.clone(), "sess_e2e_error".to_string(), config);

        tracer.on_turn_start("turn_err");
        let _handle = tracer.on_turn_end(Some("SomeError"));

        tokio::task::yield_now().await;
        let events = session.events_snapshot();
        // 采样率 0 但错误 turn，应有 ErrorSpan + 合成 TraceCreate
        let has_trace = events
            .iter()
            .any(|e| matches!(e, langfuse_client::IngestionEvent::TraceCreate { .. }));
        let has_error_span = events.iter().any(|e| {
            if let langfuse_client::IngestionEvent::SpanCreate { body, .. } = e {
                body.name.as_deref() == Some("ErrorTurn")
            } else {
                false
            }
        });
        assert!(has_trace, "e2e: 错误 turn 应补发 TraceCreate");
        assert!(has_error_span, "e2e: 错误 turn 应发 ErrorSpan");
    }

    // ── SubAgent e2e 测试 ──────────────────────────────────────────────────
    // 阶段②(registry)语义:AGENT obs 生命周期由 SubagentStart(create)/
    // SubagentStop(close)驱动;ToolEnded 不关 child;on_turn_end 仅兜底。
    // 事件经 LangfuseBridge 注入(与生产 forwarder 路径同构)。

    /// Fork 子 agent:Start join 创建 AGENT obs(open),Stop 关闭(update)。
    /// 验证:AGENT obs 存在、类型为 Agent、有父 observation、内部工具有记录。
    #[tokio::test]
    async fn test_e2e_fork_subagent_emits_observation_create() {
        let session = FakeLangfuseSession::new("sess_e2e_fork");
        let (bridge, tracer, turn_id, main_id, child_id) = make_bridge(session.clone());

        // 主 agent Act stage(冻结父 span)
        bridge.process_observe_event(&ObserveEvent::StageStarted {
            turn_id,
            agent_id: main_id,
            stage: Stage::Act,
        });
        // 父 Agent 工具
        bridge.process_render_event(&RenderEvent::ToolStarted {
            turn_id,
            agent_id: main_id,
            tool_call_id: "call_fork".to_string(),
            name: "Agent".to_string(),
            input: serde_json::json!({"task": "读取文件内容"}),
        });
        // SubagentStart:join → AGENT obs create(open)
        bridge.process_observe_event(&ObserveEvent::SubagentStart {
            turn_id,
            agent_id: main_id,
            child_agent_id: child_id,
            agent_name: "fork".to_string(),
            is_background: false,
        });

        // 子 agent 内部执行
        bridge.process_observe_event(&ObserveEvent::StageStarted {
            turn_id,
            agent_id: child_id,
            stage: Stage::Reason,
        });
        bridge.process_observe_event(&ObserveEvent::LlmCallStart {
            turn_id,
            agent_id: child_id,
            step: 0,
            messages: Arc::new(vec![]),
            tools: vec![],
        });
        bridge.process_observe_event(&ObserveEvent::LlmCallEnd {
            turn_id,
            agent_id: child_id,
            step: 0,
            model: "claude-sonnet-4".to_string(),
            output: "我来读取文件".to_string(),
            input_tokens: 10,
            output_tokens: 5,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
            request_id: None,
        });
        bridge.process_render_event(&RenderEvent::ToolStarted {
            turn_id,
            agent_id: child_id,
            tool_call_id: "sub_read".to_string(),
            name: "Read".to_string(),
            input: serde_json::json!({"file_path": "/tmp/test.txt"}),
        });
        bridge.process_render_event(&RenderEvent::ToolEnded {
            turn_id,
            agent_id: child_id,
            tool_call_id: "sub_read".to_string(),
            name: "Read".to_string(),
            output: "文件内容是 hello world".to_string(),
            is_error: false,
        });
        bridge.process_observe_event(&ObserveEvent::StageEnded {
            turn_id,
            agent_id: child_id,
            stage: Stage::Reason,
            status: StageStatus::Done,
            duration_ms: 5,
        });

        // 父 ToolEnded:只结束父工具记录,AGENT obs 未关闭
        bridge.process_render_event(&RenderEvent::ToolEnded {
            turn_id,
            agent_id: main_id,
            tool_call_id: "call_fork".to_string(),
            name: "Agent".to_string(),
            output: "子 agent 执行完毕".to_string(),
            is_error: false,
        });
        // SubagentStop:关闭 AGENT obs
        bridge.process_observe_event(&ObserveEvent::SubagentStop {
            turn_id,
            agent_id: main_id,
            child_agent_id: child_id,
            agent_name: "fork".to_string(),
            result: "子 agent 执行完毕".to_string(),
            is_error: false,
        });

        let _h = tracer.lock().on_turn_end(None);
        tokio::task::yield_now().await;
        let events = session.events_snapshot();

        // 断言 1:AGENT obs create 恰好 1 个
        let sa_creates: Vec<_> = events
            .iter()
            .filter(|e| {
                matches!(e, IngestionEvent::ObservationCreate { body, .. }
                    if body.name.as_deref().is_some_and(|n| n.starts_with("subagent-")))
            })
            .collect();
        assert_eq!(
            sa_creates.len(),
            1,
            "fork 子 agent：应有恰好 1 个 AGENT ObservationCreate"
        );

        // 断言 2:类型为 Agent + 有父 observation(join 时冻结的父 stage span)
        if let IngestionEvent::ObservationCreate { body, .. } = sa_creates[0] {
            assert_eq!(
                body.r#type,
                ObservationType::Agent,
                "子 agent observation 类型应为 Agent"
            );
            assert!(
                body.parent_observation_id.is_some(),
                "子 agent 应有 parent_observation_id"
            );
        }

        // 断言 3:AGENT obs 关闭(ObservationUpdate,Stop 驱动)
        let sa_updates = events
            .iter()
            .filter(|e| {
                matches!(e, IngestionEvent::ObservationUpdate { body, .. }
                    if body.name.as_deref().is_some_and(|n| n.starts_with("subagent-")))
            })
            .count();
        assert_eq!(
            sa_updates, 1,
            "fork 子 agent：Stop 后应有恰好 1 个 AGENT ObservationUpdate"
        );

        // 断言 4:子 agent 内部 Read 工具 observation 存在
        let has_read_tool = events.iter().any(|e| {
            matches!(e, IngestionEvent::ObservationCreate { body, .. }
                if body.r#type == ObservationType::Tool
                    && body.name.as_deref() == Some("Read"))
        });
        assert!(
            has_read_tool,
            "fork 子 agent：内部 Read 工具应在 events 中有记录"
        );
    }

    /// BG 子 agent:Start join 创建 AGENT obs,Stop 永不到达 → on_turn_end 兜底
    /// 关闭(ObservationUpdate + incomplete_reason)。验证:父 ToolEnded 不关闭,
    /// 兜底关闭存在且 metadata 携带 incomplete_reason。
    #[tokio::test]
    async fn test_e2e_bg_subagent_defers_until_turn_end() {
        let session = FakeLangfuseSession::new("sess_e2e_bg");
        let (bridge, tracer, turn_id, main_id, child_id) = make_bridge(session.clone());

        bridge.process_observe_event(&ObserveEvent::StageStarted {
            turn_id,
            agent_id: main_id,
            stage: Stage::Act,
        });
        bridge.process_render_event(&RenderEvent::ToolStarted {
            turn_id,
            agent_id: main_id,
            tool_call_id: "call_bg".to_string(),
            name: "Agent".to_string(),
            input: serde_json::json!({"task": "后台搜索代码"}),
        });
        // SubagentStart:join → AGENT obs create
        bridge.process_observe_event(&ObserveEvent::SubagentStart {
            turn_id,
            agent_id: main_id,
            child_agent_id: child_id,
            agent_name: "bg".to_string(),
            is_background: true,
        });
        // 父 ToolEnded:只结束父工具记录,不关闭 AGENT obs
        bridge.process_render_event(&RenderEvent::ToolEnded {
            turn_id,
            agent_id: main_id,
            tool_call_id: "call_bg".to_string(),
            name: "Agent".to_string(),
            output: "后台任务已分派".to_string(),
            is_error: false,
        });

        // 子 agent 内部执行(Stop 永不出现)
        bridge.process_observe_event(&ObserveEvent::StageStarted {
            turn_id,
            agent_id: child_id,
            stage: Stage::Reason,
        });
        bridge.process_observe_event(&ObserveEvent::LlmCallStart {
            turn_id,
            agent_id: child_id,
            step: 0,
            messages: Arc::new(vec![]),
            tools: vec![],
        });
        bridge.process_observe_event(&ObserveEvent::LlmCallEnd {
            turn_id,
            agent_id: child_id,
            step: 0,
            model: "claude-sonnet-4".to_string(),
            output: "搜索完成，发现 3 个结果".to_string(),
            input_tokens: 10,
            output_tokens: 5,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
            request_id: None,
        });
        bridge.process_observe_event(&ObserveEvent::StageEnded {
            turn_id,
            agent_id: child_id,
            stage: Stage::Reason,
            status: StageStatus::Done,
            duration_ms: 5,
        });

        // 父 ToolEnded 后、turn_end 前:AGENT obs 已 create(open)但未关闭
        {
            let mid = session.events_snapshot();
            let creates = mid
                .iter()
                .filter(|e| {
                    matches!(e, IngestionEvent::ObservationCreate { body, .. }
                        if body.name.as_deref().is_some_and(|n| n.starts_with("subagent-")))
                })
                .count();
            assert_eq!(creates, 1, "Start join 后 AGENT obs 应已创建(open)");
            let updates = mid
                .iter()
                .filter(|e| {
                    matches!(e, IngestionEvent::ObservationUpdate { body, .. }
                        if body.name.as_deref().is_some_and(|n| n.starts_with("subagent-")))
                })
                .count();
            assert_eq!(updates, 0, "Stop 未到时 AGENT obs 不应关闭");
        }

        // Turn 结束——兜底关闭 bg 子 agent
        let _h = tracer.lock().on_turn_end(None);
        tokio::task::yield_now().await;
        let events = session.events_snapshot();

        // 断言 1:兜底关闭存在(ObservationUpdate)
        let sa_updates: Vec<_> = events
            .iter()
            .filter(|e| {
                matches!(e, IngestionEvent::ObservationUpdate { body, .. }
                    if body.name.as_deref().is_some_and(|n| n.starts_with("subagent-")))
            })
            .collect();
        assert_eq!(
            sa_updates.len(),
            1,
            "BG 子 agent：turn_end 兜底应有恰好 1 个 AGENT ObservationUpdate"
        );
        if let IngestionEvent::ObservationUpdate { body, .. } = sa_updates[0] {
            assert_eq!(body.r#type, ObservationType::Agent);
            assert!(body.end_time.is_some(), "兜底关闭应带 end_time");
            assert!(
                body.metadata
                    .as_ref()
                    .and_then(|m| m.get("incomplete_reason"))
                    .is_some(),
                "兜底关闭 metadata 应携带 incomplete_reason"
            );
        }

        // 断言 2:agent-run 仍存在(主 agent 的 observation)
        let has_agent_run = events.iter().any(|e| {
            matches!(e, IngestionEvent::ObservationCreate { body, .. }
                if body.name.as_deref() == Some("agent-run"))
        });
        assert!(
            has_agent_run,
            "BG 场景：主 agent 的 agent-run observation 应存在"
        );
    }

    /// 子 agent AGENT obs 的父节点关系验证:
    /// parent 应为 join 时冻结的父 stage span(主 agent 的 act span),
    /// 不随运行时活跃 stage 漂移;内部工具挂在该 child 的 tool-batch 下。
    #[tokio::test]
    async fn test_e2e_subagent_has_correct_parent_hierarchy() {
        let session = FakeLangfuseSession::new("sess_e2e_parent");
        let (bridge, tracer, turn_id, main_id, child_id) = make_bridge(session.clone());

        // 主 agent Act stage:其 span 将是子 agent AGENT obs 的冻结父
        bridge.process_observe_event(&ObserveEvent::StageStarted {
            turn_id,
            agent_id: main_id,
            stage: Stage::Act,
        });
        // 确保主 stage duration > 0(v2 条件上报:0ms stage span 不上报)
        std::thread::sleep(std::time::Duration::from_millis(2));
        bridge.process_render_event(&RenderEvent::ToolStarted {
            turn_id,
            agent_id: main_id,
            tool_call_id: "call_parent".to_string(),
            name: "Agent".to_string(),
            input: serde_json::json!({"task": "层次测试任务"}),
        });
        bridge.process_observe_event(&ObserveEvent::SubagentStart {
            turn_id,
            agent_id: main_id,
            child_agent_id: child_id,
            agent_name: "child".to_string(),
            is_background: false,
        });
        // 子 agent 内部
        bridge.process_observe_event(&ObserveEvent::StageStarted {
            turn_id,
            agent_id: child_id,
            stage: Stage::Reason,
        });
        bridge.process_observe_event(&ObserveEvent::LlmCallStart {
            turn_id,
            agent_id: child_id,
            step: 0,
            messages: Arc::new(vec![]),
            tools: vec![],
        });
        bridge.process_observe_event(&ObserveEvent::LlmCallEnd {
            turn_id,
            agent_id: child_id,
            step: 0,
            model: "claude-sonnet-4".to_string(),
            output: "完成".to_string(),
            input_tokens: 10,
            output_tokens: 5,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
            request_id: None,
        });
        bridge.process_render_event(&RenderEvent::ToolStarted {
            turn_id,
            agent_id: child_id,
            tool_call_id: "sub_bash".to_string(),
            name: "Bash".to_string(),
            input: serde_json::json!({"command": "ls"}),
        });
        bridge.process_render_event(&RenderEvent::ToolEnded {
            turn_id,
            agent_id: child_id,
            tool_call_id: "sub_bash".to_string(),
            name: "Bash".to_string(),
            output: "file1 file2".to_string(),
            is_error: false,
        });
        bridge.process_observe_event(&ObserveEvent::StageEnded {
            turn_id,
            agent_id: child_id,
            stage: Stage::Reason,
            status: StageStatus::Done,
            duration_ms: 5,
        });
        bridge.process_render_event(&RenderEvent::ToolEnded {
            turn_id,
            agent_id: main_id,
            tool_call_id: "call_parent".to_string(),
            name: "Agent".to_string(),
            output: "子 agent 完成".to_string(),
            is_error: false,
        });
        bridge.process_observe_event(&ObserveEvent::SubagentStop {
            turn_id,
            agent_id: main_id,
            child_agent_id: child_id,
            agent_name: "child".to_string(),
            result: "子 agent 完成".to_string(),
            is_error: false,
        });
        // 主 agent Act stage 结束:发出 stage-act SpanCreate(供 parent 断言)
        bridge.process_observe_event(&ObserveEvent::StageEnded {
            turn_id,
            agent_id: main_id,
            stage: Stage::Act,
            status: StageStatus::Done,
            duration_ms: 5,
        });

        let _h = tracer.lock().on_turn_end(None);
        tokio::task::yield_now().await;
        let events = session.events_snapshot();

        // 收集主 agent act stage span id(父 span)
        let main_act_span: Option<String> = events
            .iter()
            .filter_map(|e| {
                if let IngestionEvent::SpanCreate { body, .. } = e {
                    if body.name.as_deref() == Some("stage-act") {
                        return body.id.clone();
                    }
                }
                None
            })
            .next();

        // 子 agent AGENT obs create 的 parent 应为冻结的父 stage span
        let sa_event = events
            .iter()
            .find(|e| {
                matches!(e, IngestionEvent::ObservationCreate { body, .. }
                    if body.name.as_deref().is_some_and(|n| n.starts_with("subagent-")))
            })
            .expect("应有子 agent ObservationCreate");

        if let IngestionEvent::ObservationCreate { body, .. } = sa_event {
            let parent_id = body
                .parent_observation_id
                .as_deref()
                .expect("子 agent 应有 parent_observation_id");
            assert_eq!(
                Some(parent_id),
                main_act_span.as_deref(),
                "子 agent 的 parent 应为 join 时冻结的父 stage span(不漂移)"
            );
        }

        // 验证子 agent 的内部工具也存在且有父节点(tool-batch)
        let sub_tool_event = events
            .iter()
            .find(|e| {
                matches!(e, IngestionEvent::ObservationCreate { body, .. }
                    if body.r#type == ObservationType::Tool
                        && body.name.as_deref() == Some("Bash"))
            })
            .expect("子 agent 内部的 Bash 工具应存在");

        if let IngestionEvent::ObservationCreate { body, .. } = sub_tool_event {
            assert!(
                body.parent_observation_id.is_some(),
                "子 agent 的工具应有 parent_observation_id"
            );
            // 工具 parent 是 batch span(非主 agent 的 act span)
            assert!(
                body.parent_observation_id.as_deref() != main_act_span.as_deref(),
                "子 agent 的工具不应直接挂主 agent 的 act span"
            );
        }
    }
}
