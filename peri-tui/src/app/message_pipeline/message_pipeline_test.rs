use peri_agent::messages::{BaseMessage, ContentBlock, MessageContent, ToolCallRequest};
use serde_json::json;
use std::time::{Duration, Instant};

use super::*;
use crate::ui::message_view::aggregate_batch_groups;
use crate::ui::message_view::ContentBlockView;

fn _normalize_vms(vms: Vec<MessageViewModel>) -> Vec<String> {
    vms.iter().map(|vm| format!("{:?}", vm)).collect()
}

/// 测试：流式路径和恢复路径对简单文本回复产生一致的输出
#[test]
fn test_streaming_vs_restore_text_only() {
    let cwd = "/Users/test/project";

    // 恢复路径
    let msgs = vec![BaseMessage::human("hello"), BaseMessage::ai("world")];
    let restore_vms = MessagePipeline::messages_to_view_models(&msgs, cwd);

    // 流式路径：模拟事件序列
    let mut pipeline = MessagePipeline::new(cwd.to_string());
    pipeline.push_chunk("world");
    pipeline.done();
    // 模拟 StateSnapshot 填充 completed
    pipeline.set_completed(vec![BaseMessage::ai("world")]);
    let stream_vms = pipeline.reconcile();

    // 比较非系统消息
    assert_eq!(restore_vms.len(), 2);
    assert_eq!(stream_vms.len(), 1); // 流式路径没有用户消息（由 handle_agent_event 添加）
}

/// 测试：工具调用的 cwd 一致性（核心修复验证）
#[test]
fn test_tool_args_cwd_consistency() {
    let cwd = "/Users/test/project";

    // 模拟恢复路径：Tool 消息从 BaseMessage 转换
    // Ai 消息带文本 + tool_calls，确保不会被过滤
    let msgs = vec![
        BaseMessage::human("read file"),
        BaseMessage::ai_with_tool_calls(
            MessageContent::text("I'll read the file"),
            vec![ToolCallRequest::new(
                "tc1",
                "Read",
                json!({"file_path": "/Users/test/project/src/main.rs"}),
            )],
        ),
        BaseMessage::Tool {
            id: peri_agent::messages::MessageId::new(),
            tool_call_id: "tc1".to_string(),
            content: MessageContent::text("file content here"),
            is_error: false,
        },
    ];
    let restore_vms = MessagePipeline::messages_to_view_models(&msgs, cwd);

    // 找到 ToolBlock 或 ToolCallGroup
    let tool_vm = restore_vms.iter().find(|vm| {
        matches!(vm, MessageViewModel::ToolBlock { .. })
            || matches!(vm, MessageViewModel::ToolCallGroup { .. })
    });
    assert!(
        tool_vm.is_some(),
        "应有 ToolBlock/ToolCallGroup，实际 VMs: {:?}",
        restore_vms
    );

    if let Some(MessageViewModel::ToolBlock { args_display, .. }) = tool_vm {
        // 应该显示相对路径而非绝对路径
        assert!(args_display.is_some(), "args_display 应有值");
        let args = args_display.as_ref().unwrap();
        assert!(
            args.contains("src/main.rs"),
            "应显示相对路径，实际: {}",
            args
        );
        assert!(
            !args.contains("/Users/test/project"),
            "不应包含 cwd 前缀，实际: {}",
            args
        );
    }
}

/// 测试：恢复路径的 cwd=None 仍能正常工作（向后兼容）
#[test]
fn test_restore_without_cwd() {
    let msgs = vec![BaseMessage::human("hello"), BaseMessage::ai("hi")];
    // cwd=None → fallback 行为
    let vms = MessagePipeline::messages_to_view_models(&msgs, "");
    assert_eq!(vms.len(), 2);
}

/// 测试：流式 pipeline 的 finalize 清理流式缓冲（completed 由 StateSnapshot 填充）
#[test]
fn test_pipeline_finalize_clears_buffers() {
    let mut pipeline = MessagePipeline::new("/tmp".to_string());
    pipeline.push_reasoning("thinking...");
    pipeline.push_chunk("Hello world");
    pipeline.done();

    // finalize 不再 push 到 completed（StateSnapshot 是唯一数据源）
    assert!(pipeline.completed_messages().is_empty());
    // done() 不再清空流式缓冲（set_completed 到达时才清空），
    // 但 current_ai_finalized 被重置为 false，所以流式状态仍然存在
    assert!(pipeline.has_streaming_content());
    // set_completed 到达后才清空流式缓冲
    pipeline.set_completed(vec![
        BaseMessage::human("hi"),
        BaseMessage::ai("Hello world"),
    ]);
    assert!(!pipeline.has_streaming_content());
}

/// 测试：set_completed 是 completed 的唯一数据源
#[test]
fn test_pipeline_set_completed_single_source() {
    let mut pipeline = MessagePipeline::new("/tmp".to_string());
    let msgs = vec![BaseMessage::human("hello"), BaseMessage::ai("world")];
    pipeline.set_completed(msgs.clone());

    assert_eq!(pipeline.completed_messages().len(), 2);
}

/// 测试：tool_start/tool_end 不直接写入 completed
#[test]
fn test_pipeline_tool_end_no_duplicate() {
    let mut pipeline = MessagePipeline::new("/tmp".to_string());
    let _ = pipeline.handle_event(AgentEvent::ToolStart {
        tool_call_id: "tc1".into(),
        name: "Read".into(),
        display: "ReadFile".into(),
        args: "test.txt".into(),
        input: json!({"file_path": "/tmp/test.txt"}),
        source_agent_id: None,
    });
    let _ = pipeline.handle_event(AgentEvent::ToolEnd {
        tool_call_id: "tc1".into(),
        name: "Read".into(),
        output: "content here".into(),
        is_error: false,
        source_agent_id: None,
    });

    // tool_end 不 push 到 completed
    assert!(pipeline.completed_messages().is_empty());

    // 模拟 StateSnapshot 填充
    let snapshot = vec![
        BaseMessage::ai_with_tool_calls(
            MessageContent::text("reading"),
            vec![ToolCallRequest::new(
                "tc1",
                "Read",
                json!({"file_path": "/tmp/test.txt"}),
            )],
        ),
        BaseMessage::Tool {
            id: peri_agent::messages::MessageId::new(),
            tool_call_id: "tc1".to_string(),
            content: MessageContent::text("content here"),
            is_error: false,
        },
    ];
    pipeline.set_completed(snapshot);
    assert_eq!(
        pipeline.completed_messages().len(),
        2,
        "StateSnapshot 应无重复地填充 completed"
    );
}

/// 测试：from_base_message_with_cwd 与 from_base_message 向后兼容
#[test]
fn test_from_base_message_backward_compat() {
    let msg = BaseMessage::ai("hello");
    let vm1 = MessageViewModel::from_base_message(&msg, &[]);
    let vm2 = MessageViewModel::from_base_message_with_cwd(&msg, &[], None);
    // 两者应产生相同结果
    assert_eq!(format!("{:?}", vm1), format!("{:?}", vm2));
}

// ─── handle_event 测试 ─────────────────────────────────────────────────

/// 测试：handle_event AssistantChunk 更新内部状态并通过策略跟踪积压
#[test]
fn test_handle_event_assistant_chunk() {
    let mut pipeline = MessagePipeline::new("/tmp".to_string());
    let actions = pipeline.handle_event(AgentEvent::AssistantChunk {
        chunk: "hello".into(),
        source_agent_id: None,
    });
    assert_eq!(actions.len(), 1);
    assert!(matches!(actions[0], PipelineAction::None));
    assert_eq!(pipeline.current_ai_text, "hello");
    assert!(
        pipeline.adaptive_policy.pending_lines > 0,
        "AssistantChunk 应通过策略跟踪积压"
    );
}

/// 测试：handle_event 空 chunk 不产生 AppendChunk
#[test]
fn test_handle_event_empty_chunk() {
    let mut pipeline = MessagePipeline::new("/tmp".to_string());
    let actions = pipeline.handle_event(AgentEvent::AssistantChunk {
        chunk: String::new(),
        source_agent_id: None,
    });
    assert_eq!(actions.len(), 1);
    assert!(matches!(actions[0], PipelineAction::None));
}

/// 测试：handle_event ToolStart + ToolEnd + Done 更新内部状态（所有返回 None）
#[test]
fn test_handle_event_tool_lifecycle() {
    let mut pipeline = MessagePipeline::new("/tmp".to_string());
    // ToolStart → None，但内部状态更新
    let actions = pipeline.handle_event(AgentEvent::ToolStart {
        tool_call_id: "tc1".into(),
        name: "Read".into(),
        display: "ReadFile".into(),
        args: "src/main.rs".into(),
        input: serde_json::json!({"file_path": "/tmp/src/main.rs"}),
        source_agent_id: None,
    });
    assert_eq!(actions.len(), 1);
    assert!(matches!(actions[0], PipelineAction::None));
    assert!(
        pipeline.pending_tools.contains_key("tc1"),
        "ToolStart 后 pending_tools 应包含 tc1"
    );
    // ToolEnd → None，pending_tools 移除
    let actions = pipeline.handle_event(AgentEvent::ToolEnd {
        tool_call_id: "tc1".into(),
        name: "Read".into(),
        output: "file content".into(),
        is_error: false,
        source_agent_id: None,
    });
    assert_eq!(actions.len(), 1);
    assert!(matches!(actions[0], PipelineAction::None));
    assert!(
        !pipeline.pending_tools.contains_key("tc1"),
        "ToolEnd 后 pending_tools 应不包含 tc1"
    );
    // Done → None
    let actions = pipeline.handle_event(AgentEvent::Done);
    assert_eq!(actions.len(), 1);
    assert!(matches!(actions[0], PipelineAction::None));
}

