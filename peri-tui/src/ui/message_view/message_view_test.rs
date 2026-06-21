use peri_agent::messages::{BaseMessage, MessageContent, ToolCallRequest};
use serde_json::json;

use super::*;
use crate::ui::message_view::ContentBlockView;
use crate::ui::theme;

/// 测试：AI 消息只有 tool_calls（无 content）时，应正确渲染工具调用
#[test]
fn test_ai_message_with_only_tool_calls_renders_tool_use() {
    // 模拟：AI 消息只包含 tool_calls，content 为空
    let msg = BaseMessage::ai_with_tool_calls(
        MessageContent::text(""),
        vec![
            ToolCallRequest::new("toolu_001", "Bash", json!({"command": "ls"})),
            ToolCallRequest::new("toolu_002", "Read", json!({"path": "test.txt"})),
        ],
    );

    let vm = MessageViewModel::from_base_message(&msg, &[]);
    match vm {
        MessageViewModel::AssistantBubble { blocks, .. } => {
            // 应该有 2 个 ToolUse block
            let tool_uses: Vec<_> = blocks
                .iter()
                .filter(|b| matches!(b, ContentBlockView::ToolUse { .. }))
                .collect();
            assert_eq!(tool_uses.len(), 2, "应该有 2 个 ToolUse block");

            // 验证工具名称
            let names: Vec<&str> = blocks
                .iter()
                .filter_map(|b| {
                    if let ContentBlockView::ToolUse { name } = b {
                        Some(name.as_str())
                    } else {
                        None
                    }
                })
                .collect();
            assert!(names.contains(&"Bash"), "应包含 bash 工具");
            assert!(names.contains(&"Read"), "应包含 read_file 工具");
        }
        _ => panic!("应该是 AssistantBubble"),
    }
}

/// 测试：AI 消息同时有文本和 tool_calls 时，两者都应渲染
#[test]
fn test_ai_message_with_text_and_tool_calls_renders_both() {
    let msg = BaseMessage::ai_with_tool_calls(
        MessageContent::text("I'll run a command"),
        vec![ToolCallRequest::new(
            "toolu_001",
            "Bash",
            json!({"command": "ls"}),
        )],
    );

    let vm = MessageViewModel::from_base_message(&msg, &[]);
    match vm {
        MessageViewModel::AssistantBubble { blocks, .. } => {
            // 应该有 1 个 Text block 和 1 个 ToolUse block
            let text_count = blocks
                .iter()
                .filter(|b| matches!(b, ContentBlockView::Text { .. }))
                .count();
            let tool_count = blocks
                .iter()
                .filter(|b| matches!(b, ContentBlockView::ToolUse { .. }))
                .count();

            assert_eq!(text_count, 1, "应该有 1 个 Text block");
            assert_eq!(tool_count, 1, "应该有 1 个 ToolUse block");
        }
        _ => panic!("应该是 AssistantBubble"),
    }
}

/// 测试：content 中已有 ToolUse block 时，不重复添加 tool_calls
#[test]
fn test_no_duplicate_tool_use_from_tool_calls() {
    use peri_agent::messages::ContentBlock;

    // content 中包含 ToolUse block，同时 tool_calls 也有相同的
    let blocks = vec![
        ContentBlock::text("I'll run bash"),
        ContentBlock::tool_use("toolu_001", "Bash", json!({"command": "ls"})),
    ];
    let msg = BaseMessage::ai_from_blocks(blocks);

    let vm = MessageViewModel::from_base_message(&msg, &[]);
    match vm {
        MessageViewModel::AssistantBubble { blocks, .. } => {
            // 应该只有 1 个 ToolUse block（不重复）
            let tool_count = blocks
                .iter()
                .filter(|b| matches!(b, ContentBlockView::ToolUse { .. }))
                .count();
            assert_eq!(tool_count, 1, "不应该重复添加 ToolUse block");
        }
        _ => panic!("应该是 AssistantBubble"),
    }
}

