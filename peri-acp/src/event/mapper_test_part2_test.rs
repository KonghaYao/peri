#[test]
fn test_text_chunk_maps_to_session_update_with_source() {
    // TextChunk → AgentMessageChunk，携带 source_agent_id
    let event = ExecutorEvent::TextChunk {
        message_id: MessageId::new(),
        chunk: "Hello world".to_string(),
        source_agent_id: Some("sub-agent-1".to_string()),
    };
    let mapped = crate::event::mapper::map_event(&event, 200_000, &PeriCaps::default());
    assert_eq!(mapped.len(), 1);
    assert_eq!(mapped[0].updates.len(), 1);
    assert_eq!(
        mapped[0].source_agent_id.as_deref(),
        Some("sub-agent-1"),
        "应携带 source_agent_id"
    );
    match &mapped[0].updates[0] {
        SessionUpdate::AgentMessageChunk(chunk) => match &chunk.content {
            ContentBlock::Text(tc) => {
                assert_eq!(tc.text, "Hello world");
            }
            other => panic!("预期 Text ContentBlock，实际: {:?}", other),
        },
        other => panic!("预期 AgentMessageChunk，实际: {:?}", other),
    }
}

#[test]
fn test_text_chunk_without_source_agent_id() {
    // TextChunk 无 source_agent_id 时 source_agent_id 为 None
    let event = ExecutorEvent::TextChunk {
        message_id: MessageId::new(),
        chunk: "main text".to_string(),
        source_agent_id: None,
    };
    let mapped = crate::event::mapper::map_event(&event, 200_000, &PeriCaps::default());
    assert_eq!(mapped.len(), 1);
    assert!(mapped[0].source_agent_id.is_none());
}

#[test]
fn test_tool_start_maps_to_session_update_with_tool_info() {
    // ToolStart → ToolCall SessionUpdate，携带 tool_call_id/name/kind/status/raw_input
    let event = ExecutorEvent::ToolStart {
        message_id: MessageId::new(),
        tool_call_id: "tc-456".to_string(),
        name: "Bash".to_string(),
        input: serde_json::json!({"command": "ls -la"}),
        source_agent_id: Some("sub-agent-2".to_string()),
        origin_message_id: None,
        origin_readable: None,
    };
    let mapped = crate::event::mapper::map_event(&event, 200_000, &PeriCaps::default());
    assert_eq!(mapped.len(), 1);
    assert_eq!(mapped[0].updates.len(), 1);
    assert_eq!(
        mapped[0].source_agent_id.as_deref(),
        Some("sub-agent-2"),
        "应携带 source_agent_id"
    );
    match &mapped[0].updates[0] {
        SessionUpdate::ToolCall(tc) => {
            assert_eq!(tc.tool_call_id.0.as_ref(), "tc-456");
            assert_eq!(tc.title, "Bash");
            assert_eq!(tc.kind, ToolKind::Execute, "Bash 应推断为 Execute");
            assert_eq!(tc.status, ToolCallStatus::InProgress);
            assert!(tc.raw_input.is_some(), "raw_input 应存在");
        }
        other => panic!("预期 ToolCall，实际: {:?}", other),
    }
}