/// 测试：handle_event StateSnapshot 更新 completed
#[test]
fn test_handle_event_state_snapshot() {
    let mut pipeline = MessagePipeline::new("/tmp".to_string());
    let msgs = vec![BaseMessage::human("hello"), BaseMessage::ai("world")];
    let actions = pipeline.handle_event(AgentEvent::StateSnapshot(msgs.clone()));
    assert_eq!(actions.len(), 1);
    assert!(matches!(actions[0], PipelineAction::None));
    assert_eq!(pipeline.completed_messages().len(), 2);
}

/// 测试：SubAgent 内部并行相同工具的 tool_call_id 精确匹配
#[test]
fn test_subagent_parallel_same_tool_matches_by_call_id() {
    let mut pipeline = MessagePipeline::new("/tmp".to_string());

    // 启动 SubAgent
    let _ = pipeline.handle_event(AgentEvent::SubAgentStart {
        agent_id: "test-agent".into(),
        instance_id: "test-instance".into(),
        task_preview: "parallel reads".into(),
        is_background: false,
    });

    // SubAgent 内部并行启动两个 read_file（带 source_agent_id）
    let _ = pipeline.handle_event(AgentEvent::ToolStart {
        tool_call_id: "tc_a".into(),
        name: "Read".into(),
        display: "ReadFile".into(),
        args: "a.rs".into(),
        input: serde_json::json!({"file_path": "/tmp/a.rs"}),
        source_agent_id: Some("test-instance".into()),
    });
    let _ = pipeline.handle_event(AgentEvent::ToolStart {
        tool_call_id: "tc_b".into(),
        name: "Read".into(),
        display: "ReadFile".into(),
        args: "b.rs".into(),
        input: serde_json::json!({"file_path": "/tmp/b.rs"}),
        source_agent_id: Some("test-instance".into()),
    });

    // ToolEnd 按不同顺序到达（tc_b 先完成）
    let _ = pipeline.handle_event(AgentEvent::ToolEnd {
        tool_call_id: "tc_b".into(),
        name: "Read".into(),
        output: "content of b".into(),
        is_error: false,
        source_agent_id: Some("test-instance".into()),
    });
    let _ = pipeline.handle_event(AgentEvent::ToolEnd {
        tool_call_id: "tc_a".into(),
        name: "Read".into(),
        output: "content of a".into(),
        is_error: false,
        source_agent_id: Some("test-instance".into()),
    });

    // 验证 recent_messages 中两个 ToolBlock 被正确更新
    let sub = pipeline.subagent_stack.last().unwrap();
    assert_eq!(sub.recent_messages.len(), 2);

    // 找到 tc_a 和 tc_b 对应的 ToolBlock
    let mut found_a = false;
    let mut found_b = false;
    for vm in &sub.recent_messages {
        if let MessageViewModel::ToolBlock {
            tool_call_id,
            content,
            ..
        } = vm
        {
            match tool_call_id.as_str() {
                "tc_a" => {
                    assert_eq!(content, "content of a", "tc_a 应匹配自己的结果");
                    found_a = true;
                }
                "tc_b" => {
                    assert_eq!(content, "content of b", "tc_b 应匹配自己的结果");
                    found_b = true;
                }
                _ => {}
            }
        }
    }
    assert!(found_a, "应找到 tc_a 的 ToolBlock");
    assert!(found_b, "应找到 tc_b 的 ToolBlock");
}

// ─── 并发 SubAgent 路由测试 ──────────────────────────────────────────────

/// 测试：并发 SubAgent 通过 source_agent_id 精确路由工具调用事件
/// 验证 ToolStart/ToolEnd/AssistantChunk 事件路由到正确的 SubAgentGroup
#[test]
fn test_concurrent_subagents_route_by_source_agent_id() {
    let mut pipeline = MessagePipeline::new("/tmp".to_string());

    // 启动两个并发 SubAgent
    let _ = pipeline.handle_event(AgentEvent::SubAgentStart {
        agent_id: "agent-a".into(),
        instance_id: "inst-a".into(),
        task_preview: "task a".into(),
        is_background: false,
    });
    let _ = pipeline.handle_event(AgentEvent::SubAgentStart {
        agent_id: "agent-b".into(),
        instance_id: "inst-b".into(),
        task_preview: "task b".into(),
        is_background: false,
    });

    // Agent A 的 ToolStart → source_agent_id = instance_id
    let _ = pipeline.handle_event(AgentEvent::ToolStart {
        tool_call_id: "tc_a1".into(),
        name: "Read".into(),
        display: "Read".into(),
        args: "a.rs".into(),
        input: json!({"file_path": "/tmp/a.rs"}),
        source_agent_id: Some("inst-a".into()),
    });
    // Agent B 的 ToolStart → source_agent_id = instance_id
    let _ = pipeline.handle_event(AgentEvent::ToolStart {
        tool_call_id: "tc_b1".into(),
        name: "Grep".into(),
        display: "Grep".into(),
        args: "pattern".into(),
        input: json!({"pattern": "fn main"}),
        source_agent_id: Some("inst-b".into()),
    });

    // 验证：agent-a 的 recent_messages 有 Read，agent-b 有 Grep
    let sub_a = pipeline
        .subagent_stack
        .iter()
        .find(|s| s.agent_id == "agent-a")
        .unwrap();
    assert_eq!(sub_a.recent_messages.len(), 1);
    if let MessageViewModel::ToolBlock { tool_name, .. } = &sub_a.recent_messages[0] {
        assert_eq!(tool_name, "Read", "agent-a 应包含 Read 工具调用");
    } else {
        panic!("agent-a 的 recent_messages[0] 应为 ToolBlock");
    }

    let sub_b = pipeline
        .subagent_stack
        .iter()
        .find(|s| s.agent_id == "agent-b")
        .unwrap();
    assert_eq!(sub_b.recent_messages.len(), 1);
    if let MessageViewModel::ToolBlock { tool_name, .. } = &sub_b.recent_messages[0] {
        assert_eq!(tool_name, "Grep", "agent-b 应包含 Grep 工具调用");
    } else {
        panic!("agent-b 的 recent_messages[0] 应为 ToolBlock");
    }

    // Agent A 的 ToolEnd
    let _ = pipeline.handle_event(AgentEvent::ToolEnd {
        tool_call_id: "tc_a1".into(),
        name: "Read".into(),
        output: "content of a".into(),
        is_error: false,
        source_agent_id: Some("inst-a".into()),
    });
    // Agent B 的 ToolEnd
    let _ = pipeline.handle_event(AgentEvent::ToolEnd {
        tool_call_id: "tc_b1".into(),
        name: "Grep".into(),
        output: "match in b".into(),
        is_error: false,
        source_agent_id: Some("inst-b".into()),
    });

    // 验证 ToolEnd 结果路由到正确的 SubAgent
    let sub_a = pipeline
        .subagent_stack
        .iter()
        .find(|s| s.agent_id == "agent-a")
        .unwrap();
    if let MessageViewModel::ToolBlock {
        tool_call_id,
        content,
        ..
    } = &sub_a.recent_messages[0]
    {
        assert_eq!(tool_call_id, "tc_a1");
        assert_eq!(
            content, "content of a",
            "agent-a 的 ToolEnd 应路由到 agent-a"
        );
    }

    let sub_b = pipeline
        .subagent_stack
        .iter()
        .find(|s| s.agent_id == "agent-b")
        .unwrap();
    if let MessageViewModel::ToolBlock {
        tool_call_id,
        content,
        ..
    } = &sub_b.recent_messages[0]
    {
        assert_eq!(tool_call_id, "tc_b1");
        assert_eq!(content, "match in b", "agent-b 的 ToolEnd 应路由到 agent-b");
    }

    // AssistantChunk 也应精确路由
    let _ = pipeline.handle_event(AgentEvent::AssistantChunk {
        chunk: "chunk for a".into(),
        source_agent_id: Some("inst-a".into()),
    });
    let _ = pipeline.handle_event(AgentEvent::AssistantChunk {
        chunk: "chunk for b".into(),
        source_agent_id: Some("inst-b".into()),
    });

    let sub_a = pipeline
        .subagent_stack
        .iter()
        .find(|s| s.instance_id == "inst-a")
        .unwrap();
    // agent-a 有 ToolBlock + AssistantBubble = 2 个
    assert_eq!(sub_a.recent_messages.len(), 2);

    let sub_b = pipeline
        .subagent_stack
        .iter()
        .find(|s| s.instance_id == "inst-b")
        .unwrap();
    assert_eq!(sub_b.recent_messages.len(), 2);

    // SubAgentEnd 带 instance_id 精确匹配
    let _ = pipeline.handle_event(AgentEvent::SubAgentEnd {
        agent_id: Some("agent-a".into()),
        instance_id: Some("inst-a".into()),
        result: "done a".into(),
        is_error: false,
    });
    let _ = pipeline.handle_event(AgentEvent::SubAgentEnd {
        agent_id: Some("agent-b".into()),
        instance_id: Some("inst-b".into()),
        result: "done b".into(),
        is_error: false,
    });

    // 验证两个 SubAgent 都被冻结
    assert_eq!(
        pipeline.frozen_subagent_vms_count(),
        2,
        "两个 SubAgent 都应被冻结"
    );
}