/// 测试：纯文本 AI 消息正常渲染
#[test]
fn test_ai_message_with_only_text_renders_text() {
    let msg = BaseMessage::ai("Hello, how can I help?");

    let vm = MessageViewModel::from_base_message(&msg, &[]);
    match vm {
        MessageViewModel::AssistantBubble { blocks, .. } => {
            assert_eq!(blocks.len(), 1, "应该有 1 个 block");
            assert!(
                matches!(blocks[0], ContentBlockView::Text { .. }),
                "应该是 Text block"
            );
        }
        _ => panic!("应该是 AssistantBubble"),
    }
}

#[test]
fn test_tool_category_new_names() {
    assert_eq!(
        ToolCategory::from_tool_name("Read"),
        Some(ToolCategory::Read)
    );
    assert_eq!(
        ToolCategory::from_tool_name("Grep"),
        Some(ToolCategory::Search)
    );
    assert_eq!(
        ToolCategory::from_tool_name("Glob"),
        Some(ToolCategory::Glob)
    );
    assert_eq!(ToolCategory::from_tool_name("Write"), None);
    assert_eq!(ToolCategory::from_tool_name("Bash"), None);
    assert_eq!(ToolCategory::from_tool_name("Agent"), None);
    assert_eq!(
        ToolCategory::from_tool_name("AskUserQuestion"),
        Some(ToolCategory::AskUser)
    );
}

#[test]
fn test_tool_color_new_names() {
    // 读取/搜索 — SAGE
    assert_eq!(tool_color("Read"), theme::SAGE);
    assert_eq!(tool_color("Glob"), theme::SAGE);
    assert_eq!(tool_color("Grep"), theme::SAGE);
    // 写入/编辑 — WARNING
    assert_eq!(tool_color("Write"), theme::WARNING);
    assert_eq!(tool_color("Edit"), theme::WARNING);
    // 执行 — BASH_BORDER
    assert_eq!(tool_color("Bash"), theme::BASH_BORDER);
    // 代理/交互 — THINKING
    assert_eq!(tool_color("Agent"), theme::THINKING);
    assert_eq!(tool_color("AskUserQuestion"), theme::THINKING);
    assert_eq!(tool_color("TodoWrite"), theme::THINKING);
}

// ── aggregate_batch_groups 测试 ──

/// 创建一个已完成的单 agent SubAgentGroup VM
fn make_done_subagent(agent_id: &str, task: &str) -> MessageViewModel {
    let mut vm = MessageViewModel::SubAgentGroup {
        agent_id: agent_id.to_string(),
        task_preview: task.to_string(),
        total_steps: 3,
        recent_messages: Vec::new(),
        is_running: false,
        collapsed: false,
        final_result: Some("done".to_string()),
        is_error: false,
        is_background: false,
        bg_hash: Some("test01".to_string()),
        batch_agents: Vec::new(),
        instance_id: None,
        content_hash: 0,
    };
    vm.recompute_hash();
    vm
}

/// 创建一个运行中的 SubAgentGroup VM
fn make_running_subagent(agent_id: &str, task: &str) -> MessageViewModel {
    let mut vm = MessageViewModel::SubAgentGroup {
        agent_id: agent_id.to_string(),
        task_preview: task.to_string(),
        total_steps: 0,
        recent_messages: Vec::new(),
        is_running: true,
        collapsed: false,
        final_result: None,
        is_error: false,
        is_background: false,
        bg_hash: Some("test02".to_string()),
        batch_agents: Vec::new(),
        instance_id: None,
        content_hash: 0,
    };
    vm.recompute_hash();
    vm
}

#[test]
fn test_aggregate_batch_groups_single_agent_noop() {
    let mut vms = vec![make_done_subagent("explorer", "explore code")];
    aggregate_batch_groups(&mut vms);
    assert_eq!(vms.len(), 1, "单个 SubAgentGroup 不应聚合");
    // batch_agents 应保持为空
    if let MessageViewModel::SubAgentGroup { batch_agents, .. } = &vms[0] {
        assert!(batch_agents.is_empty());
    } else {
        panic!("应为 SubAgentGroup");
    }
}