#[test]
fn test_tool_start_infer_tool_kind_variants() {
    // 验证 infer_tool_kind 对不同工具名的推断结果
    //
    // v4-part-2（A4 匹配型归一）：builtin 一等工具的 effective name（迁移后模型面
    // 实际名字）必须仍推断为与原工具相同的 kind；名字字面量只在
    // `peri_acp_types::builtin_mcp` 声明一份，此处按声明表派生，不复写 `mcp__*`
    // 字面量（消费点不得硬编码 effective name）。
    let effective = |instance: &str, original: &str| -> &'static str {
        peri_acp_types::builtin_mcp::find(instance)
            .and_then(|declared| {
                declared
                    .tools
                    .iter()
                    .find(|tool| tool.original_name == original)
            })
            .map(|tool| tool.effective_name)
            .expect("builtin 声明表应声明该 (实例, 原始工具名)")
    };
    let cases = [
        ("Read", ToolKind::Read),
        ("Write", ToolKind::Edit),
        ("Edit", ToolKind::Edit),
        ("folder_operations", ToolKind::Edit),
        ("Bash", ToolKind::Execute),
        ("Grep", ToolKind::Search),
        ("Glob", ToolKind::Search),
        ("WebFetch", ToolKind::Fetch),
        ("WebSearch", ToolKind::Fetch),
        (effective("web", "WebFetch"), ToolKind::Fetch),
        (effective("web", "WebSearch"), ToolKind::Fetch),
        // artifact 不属于任何 Fetch 分支：归一到原始名 `artifact` 后仍是 Other
        (effective("artifact", "artifact"), ToolKind::Other),
        ("mcp__server__tool", ToolKind::Other),
        // 反证：未命中归一表（外部 server）不得被误吞
        ("mcp__foo__bar", ToolKind::Other),
        ("mcp__filesystem__read_file", ToolKind::Other),
    ];
    for (name, expected_kind) in cases {
        let event = ExecutorEvent::ToolStart {
            message_id: MessageId::new(),
            tool_call_id: "tc-x".to_string(),
            name: name.to_string(),
            input: serde_json::Value::Null,
            source_agent_id: None,
            origin_message_id: None,
            origin_readable: None,
        };
        let mapped = crate::event::mapper::map_event(&event, 200_000, &PeriCaps::default());
        match &mapped[0].updates[0] {
            SessionUpdate::ToolCall(tc) => {
                assert_eq!(
                    tc.kind, expected_kind,
                    "工具名 {} 的 kind 应为 {:?}",
                    name, expected_kind
                );
            }
            other => panic!("{} 预期 ToolCall，实际: {:?}", name, other),
        }
    }

    // 大小写不匹配的 effective name（非声明表字面量）不命中归一 ⇒ 仍为 Other
    let lowered = effective("web", "WebFetch").to_lowercase();
    assert!(
        peri_acp_types::builtin_mcp::original_tool_name_of_effective(&lowered).is_none(),
        "归一表是精确匹配：{lowered} 不得命中"
    );
    let event = ExecutorEvent::ToolStart {
        message_id: MessageId::new(),
        tool_call_id: "tc-lower".to_string(),
        name: lowered.clone(),
        input: serde_json::Value::Null,
        source_agent_id: None,
        origin_message_id: None,
        origin_readable: None,
    };
    match &crate::event::mapper::map_event(&event, 200_000, &PeriCaps::default())[0].updates[0] {
        SessionUpdate::ToolCall(tc) => {
            assert_eq!(tc.kind, ToolKind::Other, "{lowered} 不得被误分类为 Fetch");
            assert_eq!(
                tc.title, lowered,
                "投影真值仍是模型面名字（归一不改写载荷）"
            );
        }
        other => panic!("{lowered} 预期 ToolCall，实际: {:?}", other),
    }
}

#[test]
fn test_todo_update_maps_to_session_update() {
    // TodoUpdate → Plan SessionUpdate，条目状态正确映射
    let entries = vec![
        TodoEntry {
            content: "实现功能 A".to_string(),
            active_form: Some("正在实现功能 A".to_string()),
            status: TodoStatus::InProgress,
        },
        TodoEntry {
            content: "测试功能 B".to_string(),
            active_form: None,
            status: TodoStatus::Pending,
        },
        TodoEntry {
            content: "完成功能 C".to_string(),
            active_form: None,
            status: TodoStatus::Completed,
        },
    ];
    let event = ExecutorEvent::TodoUpdate(entries);
    let mapped = crate::event::mapper::map_event(&event, 200_000, &PeriCaps::default());
    assert_eq!(mapped.len(), 1);
    assert_eq!(mapped[0].updates.len(), 1);
    match &mapped[0].updates[0] {
        SessionUpdate::Plan(plan) => {
            assert_eq!(plan.entries.len(), 3, "Plan 应包含 3 个条目");
            assert_eq!(plan.entries[0].content, "实现功能 A");
            assert_eq!(plan.entries[0].status, PlanEntryStatus::InProgress);
            assert!(plan.entries[0].meta.is_none(), "未协商不得附加 Peri 元数据");
            assert_eq!(plan.entries[1].status, PlanEntryStatus::Pending);
            assert_eq!(plan.entries[2].status, PlanEntryStatus::Completed);
            // 所有条目优先级为 Medium（mapper 中硬编码）
            for entry in &plan.entries {
                assert_eq!(entry.priority, PlanEntryPriority::Medium);
            }
        }
        other => panic!("预期 Plan，实际: {:?}", other),
    }
}

#[test]
fn test_todo_active_form_requires_negotiated_capability() {
    let event = ExecutorEvent::TodoUpdate(vec![TodoEntry {
        content: "运行测试".into(),
        active_form: Some("正在运行测试".into()),
        status: TodoStatus::InProgress,
    }]);
    let caps = PeriCaps {
        plan_entry_active_form: true,
        ..PeriCaps::default()
    };
    let mapped = crate::event::mapper::map_event(&event, 200_000, &caps);
    let SessionUpdate::Plan(plan) = &mapped[0].updates[0] else {
        panic!("预期 Plan")
    };
    assert_eq!(
        plan.entries[0]
            .meta
            .as_ref()
            .and_then(|meta| meta.get("activeForm"))
            .and_then(serde_json::Value::as_str),
        Some("正在运行测试")
    );
}