/// 测试：source_agent_id 为 None 时回退到 last_mut()（向后兼容）
#[test]
fn test_subagent_source_agent_id_none_fallback_to_last_mut() {
    let mut pipeline = MessagePipeline::new("/tmp".to_string());

    // 启动单个 SubAgent（无 source_agent_id 时 last_mut() 回退生效）
    let _ = pipeline.handle_event(AgentEvent::SubAgentStart {
        agent_id: "legacy-agent".into(),
        instance_id: "test-instance".into(),
        task_preview: "legacy task".into(),
        is_background: false,
    });
    // ToolStart 无 source_agent_id → last_mut() 回退到唯一的 SubAgent
    let _ = pipeline.handle_event(AgentEvent::ToolStart {
        tool_call_id: "tc_legacy".into(),
        name: "Bash".into(),
        display: "Bash".into(),
        args: "ls".into(),
        input: json!({"command": "ls"}),
        source_agent_id: None,
    });

    let sub = pipeline.subagent_stack.last().unwrap();
    assert_eq!(
        sub.recent_messages.len(),
        1,
        "source_agent_id=None 通过 last_mut() 回退路由到 SubAgent"
    );
    if let MessageViewModel::ToolBlock { tool_name, .. } = &sub.recent_messages[0] {
        assert_eq!(tool_name, "Bash");
    }
}

// ─── build_tail_vms 测试 ──────────────────────────────────────────────────

/// 场景1: has_snapshot=true, completed 有消息 → 从最后一条 Human 开始 reconcile
#[test]
fn test_build_tail_vms_with_snapshot() {
    let mut pipeline = MessagePipeline::new("/tmp".to_string());
    pipeline.completed = vec![
        BaseMessage::human("q1"),
        BaseMessage::ai("a1"),
        BaseMessage::human("q2"),
        BaseMessage::ai("a2"),
    ];
    pipeline.has_snapshot_this_round = true;
    pipeline.completed_len_at_round_start = 0;

    let tail_vms = pipeline.build_tail_vms();
    // 不再用 rposition 截断——从 start 渲染全部消息
    let expected =
        MessagePipeline::messages_to_view_models(&pipeline.completed[0..], &pipeline.cwd);
    assert_eq!(format!("{:?}", tail_vms), format!("{:?}", expected));
}

/// 场景2: has_snapshot=false（Case 1）→ 跳过 reconcile，只输出 streaming + pending tools
#[test]
fn test_build_tail_vms_case1_no_snapshot() {
    let mut pipeline = MessagePipeline::new("/tmp".to_string());
    pipeline.completed = vec![BaseMessage::human("old q"), BaseMessage::ai("old a")];
    pipeline.has_snapshot_this_round = false;
    pipeline.completed_len_at_round_start = 2;

    // 有流式内容
    pipeline.push_chunk("streaming text");

    let tail_vms = pipeline.build_tail_vms();
    // Case 1 不应包含 old q / old a
    assert!(
        tail_vms.iter().all(
            |vm| !matches!(vm, MessageViewModel::UserBubble { content, .. } if content == "old q")
        ),
        "Case 1 不应包含上一轮消息"
    );
    // 应包含 streaming bubble
    assert!(
        tail_vms.iter().any(|vm| matches!(
            vm,
            MessageViewModel::AssistantBubble {
                is_streaming: true,
                ..
            }
        )),
        "Case 1 应包含 streaming bubble"
    );
}

/// goal steering 注入的 <goal-message> Human 消息不应跳过前面的 AI 回复。
///
/// 场景：用户发消息 → AI 回复 "1" → GoalMiddleware 注入 steering → AI 回复 "2"
/// 修复前（rposition 不过滤注入消息）：tail_vms 从 steering 开始 → AI "1" 丢失
/// 修复后（rposition 过滤 < 开头的注入消息）：tail_vms 从用户消息开始 → 完整渲染
#[test]
fn test_build_tail_vms_goal_steering_injected_skipped() {
    let mut pipeline = MessagePipeline::new("/tmp".to_string());
    pipeline.completed = vec![
        BaseMessage::human("count to 3"),
        BaseMessage::ai("1"),
        BaseMessage::human("<goal-message>\n[Goal Steering]\n目标: 数到 3\n</goal-message>"),
        BaseMessage::ai("2\n3"),
    ];
    pipeline.has_snapshot_this_round = true;
    pipeline.completed_len_at_round_start = 0;

    let tail_vms = pipeline.build_tail_vms();

    // tail_vms 应包含完整对话：user → AI "1" → steering → AI "2,3"
    let has_ai_1 = tail_vms.iter().any(|vm| match vm {
        MessageViewModel::AssistantBubble { blocks, .. } => blocks
            .iter()
            .any(|b| matches!(b, ContentBlockView::Text { raw, .. } if raw.contains("1"))),
        _ => false,
    });
    assert!(
        has_ai_1,
        "tail_vms 应包含 AI 回复 '1'，实际: {:?}",
        tail_vms
    );

    // 也应包含 steering 消息
    let has_steering = tail_vms.iter().any(|vm| {
        matches!(vm,
            MessageViewModel::UserBubble { content, .. }
            if content.contains("Goal Steering")
        )
    });
    assert!(has_steering, "tail_vms 应包含 steering 消息");
}

/// 场景3: 空 completed + 无 streaming → 空 tail
#[test]
fn test_build_tail_vms_empty() {
    let pipeline = MessagePipeline::new("/tmp".to_string());
    let tail_vms = pipeline.build_tail_vms();
    assert!(tail_vms.is_empty());
}

/// 场景：AssistantChunk → ToolStart 后，build_tail_vms 应包含 AI 文本 + ToolBlock
#[test]
fn test_build_tail_vms_text_visible_with_pending_tool() {
    let mut pipeline = MessagePipeline::new("/tmp".to_string());

    // 模拟真实事件流：AI 先输出文本，再调用工具
    pipeline.handle_event(AgentEvent::AssistantChunk {
        chunk: "I'll read the file".into(),
        source_agent_id: None,
    });
    pipeline.handle_event(AgentEvent::ToolStart {
        tool_call_id: "tc1".into(),
        name: "Read".into(),
        display: "ReadFile".into(),
        args: "src/main.rs".into(),
        input: json!({"file_path": "/tmp/src/main.rs"}),
        source_agent_id: None,
    });

    let tail_vms = pipeline.build_tail_vms();

    // 应包含 streaming bubble 且有文本内容
    let has_text = tail_vms.iter().any(|vm| {
        if let MessageViewModel::AssistantBubble { blocks, .. } = vm {
            blocks.iter().any(
                |b| matches!(b, ContentBlockView::Text { raw, .. } if raw.contains("I'll read")),
            )
        } else {
            false
        }
    });
    assert!(
        has_text,
        "ToolStart 后 streaming bubble 应包含 AI 文本，实际 VMs: {:?}",
        tail_vms
    );

    // Read 工具被 aggregate_tool_groups 折叠为 ToolCallGroup
    let has_tool = tail_vms.iter().any(|vm| {
            matches!(
                vm,
                MessageViewModel::ToolCallGroup { tools, .. } if tools.iter().any(|t| t.tool_name == "Read")
            )
        });
    assert!(
        has_tool,
        "ToolStart 后应有 ToolCallGroup(Read)，实际 VMs: {:?}",
        tail_vms
    );
}

/// 端到端：多轮工具调用中 AI 文本可见性
/// Chunk → ToolStart → ToolEnd → StateSnapshot → Chunk → ToolStart → Done
#[test]
fn test_e2e_text_visible_between_tool_calls() {
    use peri_agent::messages::{MessageContent, MessageId, ToolCallRequest};

    let mut pipeline = MessagePipeline::new("/tmp".to_string());
    pipeline.begin_round();

    // 1. AI 输出文本
    pipeline.handle_event(AgentEvent::AssistantChunk {
        chunk: "Let me check the file".into(),
        source_agent_id: None,
    });
    let tail1 = pipeline.build_tail_vms();
    assert!(has_text(&tail1, "Let me check"), "步骤1: chunk 后应有文本");

    // 2. ToolStart
    pipeline.handle_event(AgentEvent::ToolStart {
        tool_call_id: "tc1".into(),
        name: "Read".into(),
        display: "ReadFile".into(),
        args: "main.rs".into(),
        input: json!({"path": "/tmp/main.rs"}),
        source_agent_id: None,
    });
    let tail2 = pipeline.build_tail_vms();
    assert!(
        has_text(&tail2, "Let me check"),
        "步骤2: ToolStart 后文本应保留"
    );

    // 3. ToolEnd
    pipeline.handle_event(AgentEvent::ToolEnd {
        tool_call_id: "tc1".into(),
        name: "Read".into(),
        output: "fn main() {}".into(),
        is_error: false,
        source_agent_id: None,
    });
    let tail3 = pipeline.build_tail_vms();
    assert!(
        has_text(&tail3, "Let me check"),
        "步骤3: ToolEnd 后文本应保留"
    );

    // 4. StateSnapshot（清空流式缓冲，切换到 reconcile 路径）
    pipeline.set_completed(vec![
        BaseMessage::human("read file"),
        BaseMessage::ai_with_tool_calls(
            MessageContent::text("Let me check the file"),
            vec![ToolCallRequest::new(
                "tc1",
                "Read",
                json!({"path": "/tmp/main.rs"}),
            )],
        ),
        BaseMessage::Tool {
            id: MessageId::new(),
            tool_call_id: "tc1".to_string(),
            content: MessageContent::text("fn main() {}"),
            is_error: false,
        },
    ]);
    let tail4 = pipeline.build_tail_vms();
    assert!(
        has_text(&tail4, "Let me check"),
        "步骤4: StateSnapshot 后 reconcile 应包含文本, VMs: {:?}",
        tail4
    );

    // 5. 新的 AI 文本（工具之间）
    pipeline.handle_event(AgentEvent::AssistantChunk {
        chunk: "Now let me write tests".into(),
        source_agent_id: None,
    });
    let tail5 = pipeline.build_tail_vms();
    assert!(
        has_text(&tail5, "Now let me write tests"),
        "步骤5: 新 chunk 后应有新文本"
    );
    assert!(
        has_text(&tail5, "Let me check"),
        "步骤5: 旧文本也应保留（reconcile）"
    );

    // 6. 第二个 ToolStart
    pipeline.handle_event(AgentEvent::ToolStart {
        tool_call_id: "tc2".into(),
        name: "Write".into(),
        display: "WriteFile".into(),
        args: "test.rs".into(),
        input: json!({"path": "/tmp/test.rs"}),
        source_agent_id: None,
    });
    let tail6 = pipeline.build_tail_vms();
    assert!(
        has_text(&tail6, "Now let me write tests"),
        "步骤6: 第二个 ToolStart 后新文本应保留"
    );
    assert!(
        has_text(&tail6, "Let me check"),
        "步骤6: 旧文本也应保留（reconcile）"
    );
}