#[test]
fn test_aggregate_batch_groups_consecutive_agents() {
    let mut vms = vec![
        make_done_subagent("agent-1", "task one"),
        make_done_subagent("agent-2", "task two"),
        make_done_subagent("agent-3", "task three"),
    ];
    aggregate_batch_groups(&mut vms);
    assert_eq!(vms.len(), 1, "3 个连续已完成 SubAgentGroup 应合并为 1 个");
    if let MessageViewModel::SubAgentGroup {
        batch_agents,
        collapsed,
        ..
    } = &vms[0]
    {
        assert_eq!(batch_agents.len(), 3);
        assert!(*collapsed, "合并后应默认折叠");
        assert_eq!(batch_agents[0].agent_id, "agent-1");
        assert_eq!(batch_agents[1].agent_id, "agent-2");
        assert_eq!(batch_agents[2].agent_id, "agent-3");
    } else {
        panic!("应为 SubAgentGroup");
    }
}

#[test]
fn test_aggregate_batch_groups_running_agent_skip() {
    let mut vms = vec![
        make_done_subagent("agent-1", "task one"),
        make_running_subagent("agent-2", "task two"),
        make_done_subagent("agent-3", "task three"),
    ];
    aggregate_batch_groups(&mut vms);
    // running agent 打断连续区间，不应聚合
    assert_eq!(vms.len(), 3, "中间有 is_running=true 时不合并");
}

#[test]
fn test_aggregate_batch_groups_mixed_batch() {
    // 3 完成 + 1 running + 2 完成 = 两个独立的聚合区间
    // 但由于 running 打断，每个区间只有 1 个或 2 个
    let mut vms = vec![
        make_done_subagent("agent-1", "task one"),
        make_done_subagent("agent-2", "task two"),
        make_done_subagent("agent-3", "task three"),
        make_running_subagent("agent-4", "task four"),
        make_done_subagent("agent-5", "task five"),
        make_done_subagent("agent-6", "task six"),
    ];
    aggregate_batch_groups(&mut vms);
    // 前 3 个合并为 1，running 保持独立，后 2 个合并为 1
    assert_eq!(vms.len(), 3, "3+1+2 = 两个聚合区间 + 1 个 running");
    // 第一个是合并后的
    if let MessageViewModel::SubAgentGroup { batch_agents, .. } = &vms[0] {
        assert_eq!(batch_agents.len(), 3);
    } else {
        panic!("第一个应为聚合的 SubAgentGroup");
    }
    // 第二个是 running 的
    if let MessageViewModel::SubAgentGroup { is_running, .. } = &vms[1] {
        assert!(*is_running);
    } else {
        panic!("第二个应为 running 的 SubAgentGroup");
    }
    // 第三个是合并后的
    if let MessageViewModel::SubAgentGroup { batch_agents, .. } = &vms[2] {
        assert_eq!(batch_agents.len(), 2);
    } else {
        panic!("第三个应为聚合的 SubAgentGroup");
    }
}

#[test]
fn test_aggregate_batch_groups_already_aggregated_skip() {
    // 已聚合的 SubAgentGroup（batch_agents 非空）不参与二次聚合
    let mut aggregated = MessageViewModel::SubAgentGroup {
        agent_id: "agent-1".to_string(),
        task_preview: "task one".to_string(),
        total_steps: 3,
        recent_messages: Vec::new(),
        is_running: false,
        collapsed: true,
        final_result: Some("done".to_string()),
        is_error: false,
        is_background: false,
        bg_hash: Some("batch01".to_string()),
        batch_agents: vec![
            AgentSummary {
                agent_id: "agent-1".to_string(),
                task_preview: "task one".to_string(),
                tool_count: 3,
                is_error: false,
                final_result: Some("done".to_string()),
            },
            AgentSummary {
                agent_id: "agent-2".to_string(),
                task_preview: "task two".to_string(),
                tool_count: 5,
                is_error: false,
                final_result: Some("done".to_string()),
            },
        ],
        instance_id: None,
        content_hash: 0,
    };
    aggregated.recompute_hash();
    let mut vms = vec![aggregated.clone()];
    aggregate_batch_groups(&mut vms);
    assert_eq!(vms.len(), 1, "已聚合的不应二次聚合");
    if let MessageViewModel::SubAgentGroup { batch_agents, .. } = &vms[0] {
        assert_eq!(batch_agents.len(), 2, "batch_agents 应保持不变");
    } else {
        panic!("应为 SubAgentGroup");
    }
}