#[test]
fn test_todo_update_empty_entries() {
    // 空 TodoUpdate → 空 Plan（条目数为 0）
    let event = ExecutorEvent::TodoUpdate(vec![]);
    let mapped = crate::event::mapper::map_event(&event, 200_000, &PeriCaps::default());
    assert_eq!(mapped.len(), 1);
    match &mapped[0].updates[0] {
        SessionUpdate::Plan(plan) => {
            assert!(plan.entries.is_empty(), "空 TodoUpdate 应产出空 Plan");
        }
        other => panic!("预期 Plan，实际: {:?}", other),
    }
}

#[test]
fn test_state_snapshot_no_session_update() {
    assert_no_session_update(&ExecutorEvent::StateSnapshot(vec![]), "StateSnapshot");
}

#[test]
fn test_state_snapshot_meta_no_session_update() {
    assert_no_session_update(
        &ExecutorEvent::StateSnapshotMeta {
            message_count: 5,
            total_tokens: 0,
            current_step: 2,
            consecutive_failures: 0,
            budget_pct: Some(0.42),
            context_total_tokens: Some(200_000),
        },
        "StateSnapshotMeta",
    );
}

#[test]
fn test_subagent_started_no_session_update() {
    assert_no_session_update(
        &ExecutorEvent::SubagentStarted {
            agent_name: "sub-agent".to_string(),
            instance_id: "inst-001".to_string(),
            is_background: false,
            parent_tool_call_id: None,
        },
        "SubagentStarted",
    );
}

#[test]
fn test_subagent_stopped_no_session_update() {
    assert_no_session_update(
        &ExecutorEvent::SubagentStopped {
            agent_name: "sub-agent".to_string(),
            result: "done".to_string(),
            is_error: false,
            instance_id: "inst-001".to_string(),
            subagent_failure: None,
        },
        "SubagentStopped",
    );
}

#[test]
fn test_compact_started_no_session_update() {
    assert_no_session_update(
        &ExecutorEvent::CompactStarted {
            turn_id: "turn_1".into(),
            agent_id: "agent_1".into(),
            step: 0,
            strategy: CompactStrategy::Smart,
            trigger: CompactTrigger::Auto,
        },
        "CompactStarted",
    );
}

#[test]
fn test_compact_completed_no_session_update() {
    // CompactCompleted 是私有事件，不产生标准 SessionUpdate。
    assert_no_session_update(
        &ExecutorEvent::CompactCompleted {
            summary: "compressed".to_string(),
            messages: vec![],
            trigger: CompactTrigger::Auto,
            strategy: CompactStrategy::Micro,
            affected_count: 2,
            estimated_tokens_saved: 128,
            files: vec![],
            skills: vec![],
        },
        "CompactCompleted",
    );
}

#[test]
fn test_background_task_completed_no_session_update() {
    assert_no_session_update(
        &ExecutorEvent::BackgroundTaskCompleted(BackgroundTaskResult {
            task_id: "bg-001".to_string(),
            agent_name: "bg-agent".to_string(),
            prompt_summary: "do stuff".to_string(),
            success: true,
            output: "ok".to_string(),
            tool_calls_count: 3,
            duration_ms: 5000,
            child_thread_id: None,
            timed_out: false,
            subagent_failure: None,
            shell_output: None,
        }),
        "BackgroundTaskCompleted",
    );
}

#[test]
fn test_command_feedback_no_session_update() {
    assert_no_session_update(
        &ExecutorEvent::CommandFeedback(CommandFeedback {
            level: FeedbackLevel::Info,
            message: "ok".into(),
            channel: FeedbackChannel::UiOnly,
        }),
        "CommandFeedback",
    );
}

#[test]
fn test_message_added_produces_user_message_chunk() {
    let result = crate::event::mapper::map_event(
        &ExecutorEvent::MessageAdded(BaseMessage::human("bg result text")),
        200000,
        &PeriCaps::default(),
    );
    assert_eq!(result.len(), 1);
    assert!(
        !result[0].updates.is_empty(),
        "MessageAdded 应产生 SessionUpdate"
    );
    match &result[0].updates[0] {
        SessionUpdate::UserMessageChunk(_chunk) => {
            // UserMessageChunk 携带 ContentChunk，由 ACP SDK 序列化——不测试内部结构
        }
        other => panic!("应为 UserMessageChunk，实际: {:?}", other),
    }
}

#[test]
fn test_llm_call_start_no_output() {
    assert_no_session_update(
        &ExecutorEvent::LlmCallStart {
            step: 1,
            messages: std::sync::Arc::new(vec![BaseMessage::human("hello")]),
            tools: vec![ToolDefinition {
                name: "Bash".to_string(),
                description: "Run command".to_string(),
                parameters: serde_json::Value::Null,
            }],
        },
        "LlmCallStart",
    );
}