fn has_text(vms: &[MessageViewModel], text: &str) -> bool {
    vms.iter().any(|vm| {
        if let MessageViewModel::AssistantBubble { blocks, .. } = vm {
            blocks
                .iter()
                .any(|b| matches!(b, ContentBlockView::Text { raw, .. } if raw.contains(text)))
        } else {
            false
        }
    })
}

/// 验证尾部重建与全量转换一致性
#[test]
fn test_build_tail_vms_consistency() {
    let mut pipeline = MessagePipeline::new("/tmp".to_string());
    pipeline.restore_completed(vec![
        BaseMessage::human("q1"),
        BaseMessage::ai("a1"),
        BaseMessage::human("q2"),
        BaseMessage::ai("a2"),
    ]);
    pipeline.has_snapshot_this_round = true;
    pipeline.completed_len_at_round_start = 0;

    let tail_vms = pipeline.build_tail_vms();

    // tail_vms 应等于从 start（completed_len_at_round_start）开始重建的全部 VMs
    let expected_tail = MessagePipeline::messages_to_view_models(
        &pipeline.completed_messages()[0..],
        &pipeline.cwd,
    );

    assert_eq!(format!("{:?}", tail_vms), format!("{:?}", expected_tail));
}

/// 验证工具调用场景的尾部重建
#[test]
fn test_build_tail_vms_with_tools() {
    let mut pipeline = MessagePipeline::new("/tmp".to_string());
    pipeline.restore_completed(vec![
        BaseMessage::human("read file"),
        BaseMessage::ai_from_blocks(vec![ContentBlock::ToolUse {
            id: "tc1".to_string(),
            name: "Read".to_string(),
            input: serde_json::json!({"file_path": "/tmp/test.rs"}),
        }]),
        BaseMessage::tool_result("tc1", "file content here"),
    ]);
    pipeline.has_snapshot_this_round = true;
    pipeline.completed_len_at_round_start = 0;

    let tail_vms = pipeline.build_tail_vms();

    // 全量转换对比
    let full_vms =
        MessagePipeline::messages_to_view_models(pipeline.completed_messages(), &pipeline.cwd);

    assert_eq!(format!("{:?}", tail_vms), format!("{:?}", full_vms));
}

/// 验证 pending tools 在 build_tail_vms 中生成 ToolBlock VMs
#[test]
fn test_build_tail_vms_with_pending_tools() {
    let mut pipeline = MessagePipeline::new("/tmp".to_string());
    pipeline.has_snapshot_this_round = true;
    pipeline.completed_len_at_round_start = 0;
    pipeline.completed = vec![BaseMessage::human("hi")];

    // 模拟 ToolStart（通过 handle_event）
    let _ = pipeline.handle_event(AgentEvent::ToolStart {
        tool_call_id: "tc1".into(),
        name: "Read".into(),
        display: "ReadFile".into(),
        args: "src/main.rs".into(),
        input: serde_json::json!({"file_path": "/tmp/test.rs"}),
        source_agent_id: None,
    });

    let tail_vms = pipeline.build_tail_vms();
    // 应包含 UserBubble + pending ToolBlock（Read 可能被聚合为 ToolCallGroup）
    let has_tool = tail_vms.iter().any(|vm| match vm {
        MessageViewModel::ToolBlock { tool_name, .. } => tool_name == "Read",
        MessageViewModel::ToolCallGroup { tools, .. } => {
            tools.iter().any(|t| t.tool_name == "Read")
        }
        _ => false,
    });
    assert!(
        has_tool,
        "build_tail_vms 应包含 pending tool 的 ToolBlock 或 ToolCallGroup"
    );
}

/// 验证 set_completed 清除 pending_tools
#[test]
fn test_set_completed_clears_pending_tools() {
    let mut pipeline = MessagePipeline::new("/tmp".to_string());
    let _ = pipeline.handle_event(AgentEvent::ToolStart {
        tool_call_id: "tc1".into(),
        name: "Read".into(),
        display: "ReadFile".into(),
        args: "src/main.rs".into(),
        input: serde_json::json!({"file_path": "/tmp/test.rs"}),
        source_agent_id: None,
    });
    assert!(pipeline.pending_tools.contains_key("tc1"));

    pipeline.set_completed(vec![BaseMessage::human("hi"), BaseMessage::ai("result")]);
    assert!(
        !pipeline.pending_tools.contains_key("tc1"),
        "set_completed 应清除 pending_tools"
    );
    assert!(pipeline.has_snapshot_this_round);
}

/// 验证 Interrupted 后 build_tail_vms 产生一致结果（可用于后续 RebuildAll）
///
/// 场景：agent 回复了文本后被中断，Interrupted 处理器调用 build_rebuild_all
/// 然后 Done 到达，如果重复 build_rebuild_all 并 RebuildAll，会覆盖 Interrupted 添加的通知消息。
#[test]
fn test_build_tail_vms_interrupted_then_done_consistency() {
    let mut pipeline = MessagePipeline::new("/tmp".to_string());
    pipeline.has_snapshot_this_round = true;
    pipeline.completed_len_at_round_start = 0;

    // 模拟流式：用户发送消息，agent 回复了文本，然后开始工具调用
    pipeline.push_chunk("I'll read the file");
    let _ = pipeline.handle_event(AgentEvent::ToolStart {
        tool_call_id: "tc1".into(),
        name: "Read".into(),
        display: "ReadFile".into(),
        args: "src/main.rs".into(),
        input: serde_json::json!({"file_path": "/tmp/test.rs"}),
        source_agent_id: None,
    });
    let _ = pipeline.handle_event(AgentEvent::ToolEnd {
        tool_call_id: "tc1".into(),
        name: "Read".into(),
        output: "file content here".into(),
        is_error: false,
        source_agent_id: None,
    });

    // 模拟 StateSnapshot 填充 completed
    pipeline.set_completed(vec![
        BaseMessage::human("read file"),
        BaseMessage::ai_from_blocks(vec![
            ContentBlock::text("I'll read the file"),
            ContentBlock::tool_use("tc1", "Read", json!({"file_path": "/tmp/test.rs"})),
        ]),
        BaseMessage::tool_result("tc1", "file content here"),
    ]);

    // Interrupted 处理器调用 build_rebuild_all
    let action1 = pipeline.build_rebuild_all(0);
    if let PipelineAction::RebuildAll {
        prefix_len,
        tail_vms,
    } = action1
    {
        assert_eq!(prefix_len, 0);
        assert!(
            tail_vms.len() >= 3,
            "build_tail_vms 应包含 UserBubble + AssistantBubble + ToolBlock/Group"
        );

        // Done 到达时，再次 build_rebuild_all 应产生相同结果
        let action2 = pipeline.build_rebuild_all(0);
        if let PipelineAction::RebuildAll {
            prefix_len: p2,
            tail_vms: tail_vms2,
        } = action2
        {
            assert_eq!(prefix_len, p2);
            assert_eq!(tail_vms.len(), tail_vms2.len());
            for (a, b) in tail_vms.iter().zip(tail_vms2.iter()) {
                assert_eq!(a, b, "两次 build_rebuild_all 结果应一致");
            }
        } else {
            panic!("Expected RebuildAll");
        }
    } else {
        panic!("Expected RebuildAll");
    }
}

/// 验证 Done 后 pipeline.done() 是幂等的（不改变 build_tail_vms 结果）
#[test]
fn test_done_idempotent_build_tail_vms() {
    let mut pipeline = MessagePipeline::new("/tmp".to_string());
    pipeline.has_snapshot_this_round = true;
    pipeline.completed_len_at_round_start = 0;

    pipeline.push_chunk("Hello world");
    pipeline.set_completed(vec![
        BaseMessage::human("hi"),
        BaseMessage::ai("Hello world"),
    ]);

    // 第一次 done
    pipeline.done();
    let action1 = pipeline.build_rebuild_all(0);
    let tail_vms1 = match action1 {
        PipelineAction::RebuildAll { tail_vms, .. } => tail_vms,
        _ => panic!("Expected RebuildAll"),
    };

    // 第二次 done（模拟 Interrupted -> Done 双重调用）
    pipeline.done();
    let action2 = pipeline.build_rebuild_all(0);
    let tail_vms2 = match action2 {
        PipelineAction::RebuildAll { tail_vms, .. } => tail_vms,
        _ => panic!("Expected RebuildAll"),
    };

    assert_eq!(tail_vms1.len(), tail_vms2.len());
    for (a, b) in tail_vms1.iter().zip(tail_vms2.iter()) {
        assert_eq!(a, b, "多次 done 后 build_tail_vms 结果应一致");
    }
}

// ── 批次聚合集成测试 ──