#[test]
fn test_agent_summary_truncation() {
    let long_task = "这是一个非常非常非常非常非常非常非常非常非常非常非常非常长的任务描述需要超过五十个字符的长度才能触发截断逻辑验证";
    assert!(long_task.chars().count() > 50, "测试数据应超过 50 字符");
    let mut vms = vec![
        make_done_subagent("agent-1", long_task),
        make_done_subagent("agent-2", "short task"),
    ];
    aggregate_batch_groups(&mut vms);
    if let MessageViewModel::SubAgentGroup { batch_agents, .. } = &vms[0] {
        assert_eq!(
            batch_agents[0].task_preview.chars().count(),
            50,
            "task_preview 应截断到 50 字符"
        );
        assert_eq!(batch_agents[1].task_preview, "short task", "短文本不应截断");
    } else {
        panic!("应为 SubAgentGroup");
    }
}

#[test]
fn test_batch_group_default_collapsed() {
    let mut vms = vec![
        make_done_subagent("agent-1", "task one"),
        make_done_subagent("agent-2", "task two"),
    ];
    aggregate_batch_groups(&mut vms);
    if let MessageViewModel::SubAgentGroup { collapsed, .. } = &vms[0] {
        assert!(*collapsed, "合并后应默认折叠");
    } else {
        panic!("应为 SubAgentGroup");
    }
}

// ── content_hash 测试 ──

#[test]
fn test_content_hash_user_bubble_consistent() {
    let vm1 = MessageViewModel::user("Hello".to_string());
    let vm2 = MessageViewModel::user("Hello".to_string());
    assert_eq!(
        vm1.content_hash(),
        vm2.content_hash(),
        "相同内容的 hash 应一致"
    );
}

#[test]
fn test_content_hash_user_bubble_different() {
    let vm1 = MessageViewModel::user("Hello".to_string());
    let vm2 = MessageViewModel::user("World".to_string());
    assert_ne!(
        vm1.content_hash(),
        vm2.content_hash(),
        "不同内容的 hash 应不同"
    );
}

#[test]
fn test_content_hash_system_note() {
    let vm = MessageViewModel::system("test note".to_string());
    assert_ne!(vm.content_hash(), 0, "hash 不应为 0（几乎不可能碰撞）");
}

#[test]
fn test_content_hash_matches_compute_hash() {
    // 验证 content_hash 与 RenderTask::compute_hash 结果一致
    let vm = MessageViewModel::user("test content".to_string());
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    vm.hash(&mut hasher);
    let expected = hasher.finish();
    assert_eq!(
        vm.content_hash(),
        expected,
        "content_hash 应与手动 compute_hash 一致"
    );
}

#[test]
fn test_content_hash_assistant_bubble_changes_on_append() {
    let mut vm = MessageViewModel::assistant();
    let hash1 = vm.content_hash();
    vm.append_chunk("new text");
    // append_chunk 内部会 recompute_hash
    assert_ne!(vm.content_hash(), hash1, "append_chunk 后 hash 应变化");
}

#[test]
fn test_content_hash_assistant_bubble_changes_on_streaming() {
    let mut vm = MessageViewModel::assistant();
    let hash_streaming = vm.content_hash();
    // 模拟 is_streaming 变化
    if let MessageViewModel::AssistantBubble { is_streaming, .. } = &mut vm {
        *is_streaming = false;
    }
    vm.recompute_hash();
    assert_ne!(
        vm.content_hash(),
        hash_streaming,
        "is_streaming 变化后 hash 应不同"
    );
}

#[test]
fn test_content_hash_subagent_group() {
    let vm = MessageViewModel::subagent_group("explorer".to_string(), "explore".to_string());
    assert_ne!(vm.content_hash(), 0);
}

