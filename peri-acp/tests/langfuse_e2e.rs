// peri-acp/tests/langfuse_e2e.rs
// e2e mock 端到端测试：验证完整 turn 序列从 tracer → batcher → HTTP 的链路。
//
// 注意：`on_turn_end()` 内部调用 `tokio::spawn`，因此需要 `#[tokio::test]`
// 提供异步运行时。

mod tests {
    use langfuse_client::types::ObservationType;
    use langfuse_client::IngestionEvent;
    use peri_acp::langfuse::config::LangfuseConfig;
    use peri_acp::langfuse::fake_session::FakeLangfuseSession;
    use peri_acp::langfuse::LangfuseTracer;
    use peri_agent::agent::events::Stage;

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

    #[tokio::test]
    async fn test_e2e_complete_turn_with_fake_session() {
        // FakeLangfuseSession::new() 已返回 Arc<Self>
        let session = FakeLangfuseSession::new("sess_e2e");
        let config = make_config(1.0);
        let mut tracer = LangfuseTracer::new(session.clone(), "sess_e2e".to_string(), config);

        tracer.on_turn_start("turn_e2e");
        tracer.on_stage_start(Stage::Receive, "turn_e2e");
        tracer.on_stage_start(Stage::Reason, "turn_e2e");
        tracer.on_llm_start(0, &[], &[]);
        tracer.on_llm_end(0, "claude-sonnet-4", "anthropic", "hello world", None, None);
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
    // 验证 fork（同步）和 bg（后台）两种子 agent 场景下的 Langfuse trace 结构：
    // 子 agent 应产生 ObservationCreate（type=Agent）并关联到主 agent trace 中。

    /// Fork 子 agent：Agent 工具同步执行，on_tool_end 时弹栈并发出 ObservationCreate。
    /// 验证：子 agent observation 存在、类型为 Agent、有父 observation、内部工具有记录。
    #[tokio::test]
    async fn test_e2e_fork_subagent_emits_observation_create() {
        let session = FakeLangfuseSession::new("sess_e2e_fork");
        let config = make_config(1.0);
        let mut tracer = LangfuseTracer::new(session.clone(), "sess_e2e_fork".to_string(), config);

        // 主 agent 启动
        tracer.on_turn_start("主 agent 初始输入");
        tracer.on_stage_start(Stage::Act, "turn_fork");

        // 子 agent 启动（Agent 工具调用）
        let sa_input = serde_json::json!({"task": "读取文件内容"});
        tracer.on_tool_start("call_fork", "Agent", &sa_input);

        // 子 agent 内部执行（fork 模式：在同一进程中同步运行）
        tracer.on_stage_start(Stage::Reason, "turn_fork");
        tracer.on_llm_start(0, &[], &[]);
        tracer.on_llm_end(
            0,
            "claude-sonnet-4",
            "anthropic",
            "我来读取文件",
            None,
            None,
        );

        tracer.on_stage_start(Stage::Act, "turn_fork");
        // 子 agent 调用 Read 工具
        let read_params = serde_json::json!({"file_path": "/tmp/test.txt"});
        tracer.on_tool_start("sub_read", "Read", &read_params);
        tracer.on_tool_end("sub_read", "文件内容是 hello world", false);

        // 子 agent 结束（Agent 工具返回）
        tracer.on_tool_end("call_fork", "子 agent 执行完毕", false);

        // 主 agent 收尾
        let _handle = tracer.on_turn_end(None);

        let events = session.events_snapshot();

        // 断言 1：子 agent ObservationCreate 存在
        let sa_events: Vec<_> = events
            .iter()
            .filter(|e| {
                matches!(e, IngestionEvent::ObservationCreate { body, .. }
                    if body.name.as_deref().is_some_and(|n| n.starts_with("subagent-")))
            })
            .collect();
        assert_eq!(
            sa_events.len(),
            1,
            "fork 子 agent：应有恰好 1 个 ObservationCreate"
        );

        // 断言 2：类型为 Agent
        if let IngestionEvent::ObservationCreate { body, .. } = sa_events[0] {
            assert_eq!(
                body.r#type,
                ObservationType::Agent,
                "子 agent observation 类型应为 Agent"
            );
            // 断言 3：有父 observation
            assert!(
                body.parent_observation_id.is_some(),
                "子 agent 应有 parent_observation_id"
            );
            // 断言 4：有输入
            assert!(body.input.is_some(), "子 agent 应有输入 task 描述");
            // 断言 5：有输出
            assert!(body.output.is_some(), "子 agent 应有输出（工具执行结果）");
        }

        // 断言 6：子 agent 内部 Read 工具 observation 存在
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

    /// BG 子 agent：Agent 工具在子 agent 启动前就结束（on_tool_end 不弹栈），
    /// 等 on_turn_end 时统一清理。验证：deferred output 正确保留、ObservationCreate 延迟发出。
    #[tokio::test]
    async fn test_e2e_bg_subagent_defers_until_turn_end() {
        let session = FakeLangfuseSession::new("sess_e2e_bg");
        let config = make_config(1.0);
        let mut tracer = LangfuseTracer::new(session.clone(), "sess_e2e_bg".to_string(), config);

        // 主 agent 启动
        tracer.on_turn_start("主 agent 后台任务输入");
        tracer.on_stage_start(Stage::Act, "turn_bg");

        // 子 agent 启动（Agent 工具调用）
        let sa_input = serde_json::json!({"task": "后台搜索代码"});
        tracer.on_tool_start("call_bg", "Agent", &sa_input);

        // BG 场景：Agent 工具在子 agent 真正启动前就结束
        // （事件时序：on_tool_end 先到达，subagent 内部 StageStarted 后到达）
        tracer.on_tool_end("call_bg", "后台任务已分派", false);

        // 此时栈应为非空且 has_started=false → deferred_output 已记录
        // 验证：on_tool_end 后还没有 subagent ObservationCreate
        {
            let events_before_sa = session.events_snapshot();
            let has_sa_early = events_before_sa.iter().any(|e| {
                matches!(e, IngestionEvent::ObservationCreate { body, .. }
                    if body.name.as_deref().is_some_and(|n| n.starts_with("subagent-")))
            });
            assert!(
                !has_sa_early,
                "BG 场景：子 agent 启动前不应有 ObservationCreate"
            );
        }

        // 模拟子 agent 后续启动（通过 StageStarted 事件恢复活跃）
        tracer.on_stage_start(Stage::Receive, "turn_bg"); // 触发 mark_top_started
        tracer.on_stage_start(Stage::Reason, "turn_bg");
        tracer.on_llm_start(0, &[], &[]);
        tracer.on_llm_end(
            0,
            "claude-sonnet-4",
            "anthropic",
            "搜索完成，发现 3 个结果",
            None,
            None,
        );

        // Turn 结束——应清理 bg 子 agent 残留栈
        let _handle = tracer.on_turn_end(None);
        // agent-run ObservationCreate 在 tokio::spawn 中异步创建，需 yield
        tokio::task::yield_now().await;

        let events = session.events_snapshot();

        // 断言 1：子 agent ObservationCreate 在 turn_end 清理时发出
        let sa_events: Vec<_> = events
            .iter()
            .filter(|e| {
                matches!(e, IngestionEvent::ObservationCreate { body, .. }
                    if body.name.as_deref().is_some_and(|n| n.starts_with("subagent-")))
            })
            .collect();
        assert_eq!(
            sa_events.len(),
            1,
            "BG 子 agent：turn_end 后应有恰好 1 个 ObservationCreate"
        );

        // 断言 2：类型为 Agent
        if let IngestionEvent::ObservationCreate { body, .. } = sa_events[0] {
            assert_eq!(
                body.r#type,
                ObservationType::Agent,
                "BG 子 agent observation 类型应为 Agent"
            );
            // 断言 3：有父 observation（挂载到主 agent）
            assert!(
                body.parent_observation_id.is_some(),
                "BG 子 agent 应有 parent_observation_id 指向主 agent"
            );
            // 断言 4：deferred output 应被正确携带
            assert!(
                body.output.is_some(),
                "BG 子 agent 应有 output（含 deferred 的后台任务描述）"
            );
        }

        // 断言 5：agent-run 仍存在（主 agent 的 observation）
        let has_agent_run = events.iter().any(|e| {
            matches!(e, IngestionEvent::ObservationCreate { body, .. }
                if body.name.as_deref() == Some("agent-run"))
        });
        assert!(
            has_agent_run,
            "BG 场景：主 agent 的 agent-run observation 应存在"
        );
    }

    /// 子 agent ObservationCreate 的父节点关系验证：
    /// 子 agent 的 parent_observation_id 应指向主 agent 的 agent-run 或 stage span，
    /// 确保在 Langfuse UI 中子 agent 挂载在主 agent trace 树下。
    #[tokio::test]
    async fn test_e2e_subagent_has_correct_parent_hierarchy() {
        let session = FakeLangfuseSession::new("sess_e2e_parent");
        let config = make_config(1.0);
        let mut tracer =
            LangfuseTracer::new(session.clone(), "sess_e2e_parent".to_string(), config);

        tracer.on_turn_start("主 agent 层次测试");
        tracer.on_stage_start(Stage::Act, "turn_parent");

        // 子 agent
        let sa_input = serde_json::json!({"task": "层次测试任务"});
        tracer.on_tool_start("call_parent", "Agent", &sa_input);
        // 内部执行
        tracer.on_stage_start(Stage::Reason, "turn_parent");
        tracer.on_llm_start(0, &[], &[]);
        tracer.on_llm_end(0, "claude-sonnet-4", "anthropic", "完成", None, None);
        tracer.on_stage_start(Stage::Act, "turn_parent");
        let tool_input = serde_json::json!({"command": "ls"});
        tracer.on_tool_start("sub_bash", "Bash", &tool_input);
        tracer.on_tool_end("sub_bash", "file1 file2", false);
        tracer.on_tool_end("call_parent", "子 agent 完成", false);

        let _handle = tracer.on_turn_end(None);
        // agent-run ObservationCreate 在 tokio::spawn 中异步创建，需 yield
        tokio::task::yield_now().await;

        let events = session.events_snapshot();

        // 收集关键 observation ID
        let agent_run_id: Option<String> = events
            .iter()
            .filter_map(|e| {
                if let IngestionEvent::ObservationCreate { body, .. } = e {
                    if body.name.as_deref() == Some("agent-run") {
                        return body.id.clone();
                    }
                }
                None
            })
            .next();

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

            // 父节点必须是 agent-run 或 stage span（不能是 tool-batch 或空）
            let is_valid_parent =
                Some(parent_id) == agent_run_id.as_deref() || parent_id.starts_with("span_");
            assert!(
                is_valid_parent,
                "子 agent 的 parent_observation_id ({}) 应指向 agent-run 或 stage span",
                parent_id
            );
        }

        // 验证子 agent 的内部工具也存在且有父节点
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
        }
    }
}