/// 创建一个包含 N 个 Agent ToolUse/ToolResult 对的 BaseMessage 历史
fn make_agent_history(agent_count: usize, separated: bool) -> Vec<BaseMessage> {
    use peri_agent::messages::MessageId;
    let mut tool_calls: Vec<ToolCallRequest> = Vec::new();
    for i in 0..agent_count {
        tool_calls.push(ToolCallRequest::new(
            format!("tc_{}", i),
            "Agent",
            json!({"subagent_type": format!("agent-{}", i), "prompt": format!("task {}", i)}),
        ));
    }
    let mut msgs = vec![
        BaseMessage::human("dispatch agents"),
        BaseMessage::ai_with_tool_calls(MessageContent::text(""), tool_calls.clone()),
    ];
    for i in 0..agent_count {
        msgs.push(BaseMessage::Tool {
            id: MessageId::new(),
            tool_call_id: format!("tc_{}", i),
            content: MessageContent::text(format!(
                "[Sub-agent executed {} tool calls: done{}]",
                i + 1,
                i
            )),
            is_error: false,
        });
    }
    // 在两组 agent 之间插入非 Agent 消息（测试批次分离）
    if separated {
        msgs.push(BaseMessage::human("continue"));
        let tool_calls2 = vec![ToolCallRequest::new(
            "tc_sep".to_string(),
            "Agent",
            json!({"subagent_type": "agent-sep", "prompt": "separated task"}),
        )];
        msgs.push(BaseMessage::ai_with_tool_calls(
            MessageContent::text(""),
            tool_calls2,
        ));
        msgs.push(BaseMessage::Tool {
            id: MessageId::new(),
            tool_call_id: "tc_sep".to_string(),
            content: MessageContent::text("[Sub-agent executed 2 tool calls: done_sep]"),
            is_error: false,
        });
    }
    msgs
}

#[test]
fn test_batch_info_single_agent_no_batch() {
    let msgs = make_agent_history(1, false);
    let vms = MessagePipeline::messages_to_view_models(&msgs, "/tmp");
    // 单个 agent 不应聚合
    let subagent_vms: Vec<_> = vms
        .iter()
        .filter(|vm| matches!(vm, MessageViewModel::SubAgentGroup { .. }))
        .collect();
    assert_eq!(subagent_vms.len(), 1, "单个 agent 应保持独立");
    if let MessageViewModel::SubAgentGroup { batch_agents, .. } = subagent_vms[0] {
        assert!(batch_agents.is_empty(), "单个 agent 的 batch_agents 应为空");
    }
}

#[test]
fn test_batch_info_multi_agent_triggers() {
    let msgs = make_agent_history(3, false);
    let mut vms = MessagePipeline::messages_to_view_models(&msgs, "/tmp");
    aggregate_batch_groups(&mut vms);
    // 3 个连续 Agent Tool 结果应聚合为 1 个批次
    let batch_vms: Vec<_> = vms
        .iter()
        .filter(|vm| matches!(vm, MessageViewModel::SubAgentGroup { batch_agents, .. } if !batch_agents.is_empty()))
        .collect();
    assert_eq!(batch_vms.len(), 1, "3 个连续 agent 完成后应聚合为 1 个批次");
    if let MessageViewModel::SubAgentGroup { batch_agents, .. } = batch_vms[0] {
        assert_eq!(batch_agents.len(), 3);
    }
}

#[test]
fn test_batch_info_reset_on_done() {
    // 模拟 Done 后新起一批 agent
    let mut msgs = make_agent_history(2, false);
    // 模拟 Done（新的 Ai 回复分隔两批）
    msgs.push(BaseMessage::ai("I'll continue"));
    let more = make_agent_history(1, false);
    // 跳过 human 和 ai 消息，只追加 Tool 消息
    msgs.extend(more.iter().skip(2).cloned());
    let mut vms = MessagePipeline::messages_to_view_models(&msgs, "/tmp");
    aggregate_batch_groups(&mut vms);
    // Ai 消息分隔了两批 agent，不应跨批次聚合
    let batch_vms: Vec<_> = vms
        .iter()
        .filter(|vm| matches!(vm, MessageViewModel::SubAgentGroup { batch_agents, .. } if !batch_agents.is_empty()))
        .collect();
    // 第一批 2 个 agent 聚合为 1 个批次，第二批 1 个 agent 保持独立
    assert_eq!(
        batch_vms.len(),
        1,
        "Done 后新起的 agent 不应与前面合并，只有第一批聚合"
    );
    if let MessageViewModel::SubAgentGroup { batch_agents, .. } = batch_vms[0] {
        assert_eq!(batch_agents.len(), 2, "第一批应聚合 2 个 agent");
    }
}

#[test]
fn test_batch_info_reset_on_non_agent_tool() {
    // 非 Agent 工具打断连续的 Agent Tool 结果
    use peri_agent::messages::MessageId;
    let mut msgs = vec![
        BaseMessage::human("go"),
        BaseMessage::ai_with_tool_calls(
            MessageContent::text(""),
            vec![
                ToolCallRequest::new(
                    "tc1",
                    "Agent",
                    json!({"subagent_type": "a1", "prompt": "t1"}),
                ),
                ToolCallRequest::new("tc_bash", "Bash", json!({"command": "ls"})),
                ToolCallRequest::new(
                    "tc2",
                    "Agent",
                    json!({"subagent_type": "a2", "prompt": "t2"}),
                ),
            ],
        ),
    ];
    // Agent Tool 结果
    msgs.push(BaseMessage::Tool {
        id: MessageId::new(),
        tool_call_id: "tc1".to_string(),
        content: MessageContent::text("[Sub-agent executed 3 tool calls: done1]"),
        is_error: false,
    });
    // Bash Tool 结果（打断连续区间）
    msgs.push(BaseMessage::Tool {
        id: MessageId::new(),
        tool_call_id: "tc_bash".to_string(),
        content: MessageContent::text("file1.txt"),
        is_error: false,
    });
    // Agent Tool 结果
    msgs.push(BaseMessage::Tool {
        id: MessageId::new(),
        tool_call_id: "tc2".to_string(),
        content: MessageContent::text("[Sub-agent executed 5 tool calls: done2]"),
        is_error: false,
    });
    let mut vms = MessagePipeline::messages_to_view_models(&msgs, "/tmp");
    aggregate_batch_groups(&mut vms);
    let batch_vms: Vec<_> = vms
        .iter()
        .filter(|vm| matches!(vm, MessageViewModel::SubAgentGroup { batch_agents, .. } if !batch_agents.is_empty()))
        .collect();
    // Bash Tool 结果打断了连续区间，两个 Agent 各自独立
    assert_eq!(batch_vms.len(), 0, "非 Agent 工具打断后 agent 不应聚合");
}

#[test]
fn test_batch_info_different_batches_no_merge() {
    // 两组 agent 被用户消息分隔
    let msgs = make_agent_history(2, true);
    let mut vms = MessagePipeline::messages_to_view_models(&msgs, "/tmp");
    aggregate_batch_groups(&mut vms);
    let batch_vms: Vec<_> = vms
        .iter()
        .filter(|vm| matches!(vm, MessageViewModel::SubAgentGroup { batch_agents, .. } if !batch_agents.is_empty()))
        .collect();
    // 第一批 2 个 agent 聚合为 1 个批次，第二批 1 个 agent 保持独立
    assert_eq!(batch_vms.len(), 1, "用户消息分隔后只有第一批聚合");
    if let MessageViewModel::SubAgentGroup { batch_agents, .. } = batch_vms[0] {
        assert_eq!(batch_agents.len(), 2, "第一批应聚合 2 个 agent");
    }
}

#[test]
fn test_build_tail_vms_aggregate_after_all_done() {
    // 通过 reconcile 路径验证聚合（reconcile 内部调用 messages_to_view_models）
    let msgs = make_agent_history(3, false);
    let mut vms = MessagePipeline::messages_to_view_models(&msgs, "/tmp");
    aggregate_batch_groups(&mut vms);
    let batch_vms: Vec<_> = vms
        .iter()
        .filter(|vm| matches!(vm, MessageViewModel::SubAgentGroup { batch_agents, .. } if !batch_agents.is_empty()))
        .collect();
    assert_eq!(batch_vms.len(), 1, "reconcile 路径应触发聚合");
}

/// 测试 extract_tail_lines：基本提取
#[test]
fn test_extract_tail_lines_basic() {
    let text = "line1\nline2\nline3\nline4\nline5\nline6";
    let result = extract_tail_lines(text, 4);
    assert_eq!(result, "line3\nline4\nline5\nline6");
}

/// 测试 extract_tail_lines：不足 N 行返回全部
#[test]
fn test_extract_tail_lines_less_than_n() {
    let text = "line1\nline2";
    let result = extract_tail_lines(text, 4);
    assert_eq!(result, "line1\nline2");
}

/// 测试 extract_tail_lines：单行
#[test]
fn test_extract_tail_lines_single_line() {
    let text = "hello world";
    let result = extract_tail_lines(text, 4);
    assert_eq!(result, "hello world");
}