#[test]
fn test_content_hash_cache_warning() {
    let vm = MessageViewModel::cache_warning("low cache".to_string());
    assert_ne!(vm.content_hash(), 0);
}

#[test]
fn test_recompute_hash_idempotent() {
    let mut vm = MessageViewModel::user("stable".to_string());
    let h1 = vm.content_hash();
    vm.recompute_hash();
    let h2 = vm.content_hash();
    assert_eq!(h1, h2, "未修改内容时 recompute_hash 应幂等");
}

#[test]
fn test_human_message_with_system_reminder_detection() {
    let text = format!(
        "<system-reminder>\nThis session continues from a previous conversation.\n## Summary\nDone.\n{}\n</system-reminder>",
        peri_agent::agent::compact::CONTINUATION_HINT
    );
    let msg = BaseMessage::human(text);
    let vm = MessageViewModel::from_base_message(&msg, &[]);
    match vm {
        MessageViewModel::UserBubble {
            system_reminder,
            content,
            ..
        } => {
            assert!(system_reminder, "应识别为系统提醒");
            assert!(
                !content.contains("<system-reminder>"),
                "标签应被剥离: {}",
                content
            );
            assert!(!content.contains("</system-reminder>"), "结束标签应被剥离");
            assert!(content.contains("## Summary"), "正文应保留");
            assert!(
                content.contains(peri_agent::agent::compact::CONTINUATION_HINT),
                "续接指令应保留"
            );
        }
        _ => panic!("应为 UserBubble"),
    }
}

#[test]
fn test_human_message_without_system_reminder() {
    let text = "普通用户消息";
    let msg = BaseMessage::human(text);
    let vm = MessageViewModel::from_base_message(&msg, &[]);
    match vm {
        MessageViewModel::UserBubble {
            system_reminder, ..
        } => {
            assert!(!system_reminder, "普通消息不应标记为系统提醒");
        }
        _ => panic!("应为 UserBubble"),
    }
}

#[test]
fn test_goal_steering_system_reminder_不折叠为_compact() {
    // GoalMiddleware 注入的 steering 用 <goal-message> 包裹（区别于 compact 的 <system-reminder>）
    let text = "<goal-message>\n[Goal Steering] 目标: 完成重构\n请决策下一步...\n</goal-message>";
    let msg = BaseMessage::human(text);
    let vm = MessageViewModel::from_base_message(&msg, &[]);
    match vm {
        MessageViewModel::UserBubble {
            system_reminder,
            content,
            ..
        } => {
            assert!(!system_reminder, "goal steering 不应折叠为 compact 摘要");
            assert!(
                content.contains("<goal-message>"),
                "goal-message 标签应原样保留"
            );
            assert!(
                content.contains("[Goal Steering]"),
                "steering 内容应原样保留"
            );
        }
        _ => panic!("应为 UserBubble"),
    }
}

#[test]
fn test_system_reminder_partial_eq_different() {
    let msg1 = BaseMessage::human("普通消息");
    let reminder = format!(
        "<system-reminder>\n压缩摘要\n{}\n</system-reminder>",
        peri_agent::agent::compact::CONTINUATION_HINT
    );
    let msg2 = BaseMessage::human(reminder);
    let vm1 = MessageViewModel::from_base_message(&msg1, &[]);
    let vm2 = MessageViewModel::from_base_message(&msg2, &[]);
    assert_ne!(vm1, vm2, "系统提醒消息和普通消息不应相等");
}

#[test]
fn test_system_reminder_content_hash_different() {
    let msg1 = BaseMessage::human("普通消息");
    let reminder = format!(
        "<system-reminder>\n压缩摘要\n{}\n</system-reminder>",
        peri_agent::agent::compact::CONTINUATION_HINT
    );
    let msg2 = BaseMessage::human(reminder);
    let vm1 = MessageViewModel::from_base_message(&msg1, &[]);
    let vm2 = MessageViewModel::from_base_message(&msg2, &[]);
    assert_ne!(
        vm1.content_hash(),
        vm2.content_hash(),
        "content_hash 应不同"
    );
}