/// 真实所属 assistant 消息经 ACP `_meta.peri.originMessageId` 透传；
/// 旧事件没有该字段时不写 `_meta`（客户端回退自身启发式）。
#[test]
fn test_tool_start_origin_message_id_is_carried_in_acp_meta() {
    let origin = MessageId::new();
    let origin_text = origin.as_uuid().to_string();
    let event = ExecutorEvent::ToolStart {
        message_id: MessageId::new(),
        origin_message_id: Some(origin),
        origin_readable: Some(true),
        tool_call_id: "tc-meta".to_string(),
        name: "Read".to_string(),
        input: serde_json::json!({"file_path": "/tmp/a"}),
        source_agent_id: None,
    };
    let mapped = crate::event::mapper::map_event(&event, 200_000, &PeriCaps::default());
    let SessionUpdate::ToolCall(tool_call) = &mapped[0].updates[0] else {
        panic!("ToolStart 应映射为 ToolCall");
    };
    let meta = tool_call.meta.as_ref().expect("_meta 必须存在");
    assert_eq!(
        meta.get("peri")
            .and_then(|peri| peri.get("originMessageId"))
            .and_then(serde_json::Value::as_str),
        Some(origin_text.as_str()),
        "origin message id 必须进入 ACP _meta"
    );
    assert_eq!(
        meta.get("peri")
            .and_then(|peri| peri.get("originReadable"))
            .and_then(serde_json::Value::as_bool),
        Some(true),
        "可回读 origin 必须显式写 originReadable=true"
    );

    // wire key 是 `_meta`（ACP SDK 的 serde rename），不是 `meta`。
    let wire = serde_json::to_value(SessionUpdate::ToolCall(tool_call.clone())).unwrap();
    assert_eq!(
        wire.get("_meta")
            .and_then(|meta| meta.get("peri"))
            .and_then(|peri| peri.get("originMessageId"))
            .and_then(serde_json::Value::as_str),
        Some(origin_text.as_str())
    );

    // 嵌套执行：确认无持久 origin，只写 originReadable=false。
    let nested = ExecutorEvent::ToolStart {
        message_id: MessageId::new(),
        origin_message_id: None,
        origin_readable: Some(false),
        tool_call_id: "tc-nested".to_string(),
        name: "Read".to_string(),
        input: serde_json::json!({}),
        source_agent_id: None,
    };
    let mapped = crate::event::mapper::map_event(&nested, 200_000, &PeriCaps::default());
    let SessionUpdate::ToolCall(tool_call) = &mapped[0].updates[0] else {
        panic!("ToolStart 应映射为 ToolCall");
    };
    let meta = tool_call.meta.as_ref().expect("_meta 必须存在");
    assert_eq!(
        meta.get("peri")
            .and_then(|peri| peri.get("originReadable"))
            .and_then(serde_json::Value::as_bool),
        Some(false),
        "嵌套执行必须显式写 originReadable=false"
    );
    assert!(
        meta.get("peri")
            .and_then(|peri| peri.get("originMessageId"))
            .is_none(),
        "不可回读时不得写 originMessageId"
    );

    // 旧服务端事件：origin 存在但没有三态字段，只写 originMessageId（不补造 true）。
    let legacy_origin = ExecutorEvent::ToolStart {
        message_id: MessageId::new(),
        origin_message_id: Some(origin),
        origin_readable: None,
        tool_call_id: "tc-legacy-origin".to_string(),
        name: "Read".to_string(),
        input: serde_json::json!({}),
        source_agent_id: None,
    };
    let mapped = crate::event::mapper::map_event(&legacy_origin, 200_000, &PeriCaps::default());
    let SessionUpdate::ToolCall(tool_call) = &mapped[0].updates[0] else {
        panic!("ToolStart 应映射为 ToolCall");
    };
    let meta = tool_call.meta.as_ref().expect("_meta 必须存在");
    assert!(
        meta.get("peri")
            .and_then(|peri| peri.get("originReadable"))
            .is_none(),
        "旧事件语义未知，不得补写 originReadable"
    );

    let legacy = ExecutorEvent::ToolStart {
        message_id: MessageId::new(),
        origin_message_id: None,
        origin_readable: None,
        tool_call_id: "tc-legacy".to_string(),
        name: "Read".to_string(),
        input: serde_json::json!({}),
        source_agent_id: None,
    };
    let mapped = crate::event::mapper::map_event(&legacy, 200_000, &PeriCaps::default());
    let SessionUpdate::ToolCall(tool_call) = &mapped[0].updates[0] else {
        panic!("ToolStart 应映射为 ToolCall");
    };
    assert!(tool_call.meta.is_none(), "旧事件不得凭空补造归属");
}