/// frozen_subagent_vms 跨轮次累积：begin_round() 应清空上一轮冻结的 VMs，
/// 防止 merge_frozen_subagents 按位置错误匹配到旧轮次的数据。
#[test]
fn test_frozen_subagent_vms_cleared_on_begin_round() {
    let mut pipeline = MessagePipeline::new("/tmp".to_string());
    pipeline.has_snapshot_this_round = true;
    pipeline.completed_len_at_round_start = 0;

    // ── 轮次 1：并发 2 个 SubAgent ──
    pipeline.handle_event(AgentEvent::SubAgentStart {
        agent_id: "sa1".into(),
        instance_id: "test-instance".into(),
        task_preview: "task one".into(),
        is_background: false,
    });
    pipeline.handle_event(AgentEvent::SubAgentStart {
        agent_id: "sa2".into(),
        instance_id: "test-instance".into(),
        task_preview: "task two".into(),
        is_background: false,
    });
    pipeline.handle_event(AgentEvent::SubAgentEnd {
        result: "result sa1".into(),
        is_error: false,
        agent_id: Some("sa1".into()),
        instance_id: None,
    });
    pipeline.handle_event(AgentEvent::SubAgentEnd {
        result: "result sa2".into(),
        is_error: false,
        agent_id: Some("sa2".into()),
        instance_id: None,
    });

    // 验证轮次 1 的 frozen_subagent_vms 包含 2 个冻结 VM
    assert_eq!(
        pipeline.frozen_subagent_vms_count(),
        2,
        "轮次 1 结束后应有 2 个 frozen VM"
    );

    pipeline.done();

    // done() 不应清空 frozen_subagent_vms（它们会被 build_tail_vms 消费）
    assert_eq!(
        pipeline.frozen_subagent_vms_count(),
        2,
        "done() 后 frozen VMs 仍在（等待 build_tail_vms 消费）"
    );

    // ── 轮次 2 开始 ──
    pipeline.begin_round();

    // begin_round() 应清空上一轮的 frozen_subagent_vms
    assert_eq!(
        pipeline.frozen_subagent_vms_count(),
        0,
        "begin_round() 后 frozen_subagent_vms 应被清空"
    );

    // ── 轮次 2：单个 SubAgent sa3 ──
    pipeline.handle_event(AgentEvent::SubAgentStart {
        agent_id: "sa3".into(),
        instance_id: "test-instance".into(),
        task_preview: "task three".into(),
        is_background: false,
    });
    pipeline.handle_event(AgentEvent::SubAgentEnd {
        result: "result sa3".into(),
        is_error: false,
        agent_id: None,
        instance_id: None,
    });

    assert_eq!(
        pipeline.frozen_subagent_vms_count(),
        1,
        "轮次 2 应只有 1 个 frozen VM（sa3）"
    );

    // 验证 sa3 的 frozen VM 内容正确（不是 sa1/sa2 的数据）
    if let MessageViewModel::SubAgentGroup { agent_id, .. } = &pipeline.frozen_subagent_vms[0] {
        assert_eq!(agent_id, "sa3", "frozen VM 应属于 sa3，而非 sa1/sa2");
    } else {
        panic!("frozen_subagent_vms[0] 应为 SubAgentGroup");
    }
}

/// merge_frozen_subagents 在 frozen_vms 为空时不应修改 new_vms。
#[test]
fn test_merge_frozen_subagents_empty_is_noop() {
    let mut new_vms = vec![{
        let mut vm = MessageViewModel::SubAgentGroup {
            agent_id: "sa1".into(),
            task_preview: "task".into(),
            total_steps: 3,
            recent_messages: Vec::new(),
            is_running: false,
            collapsed: false,
            final_result: Some("result".into()),
            is_error: false,
            is_background: false,
            bg_hash: Some("abc123".to_string()),
            batch_agents: Vec::new(),
            instance_id: None,
            content_hash: 0,
        };
        vm.recompute_hash();
        vm
    }];
    let original = new_vms.clone();
    merge_frozen_subagents(&[], &mut new_vms);
    assert_eq!(new_vms, original, "空 frozen_vms 不应修改 new_vms");
}

/// 回归测试：ToolStart(Agent) + SubAgentStart 不应创建重复 SubAgentState
///
/// 修复前：ToolStart 调用 tool_start_internal 创建 SubAgentState #1，
/// SubAgentStart 再次调用 tool_start_internal 创建 SubAgentState #2。
/// 导致 build_tail_vms 输出两个 SubAgentGroup（一个 frozen + 一个 running），
/// 造成显示闪烁和聚合异常。
#[test]
fn test_no_duplicate_subagent_state_on_tool_start_plus_subagent_start() {
    let mut pipeline = MessagePipeline::new("/tmp".to_string());

    // 1. ToolStart(Agent) — 父 Agent 调用 Agent 工具
    let _ = pipeline.handle_event(AgentEvent::ToolStart {
        tool_call_id: "call_abc123".into(),
        name: "Agent".into(),
        display: "Agent".into(),
        args: "".into(),
        input: json!({"subagent_type": "code-reviewer", "prompt": "review code"}),
        source_agent_id: None,
    });

    // ToolStart 不应创建 SubAgentState，只注册 pending_tool
    assert_eq!(
        pipeline.subagent_stack.len(),
        0,
        "ToolStart(Agent) 不应创建 SubAgentState"
    );
    assert!(
        pipeline.pending_tools.contains_key("call_abc123"),
        "ToolStart 应注册 pending_tool"
    );

    // 2. SubAgentStart — 创建 SubAgentState
    let _ = pipeline.handle_event(AgentEvent::SubAgentStart {
        agent_id: "code-reviewer".into(),
        instance_id: "test-instance".into(),
        task_preview: "review code".into(),
        is_background: false,
    });

    // SubAgentStart 创建唯一一个 SubAgentState
    assert_eq!(
        pipeline.subagent_stack.len(),
        1,
        "SubAgentStart 应创建恰好一个 SubAgentState"
    );
    assert_eq!(pipeline.subagent_stack[0].agent_id, "code-reviewer");

    // 3. SubAgentEnd — 冻结
    let _ = pipeline.handle_event(AgentEvent::SubAgentEnd {
        agent_id: Some("code-reviewer".into()),
        instance_id: None,
        result: "review done".into(),
        is_error: false,
    });

    // 只有一个 frozen VM
    assert_eq!(
        pipeline.frozen_subagent_vms_count(),
        1,
        "应恰好冻结一个 SubAgentGroup"
    );
    assert!(
        !pipeline.subagent_stack[0].is_running,
        "SubAgentState 应标记为非运行中"
    );

    // 4. ToolEnd — 清理 pending_tool，不产生额外副作用
    let _ = pipeline.handle_event(AgentEvent::ToolEnd {
        tool_call_id: "call_abc123".into(),
        name: "Agent".into(),
        output: "review done".into(),
        is_error: false,
        source_agent_id: None,
    });

    assert!(
        !pipeline.pending_tools.contains_key("call_abc123"),
        "ToolEnd 应清理 pending_tool"
    );
    // frozen_count 不应增加
    assert_eq!(
        pipeline.frozen_subagent_vms_count(),
        1,
        "ToolEnd 不应产生额外的 frozen VM"
    );
}

// ─── diff_lines 集成测试 ──────────────────────────────────────────────────

/// 测试：Edit 工具的 ToolEnd 产生 diff_lines（old_content/new_content/file_path）
#[test]
fn test_edit_tool_end_produces_diff_lines() {
    let mut pipeline = MessagePipeline::new("/tmp".to_string());

    // Arrange：发送 AssistantChunk + ToolStart + ToolEnd
    pipeline.handle_event(AgentEvent::AssistantChunk {
        chunk: "I'll edit the file".into(),
        source_agent_id: None,
    });
    pipeline.handle_event(AgentEvent::ToolStart {
        tool_call_id: "tc_edit1".into(),
        name: "Edit".into(),
        display: "Edit".into(),
        args: "src/main.rs".into(),
        input: json!({
            "file_path": "/tmp/src/main.rs",
            "old_string": "fn old() {}",
            "new_string": "fn new() {}"
        }),
        source_agent_id: None,
    });
    pipeline.handle_event(AgentEvent::ToolEnd {
        tool_call_id: "tc_edit1".into(),
        name: "Edit".into(),
        output: "Replaced text".into(),
        is_error: false,
        source_agent_id: None,
    });

    // Act：build_tail_vms 从 completed_tools 构建 ToolBlock
    let tail_vms = pipeline.build_tail_vms();

    // Assert：找到 Edit ToolBlock，验证 diff_lines 有值且内容正确
    let edit_block = tail_vms.iter().find(
        |vm| matches!(vm, MessageViewModel::ToolBlock { tool_name, .. } if tool_name == "Edit"),
    );
    assert!(
        edit_block.is_some(),
        "应包含 Edit ToolBlock，实际 VMs: {:?}",
        tail_vms
    );

    if let MessageViewModel::ToolBlock { diff_lines, .. } = edit_block.unwrap() {
        let lines = diff_lines
            .as_ref()
            .expect("Edit 工具应产生 diff_lines（预渲染行）");
        assert!(!lines.is_empty(), "Edit diff 应至少有 1 行渲染输出");
        // 验证渲染行中包含文件路径
        let combined: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.clone()))
            .collect();
        assert!(
            combined.contains("/tmp/src/main.rs"),
            "diff 渲染应包含文件路径，实际: {}",
            combined
        );
    }
}

/// 测试：非 Edit/Write 工具不产生 diff_lines
#[test]
fn test_non_edit_tool_no_diff_lines() {
    let mut pipeline = MessagePipeline::new("/tmp".to_string());

    // Arrange：发送 Bash 工具的 ToolStart + ToolEnd
    pipeline.handle_event(AgentEvent::ToolStart {
        tool_call_id: "tc_bash1".into(),
        name: "Bash".into(),
        display: "Bash".into(),
        args: "ls -la".into(),
        input: json!({"command": "ls -la"}),
        source_agent_id: None,
    });
    pipeline.handle_event(AgentEvent::ToolEnd {
        tool_call_id: "tc_bash1".into(),
        name: "Bash".into(),
        output: "file1.txt\nfile2.txt".into(),
        is_error: false,
        source_agent_id: None,
    });

    // Act
    let tail_vms = pipeline.build_tail_vms();

    // Assert：Bash ToolBlock 的 diff_lines 应为 None
    let bash_block = tail_vms.iter().find(
        |vm| matches!(vm, MessageViewModel::ToolBlock { tool_name, .. } if tool_name == "Bash"),
    );
    assert!(bash_block.is_some(), "应包含 Bash ToolBlock");

    if let MessageViewModel::ToolBlock { diff_lines, .. } = bash_block.unwrap() {
        assert!(
            diff_lines.is_none(),
            "Bash 工具不应产生 diff_lines，实际: {:?}",
            diff_lines
        );
    }
}

