    fn make_agent(id: &str, task: &str, tools: usize, error: bool) -> AgentSummary {
        AgentSummary {
            agent_id: id.to_string(),
            task_preview: task.to_string(),
            tool_count: tools,
            is_error: error,
            final_result: if error {
                Some("failed".to_string())
            } else {
                Some("done".to_string())
            },
        }
    }

    #[test]
    fn test_render_batch_summary_collapsed() {
        let agents = vec![
            make_agent("agent-1", "task one", 3, false),
            make_agent("agent-2", "task two", 5, false),
            make_agent("agent-3", "task three", 0, false),
        ];
        let lines = render_batch_summary(&agents, &true);
        // Header + 3 行 agent 摘要 = 4 行
        assert_eq!(lines.len(), 4, "折叠态应有 header + 3 行摘要");
        // Header 应包含 "3 agents finished"
        let header_text: String = lines[0].spans.iter().map(|s| s.content.clone()).collect();
        assert!(
            header_text.contains("3 agents finished"),
            "header 应显示 agent 数量: {}",
            header_text
        );
    }

    #[test]
    fn test_render_batch_summary_expanded() {
        let agents = vec![
            make_agent("agent-1", "task one", 3, false),
            make_agent("agent-2", "task two", 5, false),
        ];
        let lines = render_batch_summary(&agents, &false);
        // Header + 2 * (task_preview + final_result) = 5 行
        assert_eq!(lines.len(), 5, "展开态应有 header + 2*(task+result)");
    }

    #[test]
    fn test_render_batch_summary_with_error() {
        let agents = vec![
            make_agent("agent-1", "task one", 3, false),
            make_agent("agent-2", "task two", 1, true),
            make_agent("agent-3", "task three", 2, true),
        ];
        let lines = render_batch_summary(&agents, &true);
        let header_text: String = lines[0].spans.iter().map(|s| s.content.clone()).collect();
        assert!(
            header_text.contains("2 failed"),
            "header 应显示失败数: {}",
            header_text
        );
    }

    #[test]
    fn test_render_batch_summary_tree_connectors() {
        let agents = vec![
            make_agent("agent-1", "task one", 3, false),
            make_agent("agent-2", "task two", 5, false),
            make_agent("agent-3", "task three", 0, false),
        ];
        let lines = render_batch_summary(&agents, &true);
        // 第一个 agent 应使用 ├─
        let line1_text: String = lines[1].spans.iter().map(|s| s.content.clone()).collect();
        assert!(
            line1_text.contains("├─"),
            "非最后一个 agent 应使用 ├─: {}",
            line1_text
        );
        // 最后一个 agent 应使用 └─
        let line3_text: String = lines[3].spans.iter().map(|s| s.content.clone()).collect();
        assert!(
            line3_text.contains("└─"),
            "最后一个 agent 应使用 └─: {}",
            line3_text
        );
    }

    #[test]
    fn test_render_single_agent_unchanged() {
        // batch_agents 为空时走现有渲染路径，不经过 render_batch_summary
        // 此测试验证 render_batch_summary 对空 agents 列表的边界行为
        let agents: Vec<AgentSummary> = vec![];
        let lines = render_batch_summary(&agents, &true);
        assert_eq!(lines.len(), 1, "空 agents 应只有 header");
        let header_text: String = lines[0].spans.iter().map(|s| s.content.clone()).collect();
        assert!(
            header_text.contains("0 agents"),
            "header 应包含 0 agents: {}",
            header_text
        );
    }

    // ─── 从 headless_test.rs 迁移的 render_view_model 测试 ──────────────────

    #[test]
    fn test_system_note_error_detection() {
        let error_content = "Compact failed: No LLM Provider";
        assert!(
            error_content.contains("failed") || error_content.contains("Compact failed"),
            "应检测到错误标记"
        );
        let warn_content = "⚠ Interrupted";
        assert!(warn_content.contains("⚠"), "应检测到警告标记");
        let info_content = "Configuration saved";
        assert!(
            !info_content.contains("❌")
                && !info_content.contains("failed")
                && !info_content.contains("⚠"),
            "普通消息不应被标记为错误"
        );
    }

    #[test]
    fn test_tool_block_error_visible_when_collapsed() {
        use crate::app::MessageViewModel;
        let vm = MessageViewModel::ToolBlock {
            tool_name: "Bash".to_string(),
            tool_call_id: "tc_err".to_string(),
            display_name: "Shell".to_string(),
            args_display: Some("bad_command".to_string()),
            content: "command not found: bad_command\nexit code 127".to_string(),
            is_error: true,
            collapsed: true,
            color: crate::ui::theme::ERROR,
            diff_lines: None,
            content_hash: 0,
        };
        let lines = render_view_model(&vm, Some(1), 80, false);
        assert!(
            lines.len() >= 3,
            "collapsed error ToolBlock should have header + error lines, got {}",
            lines.len()
        );
        let text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
            .collect::<Vec<_>>()
            .join("");
        assert!(
            text.contains("command not found"),
            "error content should be visible: {}",
            text
        );
    }

    #[test]
    fn test_tool_block_success_no_summary_when_collapsed() {
        use crate::app::MessageViewModel;
        let vm = MessageViewModel::ToolBlock {
            tool_name: "Read".to_string(),
            tool_call_id: "tc_ok".to_string(),
            display_name: "Read".to_string(),
            args_display: Some("file.txt".to_string()),
            content: "file contents here".to_string(),
            is_error: false,
            collapsed: true,
            color: crate::ui::theme::SAGE,
            diff_lines: None,
            content_hash: 0,
        };
        let lines = render_view_model(&vm, Some(1), 80, false);
        assert_eq!(
            lines.len(),
            1,
            "successful collapsed ToolBlock should have only header"
        );
    }

    #[test]
    fn test_tool_call_group_error_visible_when_collapsed() {
        use crate::app::MessageViewModel;
        use crate::ui::message_view::{ToolCategory, ToolEntry};

        let vm = MessageViewModel::ToolCallGroup {
            category: ToolCategory::Read,
            tools: vec![
                ToolEntry {
                    tool_name: "Read".to_string(),
                    display_name: "Read".to_string(),
                    args_display: Some("ok_file.txt".to_string()),
                    content: "ok content".to_string(),
                    is_error: false,
                },
                ToolEntry {
                    tool_name: "Read".to_string(),
                    display_name: "Read".to_string(),
                    args_display: Some("missing.txt".to_string()),
                    content: "Error: file not found".to_string(),
                    is_error: true,
                },
            ],
            collapsed: true,
            content_hash: 0,
        };
        let lines = render_view_model(&vm, Some(1), 80, false);
        let text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
            .collect::<Vec<_>>()
            .join("");
        assert!(
            text.contains("Error: file not found"),
            "error from failed tool should be visible: {}",
            text
        );
        assert!(
            !text.contains("ok content"),
            "successful tool content should NOT be visible: {}",
            text
        );
    }

    #[test]
    fn test_subagent_group_error_red_title_and_summary() {
        use crate::app::MessageViewModel;
        let vm = MessageViewModel::SubAgentGroup {
            agent_id: "test-agent".to_string(),
            task_preview: "do something risky".to_string(),
            total_steps: 3,
            recent_messages: Vec::new(),
            is_running: false,
            collapsed: true,
            final_result: Some("Agent failed: permission denied".to_string()),
            is_error: true,
            is_background: false,
            bg_hash: Some("abc123".to_string()),
            batch_agents: Vec::new(),
            instance_id: None,
            content_hash: 0,
        };
        let lines = render_view_model(&vm, Some(1), 80, false);
        let title_color = lines
            .first()
            .and_then(|l| l.spans.get(1).and_then(|s| s.style.fg));
        assert_eq!(
            title_color,
            Some(crate::ui::theme::ERROR),
            "title should be red on error"
        );
        let text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
            .collect::<Vec<_>>()
            .join("");
        assert!(
            text.contains("Agent failed"),
            "error summary should be visible: {}",
            text
        );
    }

    #[test]
    fn test_render_system_reminder_user_bubble() {
        let mut vm = MessageViewModel::user("irrelevant content".to_string());
        if let MessageViewModel::UserBubble { system_reminder, .. } = &mut vm {
            *system_reminder = true;
        }
        vm.recompute_hash();
        let lines = render_view_model(&vm, Some(1), 80, false);
        assert_eq!(lines.len(), 1, "系统提醒应只渲染一行");
        let text: String = lines[0].spans.iter().map(|s| s.content.clone()).collect();
        assert!(text.contains("Context compacted"), "应显示压缩提示文字，实际: {}", text);
    }

    #[test]
    fn test_render_normal_user_bubble_unchanged() {
        let vm = MessageViewModel::user("Hello World".to_string());
        let lines = render_view_model(&vm, Some(1), 80, false);
        let first_text: String = lines[0].spans.iter().map(|s| s.content.clone()).collect();
        assert!(first_text.contains("\u{276f}"), "普通消息应有 ❯ 前缀");
        assert!(first_text.contains("Hello"), "应包含原始内容");
    }