/// 测试：is_error=true 的 ToolEnd 不产生 diff_lines（即使工具名是 Edit）
#[test]
fn test_error_tool_end_no_diff_lines() {
    let mut pipeline = MessagePipeline::new("/tmp".to_string());

    // Arrange：Edit 工具执行失败
    pipeline.handle_event(AgentEvent::ToolStart {
        tool_call_id: "tc_err1".into(),
        name: "Edit".into(),
        display: "Edit".into(),
        args: "missing.rs".into(),
        input: json!({
            "file_path": "/tmp/missing.rs",
            "old_string": "old code",
            "new_string": "new code"
        }),
        source_agent_id: None,
    });
    pipeline.handle_event(AgentEvent::ToolEnd {
        tool_call_id: "tc_err1".into(),
        name: "Edit".into(),
        output: "File not found".into(),
        is_error: true,
        source_agent_id: None,
    });

    // Act
    let tail_vms = pipeline.build_tail_vms();

    // Assert：错误 ToolBlock 的 diff_lines 应为 None
    let err_block = tail_vms.iter().find(|vm| {
        matches!(vm, MessageViewModel::ToolBlock { tool_name, is_error, .. } if tool_name == "Edit" && *is_error)
    });
    assert!(err_block.is_some(), "应包含 Edit 错误 ToolBlock");

    if let MessageViewModel::ToolBlock { diff_lines, .. } = err_block.unwrap() {
        assert!(
            diff_lines.is_none(),
            "错误 ToolEnd 不应产生 diff_lines，实际: {:?}",
            diff_lines
        );
    }
}

// ─── AdaptiveChunkingPolicy 测试 ───────────────────────────────────────

use super::{AdaptiveChunkingPolicy, ChunkingMode, DrainPlan};

/// 辅助：创建策略并用指定数量的行填充
fn make_policy_with_lines(line_count: usize) -> AdaptiveChunkingPolicy {
    let mut policy = AdaptiveChunkingPolicy::new();
    // 每行一个 chunk，模拟逐行到达
    for _ in 0..line_count {
        policy.on_chunk("hello\n");
    }
    policy
}

/// 测试：新策略初始状态为 Smooth，无积压，check 返回 None
#[test]
fn test_policy_initial_state() {
    let policy = AdaptiveChunkingPolicy::new();
    assert_eq!(policy.mode, ChunkingMode::Smooth);
    assert_eq!(policy.pending_lines, 0);
    assert!(policy.oldest_chunk_at.is_none());
}

/// 测试：单个 chunk 正确累积行数
#[test]
fn test_policy_on_chunk_single_line() {
    let mut policy = AdaptiveChunkingPolicy::new();
    policy.on_chunk("hello");
    assert_eq!(policy.pending_lines, 1);
    assert!(policy.oldest_chunk_at.is_some());
}

/// 测试：多行 chunk 正确累积行数
#[test]
fn test_policy_on_chunk_multi_line() {
    let mut policy = AdaptiveChunkingPolicy::new();
    policy.on_chunk("line1\nline2\nline3");
    assert_eq!(policy.pending_lines, 3);
}

/// 测试：空 chunk 仍记为 1 行（min 1）
#[test]
fn test_policy_on_chunk_empty() {
    let mut policy = AdaptiveChunkingPolicy::new();
    policy.on_chunk("");
    assert_eq!(policy.pending_lines, 1);
}

/// 测试：积压为 0 时 check 返回 None
#[test]
fn test_policy_check_no_pending() {
    let mut policy = AdaptiveChunkingPolicy::new();
    assert!(policy.check().is_none());
}

/// 测试：Smooth 模式下少量积压返回 Single
#[test]
fn test_policy_smooth_returns_single() {
    let mut policy = AdaptiveChunkingPolicy::new();
    // 低于 queue_depth_threshold(8)，仍在 Smooth
    policy.on_chunk("hello\n");
    let plan = policy.check();
    assert_eq!(plan, Some(DrainPlan::Single));
}

/// 测试：队列深度达到阈值（8 行）触发 CatchUp
#[test]
fn test_policy_catchup_by_depth() {
    let mut policy = make_policy_with_lines(8);
    let plan = policy.check();
    assert_eq!(plan, Some(DrainPlan::Batch));
    assert_eq!(policy.mode, ChunkingMode::CatchUp);
}

/// 测试：队列深度超过阈值（10 行）也触发 CatchUp
#[test]
fn test_policy_catchup_by_depth_overflow() {
    let mut policy = make_policy_with_lines(10);
    let plan = policy.check();
    assert_eq!(plan, Some(DrainPlan::Batch));
}

/// 测试：最老行年龄达到阈值（120ms）触发 CatchUp
#[test]
fn test_policy_catchup_by_age() {
    let mut policy = AdaptiveChunkingPolicy::new();
    policy.on_chunk("hello\n");
    // 手动设置 oldest_chunk_at 为 150ms 前
    policy.oldest_chunk_at = Some(Instant::now() - Duration::from_millis(150));
    let plan = policy.check();
    assert_eq!(plan, Some(DrainPlan::Batch));
    assert_eq!(policy.mode, ChunkingMode::CatchUp);
}

/// 测试：队列深度 7 + 年龄 119ms → 不触发 CatchUp（两个条件都未达阈值）
#[test]
fn test_policy_no_catchup_below_threshold() {
    let mut policy = AdaptiveChunkingPolicy::new();
    // 7 行，低于阈值 8
    for _ in 0..7 {
        policy.on_chunk("hello\n");
    }
    // 年龄 119ms，低于阈值 120ms
    policy.oldest_chunk_at = Some(Instant::now() - Duration::from_millis(119));
    let plan = policy.check();
    assert_eq!(plan, Some(DrainPlan::Single));
    assert_eq!(policy.mode, ChunkingMode::Smooth);
}

/// 测试：CatchUp 模式下 drain 后队列清空，但模式保持 CatchUp
/// （需要同时满足 exit_depth 和 exit_age 才退出）
#[test]
fn test_policy_catchup_drain_stays_catchup() {
    let mut policy = make_policy_with_lines(10);
    // 触发 CatchUp
    let _ = policy.check();
    assert_eq!(policy.mode, ChunkingMode::CatchUp);
    // drain 后队列为空
    policy.drain();
    assert_eq!(policy.pending_lines, 0);
    // 下一个 check：无积压，返回 None
    assert!(policy.check().is_none());
}

/// 测试：CatchUp → Smooth 退出条件：队列深度 <= 2 且年龄 <= 40ms
#[test]
fn test_policy_exit_catchup() {
    let mut policy = make_policy_with_lines(10);
    let _ = policy.check();
    assert_eq!(policy.mode, ChunkingMode::CatchUp);
    // drain 后重新填充少量数据（2 行，年龄 < 40ms）
    policy.drain();
    policy.on_chunk("a\n");
    policy.on_chunk("b\n");
    let plan = policy.check();
    // 退出条件满足：depth=2 <= 2 且年龄 ≈ 0ms <= 40ms
    assert_eq!(policy.mode, ChunkingMode::Smooth);
    assert_eq!(plan, Some(DrainPlan::Single));
}

/// 测试：CatchUp 不退出：深度 <= 2 但年龄 > 40ms
#[test]
fn test_policy_no_exit_age_too_old() {
    let mut policy = make_policy_with_lines(10);
    let _ = policy.check();
    assert_eq!(policy.mode, ChunkingMode::CatchUp);
    // drain 后重新填充 2 行，但设置年龄为 50ms（> 40ms）
    policy.drain();
    policy.on_chunk("a\n");
    policy.on_chunk("b\n");
    policy.oldest_chunk_at = Some(Instant::now() - Duration::from_millis(50));
    let _ = policy.check();
    // 不满足退出：年龄 50ms > 40ms
    assert_eq!(policy.mode, ChunkingMode::CatchUp);
}

/// 测试：CatchUp 不退出：年龄 <= 40ms 但深度 > 2
#[test]
fn test_policy_no_exit_depth_too_high() {
    let mut policy = make_policy_with_lines(10);
    let _ = policy.check();
    assert_eq!(policy.mode, ChunkingMode::CatchUp);
    // drain 后重新填充 3 行（> exit_depth=2），年龄约 0ms
    policy.drain();
    for _ in 0..3 {
        policy.on_chunk("a\n");
    }
    let _ = policy.check();
    // 不满足退出：depth=3 > 2
    assert_eq!(policy.mode, ChunkingMode::CatchUp);
}

/// 测试：reset 恢复到初始状态
#[test]
fn test_policy_reset() {
    let mut policy = make_policy_with_lines(10);
    let _ = policy.check();
    assert_eq!(policy.mode, ChunkingMode::CatchUp);
    policy.reset();
    assert_eq!(policy.mode, ChunkingMode::Smooth);
    assert_eq!(policy.pending_lines, 0);
    assert!(policy.oldest_chunk_at.is_none());
}

/// 测试：drain 只清空积压，不改变模式
#[test]
fn test_policy_drain_preserves_mode() {
    let mut policy = make_policy_with_lines(10);
    let _ = policy.check();
    assert_eq!(policy.mode, ChunkingMode::CatchUp);
    policy.drain();
    assert_eq!(policy.mode, ChunkingMode::CatchUp);
    assert_eq!(policy.pending_lines, 0);
}

/// 测试：on_reasoning_chunk 正确累积
#[test]
fn test_policy_on_reasoning_chunk() {
    let mut policy = AdaptiveChunkingPolicy::new();
    policy.on_reasoning_chunk();
    assert_eq!(policy.pending_lines, 1);
    assert!(policy.oldest_chunk_at.is_some());
}

/// 测试：连续多轮 chunk → check → drain → chunk 的完整生命周期
#[test]
fn test_policy_lifecycle_smooth_to_catchup_and_back() {
    let mut policy = AdaptiveChunkingPolicy::new();

    // 第一轮：Smooth，3 行
    for _ in 0..3 {
        policy.on_chunk("hello\n");
    }
    assert_eq!(policy.check(), Some(DrainPlan::Single));
    assert_eq!(policy.mode, ChunkingMode::Smooth);
    policy.drain();

    // 第二轮：积压爆发，12 行 → CatchUp
    for _ in 0..12 {
        policy.on_chunk("burst\n");
    }
    assert_eq!(policy.check(), Some(DrainPlan::Batch));
    assert_eq!(policy.mode, ChunkingMode::CatchUp);
    policy.drain();

    // 第三轮：恢复正常，1 行 → 应回到 Smooth
    policy.on_chunk("normal\n");
    // drain 后 oldest_chunk_at 已清空，重新填充后年龄约 0ms
    // depth=1 <= 2 且年龄 ≈ 0 <= 40ms → 退出 CatchUp
    assert_eq!(policy.check(), Some(DrainPlan::Single));
    assert_eq!(policy.mode, ChunkingMode::Smooth);
}

// ─── check_throttle 集成测试 ────────────────────────────────────────────

/// 测试：无流式内容时 check_throttle 返回 None
#[test]
fn test_check_throttle_no_content() {
    let mut pipeline = MessagePipeline::new("/tmp".to_string());
    let result = pipeline.check_throttle(0);
    assert!(result.is_none());
}

/// 测试：单次 chunk 后 check_throttle 返回 RebuildAll
#[test]
fn test_check_throttle_single_chunk() {
    let mut pipeline = MessagePipeline::new("/tmp".to_string());
    pipeline.handle_event(AgentEvent::AssistantChunk {
        chunk: "hello".into(),
        source_agent_id: None,
    });
    let result = pipeline.check_throttle(0);
    assert!(result.is_some());
    assert!(matches!(result.unwrap(), PipelineAction::RebuildAll { .. }));
}

/// 测试：连续 chunk 积压触发 CatchUp 模式
#[test]
fn test_check_throttle_catchup_on_burst() {
    let mut pipeline = MessagePipeline::new("/tmp".to_string());
    // 发送 10 个 chunk（超过 queue_depth_threshold=8）
    for i in 0..10 {
        pipeline.handle_event(AgentEvent::AssistantChunk {
            chunk: format!("line {}\n", i),
            source_agent_id: None,
        });
    }
    // check_throttle 会触发 update_mode，此时应切换到 CatchUp 并立即返回
    let result = pipeline.check_throttle(0);
    assert!(result.is_some(), "CatchUp 模式应立即返回 RebuildAll");
    // drain 后 pending_lines 归零
    assert_eq!(pipeline.adaptive_policy.pending_lines, 0);
}

/// 测试：ToolStart 消费积压后 check_throttle 返回 None
#[test]
fn test_check_throttle_drained_after_tool_start() {
    let mut pipeline = MessagePipeline::new("/tmp".to_string());
    pipeline.handle_event(AgentEvent::AssistantChunk {
        chunk: "hello".into(),
        source_agent_id: None,
    });
    // ToolStart 会 drain 积压
    pipeline.handle_event(AgentEvent::ToolStart {
        tool_call_id: "tc1".into(),
        name: "Read".into(),
        display: "ReadFile".into(),
        args: "src/main.rs".into(),
        input: serde_json::json!({"file_path": "/tmp/src/main.rs"}),
        source_agent_id: None,
    });
    // 积压已被消费
    assert_eq!(pipeline.adaptive_policy.pending_lines, 0);
    let result = pipeline.check_throttle(0);
    assert!(result.is_none());
}

/// 测试：done() 重置策略状态
#[test]
fn test_done_resets_policy() {
    let mut pipeline = MessagePipeline::new("/tmp".to_string());
    // 积压数据
    for _ in 0..10 {
        pipeline.handle_event(AgentEvent::AssistantChunk {
            chunk: "burst\n".into(),
            source_agent_id: None,
        });
    }
    assert!(pipeline.adaptive_policy.pending_lines > 0);
    pipeline.done();
    assert_eq!(pipeline.adaptive_policy.pending_lines, 0);
    assert_eq!(pipeline.adaptive_policy.mode, ChunkingMode::Smooth);
    assert!(pipeline.check_throttle(0).is_none());
}

/// 测试：begin_round() 重置策略状态
#[test]
fn test_begin_round_resets_policy() {
    let mut pipeline = MessagePipeline::new("/tmp".to_string());
    for _ in 0..10 {
        pipeline.handle_event(AgentEvent::AssistantChunk {
            chunk: "burst\n".into(),
            source_agent_id: None,
        });
    }
    pipeline.begin_round();
    assert_eq!(pipeline.adaptive_policy.pending_lines, 0);
    assert_eq!(pipeline.adaptive_policy.mode, ChunkingMode::Smooth);
}

// ─── StreamingMode 测试 ──────────────────────────────────────────────────

use super::StreamingMode;

#[test]
fn test_streaming_mode_default() {
    let pipeline = MessagePipeline::new("/tmp".to_string());
    assert_eq!(pipeline.streaming_mode(), StreamingMode::Streaming);
}

#[test]
fn test_set_streaming_mode() {
    let mut pipeline = MessagePipeline::new("/tmp".to_string());
    pipeline.set_streaming_mode(StreamingMode::Block);
    assert_eq!(pipeline.streaming_mode(), StreamingMode::Block);
    pipeline.set_streaming_mode(StreamingMode::None);
    assert_eq!(pipeline.streaming_mode(), StreamingMode::None);
    pipeline.set_streaming_mode(StreamingMode::Streaming);
    assert_eq!(pipeline.streaming_mode(), StreamingMode::Streaming);
}

#[test]
fn test_push_chunk_streaming_updates_policy() {
    let mut pipeline = MessagePipeline::new("/tmp".to_string());
    pipeline.set_streaming_mode(StreamingMode::Streaming);
    pipeline.push_chunk("hello");
    assert_eq!(pipeline.current_ai_text, "hello");
    assert!(pipeline.adaptive_policy.pending_lines > 0);
}

#[test]
fn test_push_chunk_none_only_accumulates() {
    let mut pipeline = MessagePipeline::new("/tmp".to_string());
    pipeline.set_streaming_mode(StreamingMode::None);
    pipeline.push_chunk("hello");
    assert_eq!(pipeline.current_ai_text, "hello");
    assert_eq!(pipeline.adaptive_policy.pending_lines, 0);
}

#[test]
fn test_push_chunk_block_buffering() {
    let mut pipeline = MessagePipeline::new("/tmp".to_string());
    pipeline.set_streaming_mode(StreamingMode::Block);
    pipeline.push_chunk("hello");
    assert_eq!(pipeline.current_ai_text, "");
    assert_eq!(pipeline.block_buffer, "hello");
}

#[test]
fn test_push_chunk_block_flush_on_double_newline() {
    let mut pipeline = MessagePipeline::new("/tmp".to_string());
    pipeline.set_streaming_mode(StreamingMode::Block);
    pipeline.push_chunk("paragraph one\n\n");
    assert_eq!(pipeline.current_ai_text, "paragraph one\n\n");
    assert!(pipeline.block_buffer.is_empty());
    assert!(pipeline.block_pending_flush);
}

#[test]
fn test_push_chunk_block_code_fence() {
    let mut pipeline = MessagePipeline::new("/tmp".to_string());
    pipeline.set_streaming_mode(StreamingMode::Block);
    pipeline.push_chunk("```rust\n");
    assert!(pipeline.inside_code_fence);
    assert_eq!(pipeline.current_ai_text, "");
    pipeline.push_chunk("fn main() {}\n");
    assert_eq!(pipeline.current_ai_text, "");
    pipeline.push_chunk("```\n");
    assert!(!pipeline.inside_code_fence);
    assert_eq!(pipeline.current_ai_text, "```rust\nfn main() {}\n```\n");
    assert!(pipeline.block_pending_flush);
}

#[test]
fn test_check_throttle_none_mode() {
    let mut pipeline = MessagePipeline::new("/tmp".to_string());
    pipeline.set_streaming_mode(StreamingMode::None);
    pipeline.handle_event(AgentEvent::AssistantChunk {
        chunk: "hello".into(),
        source_agent_id: None,
    });
    let result = pipeline.check_throttle(0);
    assert!(result.is_none());
}

#[test]
fn test_check_throttle_block_no_flush() {
    let mut pipeline = MessagePipeline::new("/tmp".to_string());
    pipeline.set_streaming_mode(StreamingMode::Block);
    pipeline.push_chunk("hello");
    let result = pipeline.check_throttle(0);
    assert!(result.is_none());
}

#[test]
fn test_check_throttle_block_after_flush() {
    let mut pipeline = MessagePipeline::new("/tmp".to_string());
    pipeline.set_streaming_mode(StreamingMode::Block);
    pipeline.push_chunk("hello\n\n");
    let result = pipeline.check_throttle(0);
    assert!(result.is_some());
    let result2 = pipeline.check_throttle(0);
    assert!(result2.is_none());
}

#[test]
fn test_set_streaming_mode_flushes_on_exit_block() {
    let mut pipeline = MessagePipeline::new("/tmp".to_string());
    pipeline.set_streaming_mode(StreamingMode::Block);
    pipeline.push_chunk("unflushed content");
    assert_eq!(pipeline.current_ai_text, "");
    pipeline.set_streaming_mode(StreamingMode::Streaming);
    assert_eq!(pipeline.current_ai_text, "unflushed content");
    assert!(pipeline.block_buffer.is_empty());
}

#[test]
fn test_done_flushes_block_buffer() {
    let mut pipeline = MessagePipeline::new("/tmp".to_string());
    pipeline.set_streaming_mode(StreamingMode::Block);
    pipeline.handle_event(AgentEvent::AssistantChunk {
        chunk: "hello".into(),
        source_agent_id: None,
    });
    assert_eq!(pipeline.current_ai_text, "");
    pipeline.done();
    // done() calls finalize_current_ai() which clears current_ai_text, but block_buffer was flushed
    assert!(pipeline.block_buffer.is_empty());
}
