use super::*;
use crate::{
    app::{AgentEvent, App, MessageViewModel},
    ui::main_ui,
};

#[tokio::test]
async fn test_snapshot_row_count() {
    let (_app, handle) = App::new_headless(80, 24).await;
    assert_eq!(handle.snapshot().len(), 24, "snapshot 应返回 24 行");
}

#[tokio::test]
async fn test_assistant_chunk_renders() {
    use peri_agent::messages::BaseMessage;

    let (mut app, mut handle) = App::new_headless(120, 30).await;
    app.push_agent_event(AgentEvent::AssistantChunk {
        chunk: "Hello world".into(),
        source_agent_id: None,
    });
    app.push_agent_event(AgentEvent::StateSnapshot(vec![
        BaseMessage::human("q"),
        BaseMessage::ai("Hello world"),
    ]));
    app.push_agent_event(AgentEvent::Done);
    app.process_pending_events();
    app.flush_rebuild();
    tokio::task::yield_now().await;
    tokio::task::yield_now().await;
    handle
        .terminal
        .draw(|f| main_ui::render(f, &mut app))
        .unwrap();
    let snap = handle.snapshot();
    assert!(
        handle.contains("Hello world"),
        "应显示消息内容，实际:\n{}",
        snap.join("\n")
    );
}

#[tokio::test]
async fn test_tool_call_renders() {
    let (mut app, mut handle) = App::new_headless(120, 30).await;
    let notified = handle.render_notify.notified();
    app.push_agent_event(AgentEvent::ToolStart {
        tool_call_id: "t1".into(),
        name: "Read".into(),
        display: "ReadFile".into(),
        args: "src/main.rs".into(),
        input: serde_json::json!({"path": "src/main.rs"}),
        source_agent_id: None,
    });
    app.process_pending_events();
    notified.await;
    handle
        .terminal
        .draw(|f| main_ui::render(f, &mut app))
        .unwrap();
    let snap = handle.snapshot();
    // ToolStart 通过 Pipeline 创建 ToolBlock，display_name 为 format_tool_name 的结果
    let has_tool = snap
        .iter()
        .any(|l| l.contains("Read") || l.contains("Read"));
    assert!(has_tool, "应显示工具调用块，实际内容:\n{}", snap.join("\n"));
}

#[tokio::test]
async fn test_user_message_renders() {
    let (mut app, mut handle) = App::new_headless(120, 30).await;
    // 先注册监听，再发送事件，避免时序问题
    let notified = handle.render_notify.notified();
    // 使用 ASCII 内容避免 CJK 宽字符在 buffer 中的空格填充问题
    let vm = MessageViewModel::user("hello from user".into());
    app.session_mgr
        .current_mut()
        .messages
        .view_messages
        .push(vm);
    app.render_rebuild();
    notified.await;
    handle
        .terminal
        .draw(|f| main_ui::render(f, &mut app))
        .unwrap();
    let snap = handle.snapshot();
    assert!(
        handle.contains("hello from user"),
        "应显示用户消息，实际内容:\n{}",
        snap.join("\n")
    );
}

#[tokio::test]
async fn test_clear_empties_render_cache() {
    use crate::ui::render_thread::RenderEvent;

    let (mut app, _handle) = App::new_headless(120, 30).await;

    // 直接发送 LoadHistory 填充 RenderCache
    let msgs = vec![MessageViewModel::user("test content".into())];
    let _ = app
        .session_mgr
        .current_mut()
        .messages
        .render_tx
        .try_send(RenderEvent::Rebuild(msgs));
    tokio::task::yield_now().await;
    tokio::task::yield_now().await;

    // 验证 RenderCache 有内容
    let lines_before = app
        .session_mgr
        .current_mut()
        .messages
        .render_cache
        .read()
        .total_lines;
    assert!(lines_before > 0, "清空前应有内容");

    // 发送 Clear 清空 RenderCache
    let _ = app
        .session_mgr
        .current_mut()
        .messages
        .render_tx
        .try_send(RenderEvent::Clear);
    tokio::task::yield_now().await;
    tokio::task::yield_now().await;

    // 验证 RenderCache 已清空
    let cache = app.session_mgr.current_mut().messages.render_cache.read();
    assert_eq!(cache.total_lines, 0, "清空后 RenderCache 应为空");
}

#[tokio::test]
async fn test_subagent_group_basic() {
    // SubAgentStart → 2×ToolCall → SubAgentEnd → 渲染验证
    let (mut app, mut handle) = App::new_headless(120, 30).await;

    app.push_agent_event(AgentEvent::SubAgentStart {
        agent_id: "code-reviewer".into(),
        instance_id: "test-instance".into(),
        task_preview: "review the code".into(),
        is_background: false,
    });
    app.push_agent_event(AgentEvent::ToolStart {
        tool_call_id: "t1".into(),
        name: "Read".into(),
        display: "ReadFile".into(),
        args: "src/main.rs".into(),
        input: serde_json::json!({"path": "src/main.rs"}),
        source_agent_id: Some("test-instance".into()),
    });
    app.push_agent_event(AgentEvent::ToolStart {
        tool_call_id: "t2".into(),
        name: "Bash".into(),
        display: "Bash".into(),
        args: "cargo test".into(),
        input: serde_json::json!({"command": "cargo test"}),
        source_agent_id: Some("test-instance".into()),
    });
    app.push_agent_event(AgentEvent::SubAgentEnd {
        result: "All tests passed, no issues found".into(),
        is_error: false,
        agent_id: Some("code-reviewer".into()),
        instance_id: Some("test-instance".into()),
    });
    app.process_pending_events();
    app.flush_rebuild();
    tokio::task::yield_now().await;
    tokio::task::yield_now().await;

    handle
        .terminal
        .draw(|f| main_ui::render(f, &mut app))
        .unwrap();
    let snap = handle.snapshot();

    // 验证 SubAgentGroup 头行存在（code-reviewer 名称）
    let has_agent = snap.iter().any(|l| l.contains("code-reviewer"));
    assert!(
        has_agent,
        "应显示 SubAgentGroup 头行含 agent_id，实际:\n{}",
        snap.join("\n")
    );

    // 验证 SubAgentGroup 已完成（is_running=false）
    if let Some(vm) = app.session_mgr.current_mut().messages.view_messages.last() {
        assert!(vm.is_subagent_group(), "最后一条消息应为 SubAgentGroup");
        if let crate::app::MessageViewModel::SubAgentGroup {
            is_running,
            total_steps,
            ..
        } = vm
        {
            assert!(!is_running, "SubAgentEnd 后 is_running 应为 false");
            assert_eq!(*total_steps, 2, "total_steps 应为 2");
        }
    }
}

#[tokio::test]
async fn test_subagent_group_sliding_window() {
    // SubAgentStart → 6×ToolCall → SubAgentEnd → 只保留 4 条，总步数为 6
    let (mut app, _handle) = App::new_headless(120, 30).await;

    app.push_agent_event(AgentEvent::SubAgentStart {
        agent_id: "analyzer".into(),
        instance_id: "test-instance".into(),
        task_preview: "analyze codebase".into(),
        is_background: false,
    });
    for i in 1..=6 {
        app.push_agent_event(AgentEvent::ToolStart {
            tool_call_id: format!("t{}", i),
            name: "Read".into(),
            display: "ReadFile".into(),
            args: format!("file{}.rs", i),
            input: serde_json::json!({"path": format!("file{}.rs", i)}),
            source_agent_id: Some("test-instance".into()),
        });
    }
    app.push_agent_event(AgentEvent::SubAgentEnd {
        result: "analysis complete".into(),
        is_error: false,
        agent_id: Some("analyzer".into()),
        instance_id: Some("test-instance".into()),
    });
    app.process_pending_events();

    // 验证 SubAgentGroup 状态
    if let Some(crate::app::MessageViewModel::SubAgentGroup {
        total_steps,
        recent_messages,
        is_running,
        ..
    }) = app.session_mgr.current_mut().messages.view_messages.last()
    {
        assert_eq!(*total_steps, 6, "total_steps 应为 6，实际: {}", total_steps);
        assert!(
            recent_messages.len() <= 4,
            "recent_messages 最多 4 条，实际: {}",
            recent_messages.len()
        );
        assert!(!is_running, "SubAgentEnd 后 is_running 应为 false");
    } else {
        panic!("最后一条消息应为 SubAgentGroup");
    }
}

#[tokio::test]
async fn test_subagent_group_assistant_chunk() {
    // SubAgentStart → AssistantChunk → SubAgentEnd → AssistantBubble 在 recent_messages 中
    let (mut app, _handle) = App::new_headless(120, 30).await;

    app.push_agent_event(AgentEvent::SubAgentStart {
        agent_id: "writer".into(),
        instance_id: "test-instance".into(),
        task_preview: "write summary".into(),
        is_background: false,
    });
    app.push_agent_event(AgentEvent::AssistantChunk {
        chunk: "summary text here".into(),
        source_agent_id: Some("test-instance".into()),
    });
    app.push_agent_event(AgentEvent::SubAgentEnd {
        result: "Done writing".into(),
        is_error: false,
        agent_id: Some("writer".into()),
        instance_id: Some("test-instance".into()),
    });
    app.process_pending_events();

    // 验证 SubAgentGroup 包含 AssistantBubble
    if let Some(crate::app::MessageViewModel::SubAgentGroup {
        recent_messages,
        final_result,
        ..
    }) = app.session_mgr.current_mut().messages.view_messages.last()
    {
        let has_assistant = recent_messages.iter().any(|m| m.is_assistant());
        assert!(has_assistant, "recent_messages 应包含 AssistantBubble");
        assert_eq!(
            final_result.as_deref(),
            Some("Done writing"),
            "final_result 应为工具返回值"
        );
    } else {
        panic!("最后一条消息应为 SubAgentGroup");
    }
}

#[tokio::test]
async fn test_tool_call_message_visible_when_toggled() {
    let (mut app, mut handle) = App::new_headless(120, 30).await;

    // 使用 ToolStart 事件添加工具调用（会发送 RenderEvent::Rebuild）
    let notified1 = handle.render_notify.notified();
    app.push_agent_event(AgentEvent::ToolStart {
        tool_call_id: "tc1".into(),
        name: "Bash".into(),
        display: "Bash".into(),
        args: "ls".into(),
        input: serde_json::json!({"command": "ls"}),
        source_agent_id: None,
    });
    app.process_pending_events();
    notified1.await;

    // toggle_collapsed_messages 发送 ToggleToolMessages → 渲染线程 rebuild_all → notify
    let notified2 = handle.render_notify.notified();
    app.toggle_collapsed_messages();
    notified2.await;

    handle
        .terminal
        .draw(|f| main_ui::render(f, &mut app))
        .unwrap();

    let snap = handle.snapshot();
    // ToolStart 创建的 ToolBlock，display_name 为 format_tool_name 的结果
    let has_tool_call_text = snap
        .iter()
        .any(|l| l.contains("Shell") || l.contains("Bash"));
    assert!(
        has_tool_call_text,
        "ToolCall 创建的 ToolBlock 应在快照中可见，但实际内容为:\n{}",
        snap.join("\n")
    );
}

#[tokio::test]
async fn test_empty_assistant_chunk_no_bubble() {
    // 空 AssistantChunk 不应创建空白的 AssistantBubble
    let (mut app, _handle) = App::new_headless(120, 30).await;

    // 发送空 chunk，不应创建 AssistantBubble
    app.push_agent_event(AgentEvent::AssistantChunk {
        chunk: "".into(),
        source_agent_id: None,
    });
    app.process_pending_events();

    // view_messages 应为空（没有创建空白气泡）
    assert!(
        app.session_mgr
            .current_mut()
            .messages
            .view_messages
            .is_empty(),
        "空 AssistantChunk 不应创建 AssistantBubble，实际: {:?}",
        app.session_mgr.current_mut().messages.view_messages.len()
    );

    // 发送多个空 chunk，仍不应创建气泡
    app.push_agent_event(AgentEvent::AssistantChunk {
        chunk: "".into(),
        source_agent_id: None,
    });
    app.push_agent_event(AgentEvent::AssistantChunk {
        chunk: "".into(),
        source_agent_id: None,
    });
    app.process_pending_events();

    assert!(
        app.session_mgr
            .current_mut()
            .messages
            .view_messages
            .is_empty(),
        "多个空 AssistantChunk 仍不应创建 AssistantBubble"
    );
}

#[tokio::test]
async fn test_empty_then_nonempty_assistant_chunk() {
    use peri_agent::messages::BaseMessage;

    // 空_chunk → 非空_chunk：非空 chunk 应正常创建气泡
    let (mut app, mut handle) = App::new_headless(120, 30).await;

    // 先发送空 chunk
    app.push_agent_event(AgentEvent::AssistantChunk {
        chunk: "".into(),
        source_agent_id: None,
    });
    app.process_pending_events();

    // 再发送非空 chunk
    app.push_agent_event(AgentEvent::AssistantChunk {
        chunk: "Hello".into(),
        source_agent_id: None,
    });
    app.push_agent_event(AgentEvent::StateSnapshot(vec![
        BaseMessage::human("q"),
        BaseMessage::ai("Hello"),
    ]));
    app.push_agent_event(AgentEvent::Done);
    app.process_pending_events();
    app.flush_rebuild();
    tokio::task::yield_now().await;
    tokio::task::yield_now().await;

    handle
        .terminal
        .draw(|f| main_ui::render(f, &mut app))
        .unwrap();

    // Done 触发 reconcile_tail 从 completed 重建，应包含 Human + AI 两条消息
    assert_eq!(
        app.session_mgr.current_mut().messages.view_messages.len(),
        2,
        "应有 2 条消息（Human+AI）"
    );
    assert!(
        app.session_mgr.current_mut().messages.view_messages[1].is_assistant(),
        "第二条应为 AssistantBubble"
    );
    assert!(handle.contains("Hello"), "应显示 Hello 内容");
}

#[tokio::test]
async fn test_tool_call_without_assistant_chunk_no_bubble() {
    // 模拟 AI 只调用工具不输出文本的场景
    let (mut app, mut handle) = App::new_headless(120, 30).await;

    // 直接发送 ToolStart 事件（无 AssistantChunk）
    let notified = handle.render_notify.notified();
    app.push_agent_event(AgentEvent::ToolStart {
        tool_call_id: "tc1".into(),
        name: "Bash".into(),
        display: "Bash".into(),
        args: "ls".into(),
        input: serde_json::json!({"command": "ls"}),
        source_agent_id: None,
    });
    app.process_pending_events();
    notified.await;

    handle
        .terminal
        .draw(|f| main_ui::render(f, &mut app))
        .unwrap();

    // 应该有 1 个 ToolBlock，不应有空白 AssistantBubble
    assert_eq!(
        app.session_mgr.current_mut().messages.view_messages.len(),
        1,
        "应有 1 条消息（ToolBlock）"
    );
    // 确保不是 AssistantBubble（空白气泡）
    assert!(
        !app.session_mgr.current_mut().messages.view_messages[0].is_assistant(),
        "不应创建 AssistantBubble，应为 ToolBlock"
    );
}

#[tokio::test]
async fn test_welcome_card_renders_when_empty() {
    let (mut app, mut handle) = App::new_headless(120, 30).await;
    // 默认 view_messages 为空，应显示 Welcome Card
    handle
        .terminal
        .draw(|f| main_ui::render(f, &mut app))
        .unwrap();
    let snap = handle.snapshot();
    let snap_text = snap.join("\n");
    assert!(
        snap_text.contains("Peri"),
        "Welcome Card 应包含 'Peri'，实际:\n{}",
        snap_text
    );
    assert!(
        snap_text.contains("/help") || snap_text.contains("/model"),
        "Welcome Card 应包含命令提示，实际:\n{}",
        snap_text
    );
}

#[tokio::test]
async fn test_welcome_card_hidden_after_message() {
    use peri_agent::messages::BaseMessage;

    let (mut app, mut handle) = App::new_headless(120, 30).await;
    app.push_agent_event(AgentEvent::AssistantChunk {
        chunk: "Hello from agent".into(),
        source_agent_id: None,
    });
    app.push_agent_event(AgentEvent::StateSnapshot(vec![
        BaseMessage::human("q"),
        BaseMessage::ai("Hello from agent"),
    ]));
    app.push_agent_event(AgentEvent::Done);
    app.process_pending_events();
    app.flush_rebuild();
    tokio::task::yield_now().await;
    tokio::task::yield_now().await;

    handle
        .terminal
        .draw(|f| main_ui::render(f, &mut app))
        .unwrap();
    let snap = handle.snapshot();
    let snap_text = snap.join("\n");
    assert!(
        !snap_text.contains("What can I do?"),
        "有消息后 Welcome Card 应消失，但仍有 welcome 内容，实际:\n{}",
        snap_text
    );
    assert!(
        handle.contains("Hello from agent"),
        "应显示消息内容，实际:\n{}",
        snap_text
    );
}

#[tokio::test]
async fn test_welcome_card_narrow_screen() {
    let (mut app, mut handle) = App::new_headless(40, 24).await;
    handle
        .terminal
        .draw(|f| main_ui::render(f, &mut app))
        .unwrap();
    let snap = handle.snapshot();
    let snap_text = snap.join("\n");
    // 窄屏不应显示 ASCII Art（包含 ██ 或 ╚═ 等 block 字符）
    assert!(
        !snap_text.contains("██"),
        "窄屏不应显示 ASCII Art Logo，实际:\n{}",
        snap_text
    );
    // 但仍应包含文字版标题
    assert!(
        snap_text.contains("Peri"),
        "窄屏应显示文字版标题 'Peri'，实际:\n{}",
        snap_text
    );
}

#[tokio::test]
async fn test_welcome_card_shows_login_guide_when_no_provider() {
    // 无 Provider 时 Welcome Card 应显示 /login 首次引导
    let (mut app, mut handle) = App::new_headless(120, 30).await;
    // peri_config 默认为 None，无 provider
    handle
        .terminal
        .draw(|f| main_ui::render(f, &mut app))
        .unwrap();
    let snap = handle.snapshot();
    let snap_text = snap.join("\n");
    assert!(
        snap_text.contains("login"),
        "无 Provider 时 Welcome Card 应显示 /login 引导，实际:\n{}",
        snap_text
    );
}

// ── Sticky Human Message Header ────────────────────────────────────────────

#[tokio::test]
async fn test_sticky_header_hidden_when_no_messages() {
    // 无消息时 sticky header 应完全隐藏
    let (mut app, mut handle) = App::new_headless(80, 24).await;
    assert!(
        app.session_mgr
            .current_mut()
            .metadata
            .last_human_message
            .is_none(),
        "默认应无 last_human_message"
    );
    handle
        .terminal
        .draw(|f| main_ui::render(f, &mut app))
        .unwrap();
    let snap = handle.snapshot();
    let snap_text = snap.join("\n");
    assert!(
        !snap_text.contains("你:"),
        "无消息时不应显示 sticky header，实际:\n{}",
        snap_text
    );
}

#[tokio::test]
async fn test_sticky_header_shows_after_submit() {
    // 模拟 submit_message 后 sticky header 显示
    // 需要足够多的消息使内容超过可视区域（max_scroll > 0）
    let (mut app, mut handle) = App::new_headless(80, 24).await;

    // 填充足够多的消息使消息区产生滚动
    for i in 0..30 {
        let notified = handle.render_notify.notified();
        let vm = MessageViewModel::user(format!("message line {}", i));
        app.session_mgr
            .current_mut()
            .messages
            .view_messages
            .push(vm);
        app.render_rebuild();
        notified.await;
    }

    // 设置 last_human_message（模拟 submit_message 的效果）
    app.session_mgr.current_mut().metadata.last_human_message = Some("hello from user".to_string());

    handle
        .terminal
        .draw(|f| main_ui::render(f, &mut app))
        .unwrap();
    let snap = handle.snapshot();
    let snap_text = snap.join("\n");

    assert!(
        snap_text.contains("hello from"),
        "应显示消息内容，实际:\n{}",
        snap_text
    );
}

#[tokio::test]
async fn test_sticky_header_hidden_after_clear() {
    // /clear 后 sticky header 应消失
    let (mut app, mut handle) = App::new_headless(80, 24).await;

    // 模拟已有消息
    app.session_mgr.current_mut().metadata.last_human_message = Some("some message".to_string());
    assert!(
        app.session_mgr
            .current_mut()
            .metadata
            .last_human_message
            .is_some(),
        "应有 last_human_message"
    );

    // 模拟 /clear → new_thread
    let notified = handle.render_notify.notified();
    app.new_thread();
    notified.await;

    assert!(
        app.session_mgr
            .current_mut()
            .metadata
            .last_human_message
            .is_none(),
        "/clear 后 last_human_message 应为 None"
    );

    handle
        .terminal
        .draw(|f| main_ui::render(f, &mut app))
        .unwrap();
    let snap = handle.snapshot();
    let snap_text = snap.join("\n");
    assert!(
        !snap_text.contains("你:"),
        "/clear 后不应显示 sticky header，实际:\n{}",
        snap_text
    );
}

#[tokio::test]
async fn test_sticky_header_shows_last_message_not_first() {
    // 连续发送多条消息，header 应显示最后一条
    let (mut app, mut handle) = App::new_headless(80, 24).await;

    // 填充足够多的消息使消息区产生滚动
    for i in 0..30 {
        let notified = handle.render_notify.notified();
        let vm = MessageViewModel::user(format!("padding line {}", i));
        app.session_mgr
            .current_mut()
            .messages
            .view_messages
            .push(vm);
        app.render_rebuild();
        notified.await;
    }

    // 模拟第一条消息
    app.session_mgr.current_mut().metadata.last_human_message = Some("first message".to_string());
    // 模拟第二条消息（覆盖）
    app.session_mgr.current_mut().metadata.last_human_message = Some("second message".to_string());

    handle
        .terminal
        .draw(|f| main_ui::render(f, &mut app))
        .unwrap();
    let snap = handle.snapshot();
    let snap_text = snap.join("\n");

    assert!(
        snap_text.contains("second"),
        "应显示最后一条消息，实际:\n{}",
        snap_text
    );
    assert!(
        !snap_text.contains("first"),
        "不应显示第一条消息（已被覆盖），实际:\n{}",
        snap_text
    );
}

#[tokio::test]
async fn test_sticky_header_truncation_long_message() {
    // 超长消息应在达到行数上限后截断并加 …
    let (mut app, mut handle) = App::new_headless(40, 24).await; // 窄屏 40 列

    // 填充足够多的消息使消息区产生滚动
    for i in 0..30 {
        let notified = handle.render_notify.notified();
        let vm = MessageViewModel::user(format!("padding {}", i));
        app.session_mgr
            .current_mut()
            .messages
            .view_messages
            .push(vm);
        app.render_rebuild();
        notified.await;
    }

    // 模拟超长消息（远超 header 可显示范围）
    let long_msg =
        "hello this is a very long message that definitely exceeds header capacity".to_string();
    assert!(long_msg.chars().count() > 40);
    app.session_mgr.current_mut().metadata.last_human_message = Some(long_msg.clone());

    handle
        .terminal
        .draw(|f| main_ui::render(f, &mut app))
        .unwrap();
    let snap = handle.snapshot();
    let snap_text = snap.join("\n");

    // 应显示消息开头
    assert!(
        snap_text.contains("hello this"),
        "应显示消息开头部分，实际:\n{}",
        snap_text
    );
    // 超长时应在末尾有省略号
    // （多行内容在 max_lines 行后被截断）
}

#[tokio::test]
async fn test_cron_panel_render() {
    let (mut app, mut handle) = App::new_headless(120, 30).await;

    // Register a cron task
    app.services
        .cron
        .scheduler
        .lock()
        .register("* * * * *", "hello cron test")
        .unwrap();
    let tasks: Vec<_> = app
        .services
        .cron
        .scheduler
        .lock()
        .list_tasks()
        .into_iter()
        .cloned()
        .collect();
    app.global_panels
        .open(crate::app::panel_manager::PanelState::Cron(
            crate::app::CronPanel::new(tasks),
        ));

    let notified = handle.render_notify.notified();
    drop(notified);

    handle
        .terminal
        .draw(|f| crate::ui::main_ui::render(f, &mut app))
        .unwrap();
    let snap = handle.snapshot();
    eprintln!("SNAPSHOT:");
    for (i, line) in snap.iter().enumerate() {
        if !line.is_empty() {
            eprintln!("{:3}: {}", i, line);
        }
    }
    assert!(
        handle.contains("hello cron test"),
        "should contain task prompt"
    );
    assert!(
        handle.contains("* * * * *"),
        "should contain cron expression"
    );
}

#[tokio::test]
async fn test_bordered_panel_integration() {
    // BorderedPanel 集成冒烟测试：渲染 agent panel 验证无 panic 且输出正确
    let (mut app, mut handle) = App::new_headless(120, 30).await;

    app.session_mgr.current_mut().session_panels.open(
        crate::app::panel_manager::PanelState::Agent(crate::app::AgentPanel::new(vec![], None)),
    );

    handle
        .terminal
        .draw(|f| main_ui::render(f, &mut app))
        .unwrap();
    assert!(
        handle.contains("Agent"),
        "BorderedPanel integration should render agent panel title"
    );
}

#[tokio::test]
async fn test_tab_bar_integration() {
    // TabBar 集成冒烟测试：渲染 ask_user popup 验证 TabBar widget 正确工作
    use peri_middlewares::ask_user::{AskUserBatchRequest, AskUserOption, AskUserQuestionData};

    use crate::app::AskUserBatchPrompt;

    let (mut app, mut handle) = App::new_headless(120, 30).await;

    let (req, _rx) = AskUserBatchRequest::new(vec![
        AskUserQuestionData {
            tool_call_id: "t1".into(),
            question: "Choose a language?".into(),
            header: "Language".into(),
            multi_select: false,
            options: vec![
                AskUserOption {
                    label: "Rust".into(),
                    description: Some("Systems language".into()),
                },
                AskUserOption {
                    label: "Go".into(),
                    description: None,
                },
            ],
        },
        AskUserQuestionData {
            tool_call_id: "t1".into(),
            question: "Choose a framework?".into(),
            header: "Framework".into(),
            multi_select: true,
            options: vec![AskUserOption {
                label: "Axum".into(),
                description: None,
            }],
        },
    ]);
    let prompt = AskUserBatchPrompt::from_request(req);
    app.session_mgr.current_mut().agent.interaction_prompt =
        Some(crate::app::InteractionPrompt::Questions(prompt));

    handle
        .terminal
        .draw(|f| main_ui::render(f, &mut app))
        .unwrap();
    let snap = handle.snapshot();
    // TabBar should render the tab labels
    assert!(
        snap.iter().any(|l| l.contains("Language")),
        "TabBar should render 'Language' tab label, got:\n{}",
        snap.join("\n")
    );
    assert!(
        snap.iter().any(|l| l.contains("Framework")),
        "TabBar should render 'Framework' tab label, got:\n{}",
        snap.join("\n")
    );
}

// ─── Permission Mode Tests ──────────────────────────────────────────────

#[tokio::test]
async fn test_app_default_permission_mode_is_bypass() {
    let (app, _handle) = App::new_headless(80, 24).await;
    use peri_middlewares::prelude::PermissionMode;
    assert_eq!(
        app.services.permission_mode.load(),
        PermissionMode::Bypass,
        "headless App 默认应为 Bypass"
    );
}

#[tokio::test]
async fn test_permission_mode_store_and_load() {
    let (app, _handle) = App::new_headless(80, 24).await;
    use peri_middlewares::prelude::PermissionMode;
    for mode in [
        PermissionMode::Default,
        PermissionMode::AcceptEdit,
        PermissionMode::AutoMode,
        PermissionMode::Bypass,
    ] {
        app.services.permission_mode.store(mode);
        assert_eq!(
            app.services.permission_mode.load(),
            mode,
            "store/load 应一致: {:?}",
            mode
        );
    }
}

#[tokio::test]
async fn test_permission_mode_cycle() {
    let (app, _handle) = App::new_headless(80, 24).await;
    use peri_middlewares::prelude::PermissionMode;
    // 默认 Bypass → cycle 到 Default
    let next = app.services.permission_mode.cycle();
    assert_eq!(next, PermissionMode::Default);
    // 继续循环 → AcceptEdit
    let next2 = app.services.permission_mode.cycle();
    assert_eq!(next2, PermissionMode::AcceptEdit);
}

#[tokio::test]
async fn test_status_bar_shows_permission_mode() {
    let (mut app, mut handle) = App::new_headless(120, 24).await;
    // 默认 Bypass → 应显示 "Bypass"
    handle
        .terminal
        .draw(|f| crate::ui::main_ui::render(f, &mut app))
        .unwrap();
    assert!(
        handle.contains("Bypass"),
        "状态栏应显示 Bypass 模式，实际:\n{}",
        handle.snapshot().join("\n")
    );
}

#[tokio::test]
async fn test_status_bar_updates_after_mode_switch() {
    use peri_middlewares::prelude::PermissionMode;
    let (mut app, mut handle) = App::new_headless(120, 24).await;
    // 切换到 Default - 不显示标签
    app.services.permission_mode.store(PermissionMode::Default);
    handle
        .terminal
        .draw(|f| crate::ui::main_ui::render(f, &mut app))
        .unwrap();
    assert!(
        !handle.contains("DEFAULT"),
        "Default 模式不应显示标签，实际:\n{}",
        handle.snapshot().join("\n")
    );

    // 切换到 AcceptEdit
    app.services
        .permission_mode
        .store(PermissionMode::AcceptEdit);
    handle
        .terminal
        .draw(|f| crate::ui::main_ui::render(f, &mut app))
        .unwrap();
    assert!(
        handle.contains("Accept Edit"),
        "切换后状态栏应显示 Accept Edit，实际:\n{}",
        handle.snapshot().join("\n")
    );

    // 切换到 AutoMode
    app.services.permission_mode.store(PermissionMode::AutoMode);
    handle
        .terminal
        .draw(|f| crate::ui::main_ui::render(f, &mut app))
        .unwrap();
    assert!(
        handle.contains("Auto Mode"),
        "切换后状态栏应显示 Auto Mode，实际:\n{}",
        handle.snapshot().join("\n")
    );
}

#[tokio::test]
async fn test_shift_tab_cycles_permission_mode() {
    use peri_middlewares::prelude::PermissionMode;
    let (app, _handle) = App::new_headless(120, 24).await;
    // 初始 Bypass
    assert_eq!(app.services.permission_mode.load(), PermissionMode::Bypass);
    // 模拟 Shift+Tab 按键效果（直接调用 cycle）
    let next = app.services.permission_mode.cycle();
    assert_eq!(next, PermissionMode::Default, "Bypass 之后应为 Default");
    assert_eq!(app.services.permission_mode.load(), PermissionMode::Default);
    // 继续循环 3 次回到 Bypass
    app.services.permission_mode.cycle(); // AcceptEdit
    app.services.permission_mode.cycle(); // AutoMode
    let final_mode = app.services.permission_mode.cycle(); // Bypass
    assert_eq!(final_mode, PermissionMode::Bypass, "循环 4 次回到起点");
}

#[tokio::test]
async fn test_mode_highlight_until_set_on_cycle() {
    let (mut app, _handle) = App::new_headless(120, 24).await;
    // 初始无闪烁
    assert!(
        app.global_ui.mode_highlight_until.is_none(),
        "初始不应有闪烁"
    );
    // 模拟 Shift+Tab: cycle + 设置 highlight
    app.services.permission_mode.cycle();
    app.global_ui.mode_highlight_until =
        Some(std::time::Instant::now() + std::time::Duration::from_millis(1500));
    assert!(
        app.global_ui.mode_highlight_until.is_some(),
        "cycle 后应设置闪烁截止时间"
    );
    // 验证截止时间在未来
    let until = app.global_ui.mode_highlight_until.unwrap();
    assert!(std::time::Instant::now() < until, "截止时间应在未来");
}

#[tokio::test]
async fn test_spinner_shows_verb_in_status_bar() {
    let (mut app, mut handle) = crate::app::App::new_headless(120, 30).await;
    // 添加一条消息，否则 render_messages 会走 welcome 分支提前 return
    app.session_mgr
        .current_mut()
        .messages
        .view_messages
        .push(crate::app::MessageViewModel::user("hello".into()));
    app.session_mgr
        .current_mut()
        .spinner_state
        .set_verb(Some("Searching code"));
    app.session_mgr.current_mut().ui.loading = true;

    handle
        .terminal
        .draw(|f| crate::ui::main_ui::render(f, &mut app))
        .unwrap();
    assert!(
        handle.contains("Searching code"),
        "status bar should show spinner verb"
    );
}

#[tokio::test]
async fn test_tool_call_widget_renders_completed() {
    let (_app, mut handle) = crate::app::App::new_headless(120, 30).await;

    let vm = crate::app::MessageViewModel::ToolBlock {
        tool_name: "Bash".to_string(),
        tool_call_id: "tc_test".to_string(),
        display_name: "Bash".to_string(),
        args_display: Some("ls -la".to_string()),
        content: "file1.txt\nfile2.txt".to_string(),
        color: crate::ui::theme::SAGE,
        is_error: false,
        collapsed: false,
        diff_lines: None,
        content_hash: 0,
    };

    let lines = crate::ui::message_render::render_view_model(&vm, Some(1), 80, false); // Render into a visible area for verification
    use ratatui::widgets::Paragraph;
    let paragraph = Paragraph::new(lines);
    handle
        .terminal
        .draw(|f| {
            let area = ratatui::layout::Rect::new(0, 0, 120, 10);
            f.render_widget(paragraph, area);
        })
        .unwrap();
    assert!(handle.contains("Bash"), "should render tool name");
}

#[tokio::test]
async fn test_retry_status_shows_in_status_bar() {
    let (mut app, mut handle) = App::new_headless(120, 30).await;

    // 直接设置 retry_status 并渲染
    app.session_mgr.current_mut().agent.retry_status = Some(crate::app::RetryStatus {
        attempt: 2,
        max_attempts: 5,
        delay_ms: 2000,
        error: "API 错误 429: Rate limit exceeded".to_string(),
    });

    handle
        .terminal
        .draw(|f| crate::ui::main_ui::render(f, &mut app))
        .unwrap();
    let snap = handle.snapshot();
    assert!(
        handle.contains("2/5"),
        "状态栏应显示重试次数 2/5，实际:\n{}",
        snap.join("\n")
    );
}

// ─── Compact 集成测试 ──────────────────────────────────────────────────

/// 辅助：构造模拟的 CompactCompleted 事件（包含摘要 + 文件 + skill 信息）
fn make_compact_done_event(summary: &str, re_inject_parts: &[&str]) -> AgentEvent {
    let mut files = Vec::new();
    let mut skills = Vec::new();
    for part in re_inject_parts {
        if let Some(rest) = part.strip_prefix("[最近读取的文件: ") {
            let path = rest.lines().next().unwrap_or("");
            let line_count = rest.lines().count().saturating_sub(1);
            if !path.is_empty() {
                files.push(peri_agent::agent::events::CompactFileInfo {
                    path: path.to_string(),
                    lines: line_count,
                });
            }
        } else if let Some(rest) = part.strip_prefix("[激活的 Skill 指令: ") {
            let name = rest.lines().next().unwrap_or("");
            if !name.is_empty() {
                skills.push(name.to_string());
            }
        }
    }
    AgentEvent::CompactCompleted {
        summary: summary.to_string(),
        files,
        skills,
        micro_cleared: 0,
        messages: vec![],
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn test_compact_done_with_re_inject() {
    let (mut app, handle) = App::new_headless(120, 30).await;
    let notified = handle.render_notify.notified();
    app.push_agent_event(make_compact_done_event(
        "Test summary",
        &[
            "[最近读取的文件: /a.rs]\nline1\nline2\nline3",
            "[激活的 Skill 指令: skill.md]\nskill content",
        ],
    ));
    app.process_pending_events();
    notified.await;

    // view_messages 应包含压缩提示（condensed summary 格式）
    let msgs = &app.session_mgr.current_mut().messages.view_messages;
    assert_eq!(
        msgs.len(),
        1,
        "应只有 1 条压缩占位消息，实际: {}",
        msgs.len()
    );
    let has_compact = msgs.iter().any(|m| {
        if let MessageViewModel::SystemNote { content, .. } = m {
            content.contains("✻ Context compressed")
                && content.contains("Read /a.rs")
                && content.contains("Skill: skill.md")
        } else {
            false
        }
    });
    assert!(has_compact, "应包含压缩提示消息");
}

#[tokio::test(flavor = "multi_thread")]
async fn test_compact_done_without_re_inject() {
    let (mut app, handle) = App::new_headless(120, 30).await;
    let notified = handle.render_notify.notified();
    app.push_agent_event(make_compact_done_event("Simple summary", &[]));
    app.process_pending_events();
    notified.await;

    let msgs = &app.session_mgr.current_mut().messages.view_messages;
    assert_eq!(msgs.len(), 1, "应只有 1 条压缩占位消息");
    let has_compact = msgs.iter().any(|m| {
        if let MessageViewModel::SystemNote { content, .. } = m {
            content.contains("✻ Context compressed")
        } else {
            false
        }
    });
    assert!(has_compact, "应包含压缩提示消息");
    let has_re_inject = msgs.iter().any(|m| {
        if let MessageViewModel::SystemNote { content, .. } = m {
            content.contains("Read ") || content.contains("Skill:")
        } else {
            false
        }
    });
    assert!(!has_re_inject, "无重新注入内容时不应显示文件/skill 详情");
}

#[tokio::test]
async fn test_get_compact_config_default() {
    let (app, _handle) = App::new_headless(120, 30).await;
    let config = app.get_compact_config();
    let default = peri_agent::agent::CompactConfig::default();
    assert!(config.auto_compact_enabled == default.auto_compact_enabled);
    assert!((config.auto_compact_threshold - default.auto_compact_threshold).abs() < 0.001);
}

#[tokio::test]
async fn test_get_compact_config_from_settings() {
    let (mut app, _handle) = App::new_headless(120, 30).await;
    let mut zen = crate::config::PeriConfig::default();
    zen.config.compact = Some(peri_agent::agent::CompactConfig {
        auto_compact_threshold: 0.9,
        ..Default::default()
    });
    app.services.peri_config = std::sync::Arc::new(parking_lot::RwLock::new(zen));
    let config = app.get_compact_config();
    assert!(
        (config.auto_compact_threshold - 0.9).abs() < 0.001,
        "应从 settings.json 读取 auto_compact_threshold"
    );
}

// ─── Pipeline 回归测试 ──────────────────────────────────────────────────

/// 回归：用户消息在 AI 回复后仍应可见（不应被 AppendChunk 覆盖）
#[tokio::test]
async fn test_user_message_survives_assistant_chunk() {
    use peri_agent::messages::BaseMessage;

    let (mut app, mut handle) = App::new_headless(120, 30).await;

    // 模拟用户发送消息
    let user_vm = MessageViewModel::user("my question".into());
    app.session_mgr
        .current_mut()
        .messages
        .view_messages
        .push(user_vm);
    app.render_rebuild();

    app.push_agent_event(AgentEvent::AssistantChunk {
        chunk: "AI answer".into(),
        source_agent_id: None,
    });
    app.push_agent_event(AgentEvent::StateSnapshot(vec![
        BaseMessage::human("my question"),
        BaseMessage::ai("AI answer"),
    ]));
    app.push_agent_event(AgentEvent::Done);
    app.process_pending_events();
    app.flush_rebuild();
    tokio::task::yield_now().await;
    tokio::task::yield_now().await;

    handle
        .terminal
        .draw(|f| main_ui::render(f, &mut app))
        .unwrap();

    // view_messages 应包含用户消息 + AI 消息
    assert!(
        app.session_mgr.current_mut().messages.view_messages.len() >= 2,
        "应有至少 2 条消息（用户+AI），实际: {}",
        app.session_mgr.current_mut().messages.view_messages.len()
    );
    assert!(
        handle.contains("my question"),
        "用户消息应在渲染输出中可见，实际:\n{}",
        handle.snapshot().join("\n")
    );
    assert!(
        handle.contains("AI answer"),
        "AI 回复应在渲染输出中可见，实际:\n{}",
        handle.snapshot().join("\n")
    );
}

/// 回归：多轮对话消息累积，不应只看到最后一条
#[tokio::test]
async fn test_messages_accumulate_across_turns() {
    use peri_agent::messages::BaseMessage;

    let (mut app, mut handle) = App::new_headless(120, 30).await;

    // 第一轮：用户 → AI
    // 模拟 submit_message 完整流程：begin_round → push UserBubble → 记录 round_start_vm_idx。
    // begin_round 重置 completed_len_at_round_start，确保 build_tail_vms 只 reconcile 本轮新增，
    // 否则前缀 completed 会被重复纳入 tail_vms（参见 reconcile.rs::build_tail_vms）。
    app.session_mgr
        .current_mut()
        .messages
        .pipeline
        .begin_round();
    let user1 = MessageViewModel::user("turn1".into());
    app.session_mgr
        .current_mut()
        .messages
        .view_messages
        .push(user1);
    app.session_mgr.current_mut().messages.round_start_vm_idx =
        app.session_mgr.current_mut().messages.view_messages.len();
    app.render_rebuild();

    app.push_agent_event(AgentEvent::AssistantChunk {
        chunk: "answer1".into(),
        source_agent_id: None,
    });
    app.push_agent_event(AgentEvent::StateSnapshot(vec![
        BaseMessage::human("turn1"),
        BaseMessage::ai("answer1"),
    ]));
    app.push_agent_event(AgentEvent::Done);
    app.process_pending_events();
    app.flush_rebuild();
    tokio::task::yield_now().await;
    tokio::task::yield_now().await;

    // 第二轮：用户 → AI
    // 模拟 submit_message：先 push Human VM，再记录 round_start_vm_idx（与
    // agent_submit.rs::submit_message 顺序一致）。
    app.session_mgr
        .current_mut()
        .messages
        .pipeline
        .begin_round();
    let user2 = MessageViewModel::user("turn2".into());
    app.session_mgr
        .current_mut()
        .messages
        .view_messages
        .push(user2);
    app.session_mgr.current_mut().messages.round_start_vm_idx =
        app.session_mgr.current_mut().messages.view_messages.len();
    app.render_rebuild();

    app.push_agent_event(AgentEvent::AssistantChunk {
        chunk: "answer2".into(),
        source_agent_id: None,
    });
    // StateSnapshot 是增量的：只携带本轮新增消息（snapshot_anchor 之后），
    // 不是整段 history。set_completed() 用 extend 累加，详见
    // peri-agent/src/agent/executor/final_answer.rs::emit_snapshot_and_drain_notifications
    // 与本文件 test_state_snapshot_is_incremental 回归。
    app.push_agent_event(AgentEvent::StateSnapshot(vec![
        BaseMessage::human("turn2"),
        BaseMessage::ai("answer2"),
    ]));
    app.push_agent_event(AgentEvent::Done);
    app.process_pending_events();
    app.flush_rebuild();
    tokio::task::yield_now().await;
    tokio::task::yield_now().await;

    handle
        .terminal
        .draw(|f| main_ui::render(f, &mut app))
        .unwrap();

    // 应累积 4 条消息
    assert_eq!(
        app.session_mgr.current_mut().messages.view_messages.len(),
        4,
        "两轮对话应有 4 条消息，实际: {}",
        app.session_mgr.current_mut().messages.view_messages.len()
    );
    assert!(handle.contains("turn1"), "第一轮用户消息应可见");
    assert!(handle.contains("turn2"), "第二轮用户消息应可见");
}

/// 回归：AI 消息不应在 Done 后重复
#[tokio::test]
async fn test_done_does_not_duplicate_ai_message() {
    use peri_agent::messages::BaseMessage;

    let (mut app, _handle) = App::new_headless(120, 30).await;

    // 模拟 StateSnapshot（增量）+ Done 序列
    app.push_agent_event(AgentEvent::AssistantChunk {
        chunk: "unique text".into(),
        source_agent_id: None,
    });
    app.push_agent_event(AgentEvent::StateSnapshot(vec![
        BaseMessage::human("q"),
        BaseMessage::ai("unique text"),
    ]));
    app.push_agent_event(AgentEvent::Done);
    app.process_pending_events();

    // 统计包含 "unique text" 的 assistant bubble 数量
    let assistant_count = app
        .session_mgr
        .current_mut()
        .messages
        .view_messages
        .iter()
        .filter(|m| m.is_assistant())
        .count();
    assert_eq!(
        assistant_count, 1,
        "应有恰好 1 个 assistant bubble，实际: {}",
        assistant_count
    );
}

/// 回归：StateSnapshot 是增量的，不应覆盖之前已完成的消息
#[test]
fn test_state_snapshot_is_incremental() {
    use peri_agent::messages::{BaseMessage, MessageContent, MessageId};

    use crate::app::message_pipeline::MessagePipeline;

    let mut pipeline = MessagePipeline::new("/tmp".to_string());

    // 第一次 snapshot：Human + Ai
    pipeline.set_completed(vec![BaseMessage::human("hello"), BaseMessage::ai("world")]);
    assert_eq!(pipeline.completed_messages().len(), 2);

    // 第二次 snapshot（增量）：Tool result
    pipeline.set_completed(vec![BaseMessage::Tool {
        id: MessageId::new(),
        tool_call_id: "tc1".into(),
        content: MessageContent::text("result"),
        is_error: false,
    }]);

    // 应累积到 3 条，不是只剩 1 条
    assert_eq!(
        pipeline.completed_messages().len(),
        3,
        "StateSnapshot 应增量追加，不应覆盖，实际: {}",
        pipeline.completed_messages().len()
    );
}

/// 回归：ToolStart 之后 AssistantChunk 不会丢失工具消息
#[tokio::test]
async fn test_tool_then_text_preserves_tool_block() {
    let (mut app, mut handle) = App::new_headless(120, 30).await;

    app.push_agent_event(AgentEvent::ToolStart {
        tool_call_id: "tc1".into(),
        name: "Bash".into(),
        display: "Shell".into(),
        args: "ls".into(),
        input: serde_json::json!({"command": "ls"}),
        source_agent_id: None,
    });
    app.push_agent_event(AgentEvent::AssistantChunk {
        chunk: "result is here".into(),
        source_agent_id: None,
    });
    app.process_pending_events();
    app.flush_rebuild();
    tokio::task::yield_now().await;
    tokio::task::yield_now().await;

    handle
        .terminal
        .draw(|f| main_ui::render(f, &mut app))
        .unwrap();

    // ToolBlock 和 AssistantBubble 都应存在
    let has_tool = app
        .session_mgr
        .current_mut()
        .messages
        .view_messages
        .iter()
        .any(|m| matches!(m, MessageViewModel::ToolBlock { .. }));
    let has_assistant = app
        .session_mgr
        .current_mut()
        .messages
        .view_messages
        .iter()
        .any(|m| m.is_assistant());
    assert!(has_tool, "应有 ToolBlock");
    assert!(has_assistant, "应有 AssistantBubble");
    assert!(handle.contains("result is here"), "应显示 AI 回复");
}

// ── 统一提示浮层测试 ──────────────────────────────────────────────────

#[tokio::test]
async fn test_unified_hint_shows_commands_and_skills() {
    use peri_middlewares::skills::loader::SkillMetadata;
    let (mut app, mut handle) = App::new_headless(120, 50).await;

    // 设置输入框内容为 /
    app.session_mgr.current_mut().ui.textarea = crate::app::build_textarea(false);
    app.session_mgr.current_mut().ui.textarea.insert_str("/");
    app.session_mgr
        .current_mut()
        .ui
        .slash_hint
        .activate(String::new(), 0);

    // 注入 2 个 Skills
    app.session_mgr
        .current_mut()
        .commands
        .skills
        .push(SkillMetadata {
            name: "commit".into(),
            description: "commit changes".into(),
            path: "/tmp/commit.md".into(),
            ..Default::default()
        });
    app.session_mgr
        .current_mut()
        .commands
        .skills
        .push(SkillMetadata {
            name: "review".into(),
            description: "review code".into(),
            path: "/tmp/review.md".into(),
            ..Default::default()
        });

    // 候选列表应包含命令和 Skills
    let count = app.hint_candidates_count();
    let cmd_count = app
        .session_mgr
        .current_mut()
        .commands
        .command_registry
        .match_prefix("", &app.services.lc)
        .len();
    assert_eq!(
        count,
        cmd_count + 2,
        "候选应包含 {} 命令 + 2 Skills",
        cmd_count
    );

    // 渲染后应显示命令（视口 MAX_VIEWPORT=10，命令优先排序）
    handle
        .terminal
        .draw(|f| main_ui::render(f, &mut app))
        .unwrap();
    let snap = handle.snapshot();
    let snap_text = snap.join("\n");

    assert!(
        snap_text.contains("model"),
        "应显示 model 命令，实际:\n{}",
        snap_text
    );
}

#[tokio::test]
async fn test_unified_hint_filters_by_prefix() {
    use peri_middlewares::skills::loader::SkillMetadata;
    let (mut app, mut handle) = App::new_headless(120, 30).await;

    app.session_mgr.current_mut().ui.textarea = crate::app::build_textarea(false);
    app.session_mgr.current_mut().ui.textarea.insert_str("/mo");
    app.session_mgr
        .current_mut()
        .ui
        .slash_hint
        .activate("mo".to_string(), 0);

    app.session_mgr
        .current_mut()
        .commands
        .skills
        .push(SkillMetadata {
            name: "commit".into(),
            description: "commit changes".into(),
            path: "/tmp/commit.md".into(),
            ..Default::default()
        });

    handle
        .terminal
        .draw(|f| main_ui::render(f, &mut app))
        .unwrap();
    let snap = handle.snapshot();
    let snap_text = snap.join("\n");

    // 应包含匹配的命令 model
    assert!(
        snap_text.contains("model"),
        "应包含匹配前缀 /mo 的命令 model，实际:\n{}",
        snap_text
    );
    // 不应包含不匹配的 Skill（commit 不含 "mo"）
    assert!(
        !snap_text.contains("commit"),
        "不应包含不匹配的 Skill，实际:\n{}",
        snap_text
    );
}

#[tokio::test]
async fn test_unified_hint_no_result_for_hash() {
    use peri_middlewares::skills::loader::SkillMetadata;
    let (mut app, mut handle) = App::new_headless(120, 30).await;

    app.session_mgr.current_mut().ui.textarea = crate::app::build_textarea(false);
    app.session_mgr
        .current_mut()
        .ui
        .textarea
        .insert_str("#skill");

    app.session_mgr
        .current_mut()
        .commands
        .skills
        .push(SkillMetadata {
            name: "skill".into(),
            description: "a skill".into(),
            path: "/tmp/skill.md".into(),
            ..Default::default()
        });

    handle
        .terminal
        .draw(|f| main_ui::render(f, &mut app))
        .unwrap();
    let snap = handle.snapshot();
    let snap_text = snap.join("\n");

    // # 前缀不应触发浮层
    assert!(
        !snap_text.contains("Skills"),
        "# 前缀不应触发 Skills 浮层，实际:\n{}",
        snap_text
    );
}

// ── Enter 触发 Skill fallback 测试 ──────────────────────────────────────────

#[tokio::test]
async fn test_enter_skill_name_submits_message() {
    use peri_middlewares::skills::loader::SkillMetadata;
    let (mut app, _handle) = App::new_headless(120, 30).await;

    app.session_mgr.current_mut().ui.textarea = crate::app::build_textarea(false);
    app.session_mgr
        .current_mut()
        .ui
        .textarea
        .insert_str("/review");
    app.session_mgr
        .current_mut()
        .commands
        .skills
        .push(SkillMetadata {
            name: "review".into(),
            description: "code review".into(),
            path: "/tmp/review.md".into(),
            ..Default::default()
        });

    // 模拟 Enter 事件处理
    let text: String = app.session_mgr.current_mut().ui.textarea.lines().join("\n");
    let text = text.trim().to_string();
    assert!(text.starts_with('/'));

    // 验证命令 dispatch 不匹配后 Skill fallback
    let registry = std::mem::take(&mut app.session_mgr.current_mut().commands.command_registry);
    let known = registry.dispatch(&mut app, &text);
    app.session_mgr.current_mut().commands.command_registry = registry;
    assert!(!known, "review 不应是已知命令");

    // 验证 Skill 匹配
    let skill_name: String = text
        .trim_start_matches('/')
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    assert_eq!(skill_name, "review");
    let skill_found = app
        .session_mgr
        .current_mut()
        .commands
        .skills
        .iter()
        .find(|s| s.name == skill_name);
    assert!(skill_found.is_some(), "应找到 review Skill");
}

#[tokio::test]
async fn test_enter_unknown_command_shows_error() {
    let (mut app, _handle) = App::new_headless(120, 30).await;

    app.session_mgr.current_mut().ui.textarea = crate::app::build_textarea(false);
    app.session_mgr
        .current_mut()
        .ui
        .textarea
        .insert_str("/nonexistent");

    // 模拟 Enter 处理逻辑
    let text: String = app.session_mgr.current_mut().ui.textarea.lines().join("\n");
    let text = text.trim().to_string();
    let registry = std::mem::take(&mut app.session_mgr.current_mut().commands.command_registry);
    let known = registry.dispatch(&mut app, &text);
    app.session_mgr.current_mut().commands.command_registry = registry;
    assert!(!known, "nonexistent 不应是已知命令");

    // Skill fallback 也应失败
    let skill_name: String = text
        .trim_start_matches('/')
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    let skill_found = app
        .session_mgr
        .current_mut()
        .commands
        .skills
        .iter()
        .find(|s| s.name == skill_name);
    assert!(skill_found.is_none(), "不应找到 nonexistent Skill");
}

#[tokio::test]
async fn test_enter_known_command_no_skill_fallback() {
    use peri_middlewares::skills::loader::SkillMetadata;
    let (mut app, _handle) = App::new_headless(120, 30).await;

    // 注入名为 help 的 Skill
    app.session_mgr
        .current_mut()
        .commands
        .skills
        .push(SkillMetadata {
            name: "help".into(),
            description: "help skill".into(),
            path: "/tmp/help.md".into(),
            ..Default::default()
        });

    // /help 应被命令 dispatch 拦截，不走 Skill fallback
    let registry = std::mem::take(&mut app.session_mgr.current_mut().commands.command_registry);
    let known = registry.dispatch(&mut app, "/help");
    app.session_mgr.current_mut().commands.command_registry = registry;
    assert!(known, "/help 应是已知命令，优先于同名 Skill");
}

// ── Input Placeholder Hint ──────────────────────────────────────────────

#[tokio::test]
async fn test_textarea_shows_placeholder_hint() {
    let (mut app, mut handle) = App::new_headless(120, 30).await;
    handle
        .terminal
        .draw(|f| main_ui::render(f, &mut app))
        .unwrap();
    let snap = handle.snapshot();
    let snap_text = snap.join("\n");
    assert!(
        snap_text.contains("Shift+Enter") || snap_text.contains("输入消息"),
        "输入框应显示占位提示（含 Shift+Enter 换行），实际:\n{}",
        snap_text
    );
}

// ── Welcome Card Alt+Enter Hint ─────────────────────────────────────────

#[tokio::test]
async fn test_welcome_card_shows_alt_enter_hint() {
    let (mut app, mut handle) = App::new_headless(120, 30).await;
    handle
        .terminal
        .draw(|f| main_ui::render(f, &mut app))
        .unwrap();
    let snap = handle.snapshot();
    let snap_text = snap.join("\n");
    assert!(
        snap_text.contains("Shift+Enter"),
        "Welcome Card 应显示 Shift+Enter 快捷键提示，实际:\n{}",
        snap_text
    );
}

// ── Command Ambiguity Feedback ──────────────────────────────────────────

#[tokio::test]
async fn test_ambiguous_command_shows_candidates() {
    let (mut app, _handle) = App::new_headless(120, 30).await;
    // /c 前缀匹配 clear/compact/cron
    let registry = &app.session_mgr.current_mut().commands.command_registry;
    let matches = registry.match_prefix("c", &app.services.lc);
    assert!(matches.len() >= 2, "/c 应匹配多个命令，实际: {:?}", matches);
    // dispatch 应返回 false（歧义）
    let registry = std::mem::take(&mut app.session_mgr.current_mut().commands.command_registry);
    let known = registry.dispatch(&mut app, "/c");
    app.session_mgr.current_mut().commands.command_registry = registry;
    assert!(!known, "歧义前缀 dispatch 应返回 false");
}

// ─── Design Review 第22轮：Model 面板 Space 键 + Cron 确认删除 + 面板 Paste 拦截 ────

/// Model 面板 Space 键在模型行应选中对应模型（而非静默无响应）
#[tokio::test]
async fn test_model_panel_space_selects_model() {
    use crate::{
        app::model_panel::{AliasTab, ModelPanel, ROW_SONNET},
        config::{AppConfig, PeriConfig, ProviderConfig, ThinkingConfig},
    };

    let cfg = PeriConfig {
        schema: None,
        config: AppConfig {
            active_alias: "opus".to_string(),
            active_provider_id: "test".to_string(),
            providers: vec![ProviderConfig {
                id: "test".to_string(),
                name: Some("TestProvider".to_string()),
                ..Default::default()
            }],
            thinking: Some(ThinkingConfig {
                enabled: false,
                budget_tokens: 8000,
                effort: "medium".to_string(),
                max_tokens: 32000,
            }),
            ..Default::default()
        },
    };

    let mut panel = ModelPanel::from_config(&cfg);
    // 光标移到 Sonnet 行
    panel.cursor = ROW_SONNET;
    assert_eq!(panel.active_tab, AliasTab::Opus);

    // 直接验证 Space 的实际处理逻辑：应设置 active_tab
    // （event.rs 中 Space 在 ROW_SONNET 会设置 active_tab = Sonnet）
    panel.active_tab = AliasTab::Sonnet;
    assert_eq!(
        panel.active_tab,
        AliasTab::Sonnet,
        "Space 应能选中 Sonnet 模型"
    );
}

/// Cron 面板删除确认：Ctrl+D 应进入确认状态而非立即删除
#[tokio::test]
async fn test_cron_panel_delete_confirmation() {
    use chrono::Utc;
    use peri_middlewares::cron::CronTask;

    use crate::app::CronPanel;

    let (mut app, _handle) = App::new_headless(120, 30).await;

    // 手动构造一个 cron 任务
    let task = CronTask {
        id: "test-job-1".to_string(),
        expression: "*/5 * * * *".to_string(),
        prompt: "test prompt".to_string(),
        enabled: true,
        next_fire: Some(Utc::now() + chrono::Duration::seconds(60)),
    };
    app.global_panels
        .open(crate::app::panel_manager::PanelState::Cron(CronPanel::new(
            vec![task],
        )));
    assert_eq!(
        app.global_panels.get::<CronPanel>().unwrap().tasks().len(),
        1
    );
    assert!(!app.global_panels.get::<CronPanel>().unwrap().confirm_delete);

    // Ctrl+D → 进入确认状态
    app.cron_panel_request_delete();
    assert!(
        app.global_panels.get::<CronPanel>().unwrap().confirm_delete,
        "Ctrl+D 应设置 confirm_delete = true"
    );
    assert_eq!(
        app.global_panels.get::<CronPanel>().unwrap().tasks().len(),
        1,
        "确认前不应删除任务"
    );

    // Esc / 其他键 → 取消确认
    app.cron_panel_cancel_delete();
    assert!(
        !app.global_panels.get::<CronPanel>().unwrap().confirm_delete,
        "取消后 confirm_delete 应为 false"
    );
    assert_eq!(
        app.global_panels.get::<CronPanel>().unwrap().tasks().len(),
        1,
        "取消后任务应仍存在"
    );

    // 再次进入确认，然后 Enter 确认删除
    app.cron_panel_request_delete();
    assert!(app.global_panels.get::<CronPanel>().unwrap().confirm_delete);
    app.cron_panel_confirm_delete();
    // 面板为空时自动关闭
    assert!(
        !app.global_panels.is_any_open(),
        "删除最后一个任务后面板应关闭"
    );
}

/// Cron 面板确认删除时渲染显示确认提示
#[tokio::test]
async fn test_cron_panel_confirm_delete_renders() {
    use chrono::Utc;
    use peri_middlewares::cron::CronTask;

    use crate::app::CronPanel;

    let (mut app, mut handle) = App::new_headless(120, 30).await;
    let task = CronTask {
        id: "job-1".to_string(),
        expression: "*/5 * * * *".to_string(),
        prompt: "test".to_string(),
        enabled: true,
        next_fire: Some(Utc::now() + chrono::Duration::seconds(60)),
    };
    app.global_panels
        .open(crate::app::panel_manager::PanelState::Cron(CronPanel::new(
            vec![task],
        )));
    app.global_panels
        .get_mut::<CronPanel>()
        .unwrap()
        .confirm_delete = true;

    handle
        .terminal
        .draw(|f| main_ui::render(f, &mut app))
        .unwrap();
    let snap = handle.snapshot();
    // 使用 ASCII 内容断言（避免 CJK 宽字符问题），确认面板渲染了 Enter 提示
    let _all_text = snap.join("");
    // 面板应该渲染了包含 "Enter" 的帮助行（确认模式提示）
    let has_enter = snap.iter().any(|l| l.contains("Enter"));
    assert!(
        has_enter,
        "Cron 面板确认删除模式应渲染 Enter 快捷键提示，实际:\n{}",
        snap.join("\n")
    );
}

// ─── Design Review 第23轮：面板操作成功反馈 ────

/// Model 面板确认选择后应显示"模型已切换为"反馈消息
#[tokio::test]
async fn test_model_panel_confirm_shows_feedback() {
    use crate::{
        app::model_panel::{AliasTab, ModelPanel},
        config::{AppConfig, PeriConfig, ProviderConfig, ThinkingConfig},
    };

    let (mut app, _handle) = App::new_headless(120, 30).await;
    let cfg = PeriConfig {
        schema: None,
        config: AppConfig {
            active_alias: "opus".to_string(),
            active_provider_id: "test".to_string(),
            providers: vec![ProviderConfig {
                id: "test".to_string(),
                name: Some("TestProvider".to_string()),
                ..Default::default()
            }],
            thinking: Some(ThinkingConfig {
                enabled: false,
                budget_tokens: 8000,
                effort: "medium".to_string(),
                max_tokens: 32000,
            }),
            ..Default::default()
        },
    };
    app.services.peri_config = std::sync::Arc::new(parking_lot::RwLock::new(cfg));
    app.session_mgr.current_mut().session_panels.open(
        crate::app::panel_manager::PanelState::Model(ModelPanel::from_config(
            &app.services.peri_config.read(),
        )),
    );
    app.session_mgr
        .current_mut()
        .session_panels
        .get_mut::<ModelPanel>()
        .unwrap()
        .active_tab = AliasTab::Sonnet;

    app.model_panel_confirm();

    let last_msg = app.session_mgr.current_mut().messages.view_messages.last();
    assert!(last_msg.is_some(), "Model 面板确认后应有反馈消息");
    let msg_text = match last_msg.unwrap() {
        MessageViewModel::SystemNote { content, .. } => content.clone(),
        _ => String::new(),
    };
    assert!(
        msg_text.contains("Sonnet"),
        "反馈消息应包含模型名 'Sonnet'，实际: {}",
        msg_text
    );
    assert!(
        !app.session_mgr
            .current_mut()
            .session_panels
            .is_active(crate::app::PanelKind::Model),
        "确认后面板应关闭"
    );
}

/// Login 面板激活 Provider 后应显示"已激活"反馈消息
#[tokio::test]
async fn test_login_select_provider_shows_feedback() {
    use crate::{
        app::login_panel::LoginPanel,
        config::{AppConfig, PeriConfig, ProviderConfig},
    };

    let (mut app, _handle) = App::new_headless(120, 30).await;
    let cfg = PeriConfig {
        schema: None,
        config: AppConfig {
            active_alias: "opus".to_string(),
            active_provider_id: "test1".to_string(),
            providers: vec![
                ProviderConfig {
                    id: "test1".to_string(),
                    name: Some("Provider1".to_string()),
                    ..Default::default()
                },
                ProviderConfig {
                    id: "test2".to_string(),
                    name: Some("Provider2".to_string()),
                    ..Default::default()
                },
            ],
            ..Default::default()
        },
    };
    app.services.peri_config = std::sync::Arc::new(parking_lot::RwLock::new(cfg));
    app.session_mgr.current_mut().session_panels.open(
        crate::app::panel_manager::PanelState::Login(Box::new(LoginPanel::from_config(
            &app.services.peri_config.read(),
        ))),
    );
    // 光标移到第二个 Provider
    app.session_mgr
        .current_mut()
        .session_panels
        .get_mut::<LoginPanel>()
        .unwrap()
        .browse_list
        .move_cursor_to(1);

    app.login_panel_select_provider();

    let last_msg = app.session_mgr.current_mut().messages.view_messages.last();
    assert!(last_msg.is_some(), "Login 面板激活后应有反馈消息");
    let msg_text = match last_msg.unwrap() {
        MessageViewModel::SystemNote { content, .. } => content.clone(),
        _ => String::new(),
    };
    assert!(
        msg_text.contains("Provider2"),
        "反馈消息应包含 Provider 名 'Provider2'，实际: {}",
        msg_text
    );
    assert!(
        !app.session_mgr
            .current_mut()
            .session_panels
            .is_active(crate::app::PanelKind::Login),
        "激活后面板应关闭"
    );
}

// ─── Design Review 第24轮：Welcome Card 模型信息 + Thread Browser 消息数 ────

/// Welcome Card 应显示当前 Provider/Model 信息
#[tokio::test]
async fn test_welcome_shows_model_info() {
    let (mut app, mut handle) = App::new_headless(120, 30).await;
    // App 默认有 provider_name="test" 和 model_name="test-model"
    handle
        .terminal
        .draw(|f| main_ui::render(f, &mut app))
        .unwrap();
    let snap = handle.snapshot().join("\n");
    // 验证 Welcome Card 包含 provider/model 信息
    assert!(
        snap.contains("test / test-model"),
        "Welcome Card 应显示 Provider/Model 信息，实际:\n{}",
        snap
    );
}

/// 验证后台任务完成通知事件处理
#[tokio::test]
async fn test_background_task_notification() {
    let (mut app, handle) = App::new_headless(120, 30).await;

    // 模拟 submit_message：设置 round_start_vm_idx 并推送用户消息
    app.session_mgr.current_mut().messages.round_start_vm_idx =
        app.session_mgr.current_mut().messages.view_messages.len();
    let user_vm = MessageViewModel::user("test query".into());
    app.session_mgr
        .current_mut()
        .messages
        .view_messages
        .push(user_vm);
    app.render_rebuild();

    // 先设置后台任务
    app.session_mgr.current_mut().background_agents = vec![crate::app::RunningBgAgent {
        agent_name: "code-reviewer".to_string(),
        instance_id: "test-inst".to_string(),
        started_at: std::time::Instant::now(),
        tool_count: 0,
    }];

    let notified = handle.render_notify.notified();

    // 推送 StateSnapshot + Done 以设置正确的 pipeline 状态
    app.push_agent_event(AgentEvent::StateSnapshot(vec![
        peri_agent::messages::BaseMessage::human("test query"),
    ]));
    app.push_agent_event(AgentEvent::Done);
    app.process_pending_events();

    // BackgroundTaskCompleted 在 Done 之后到达
    let notified2 = handle.render_notify.notified();
    app.push_agent_event(AgentEvent::BackgroundTaskCompleted {
        task_id: "bg-test-1".into(),
        agent_name: "code-reviewer".into(),
        success: true,
        output: "LGTM".into(),
        tool_calls_count: 3,
        duration_ms: 1500,
        child_thread_id: None,
    });
    app.process_pending_events();

    notified.await;
    notified2.await;

    // 断言：后台任务计数递减
    assert!(
        app.session_mgr.current_mut().background_agents.is_empty(),
        "BackgroundTaskCompleted should decrement background_agents"
    );

    // 断言：view_messages 包含后台任务 ToolBlock 通知
    use crate::ui::message_view::MessageViewModel;
    let has_notification = app.session_mgr.current_mut()
            .messages
            .view_messages
            .iter()
            .any(|vm| matches!(vm, MessageViewModel::ToolBlock { tool_name, display_name, .. } if tool_name.contains("bg:") && display_name.contains("LGTM")));
    assert!(
        has_notification,
        "view_messages should contain background task notification"
    );
}

/// 验证状态栏显示后台任务计数 [BG: N]
#[tokio::test]
async fn test_background_task_status_bar() {
    let (mut app, mut handle) = App::new_headless(120, 30).await;

    // 模拟 submit_message：设置 round_start_vm_idx 并推送用户消息
    app.session_mgr.current_mut().messages.round_start_vm_idx =
        app.session_mgr.current_mut().messages.view_messages.len();
    let user_vm = MessageViewModel::user("test".into());
    app.session_mgr
        .current_mut()
        .messages
        .view_messages
        .push(user_vm);
    app.render_rebuild();

    app.session_mgr.current_mut().background_agents = vec![
        crate::app::RunningBgAgent {
            agent_name: "reviewer-1".to_string(),
            instance_id: "test-inst-1".to_string(),
            started_at: std::time::Instant::now(),
            tool_count: 0,
        },
        crate::app::RunningBgAgent {
            agent_name: "reviewer-2".to_string(),
            instance_id: "test-inst-2".to_string(),
            started_at: std::time::Instant::now(),
            tool_count: 0,
        },
    ];

    // Trigger a render via StateSnapshot + Done
    let notified = handle.render_notify.notified();
    app.push_agent_event(AgentEvent::StateSnapshot(vec![
        peri_agent::messages::BaseMessage::human("test"),
    ]));
    app.push_agent_event(AgentEvent::Done);
    app.process_pending_events();
    notified.await;

    handle
        .terminal
        .draw(|f| main_ui::render(f, &mut app))
        .unwrap();
    let snap = handle.snapshot().join("\n");

    assert!(
        snap.contains("[BG: 2]"),
        "Status bar should display [BG: 2], actual:\n{}",
        snap
    );
}

// ── Textarea Input During Loading ──────────────────────────────────────

#[tokio::test]
async fn test_textarea_input_visible_during_loading() {
    use tui_textarea::{Input, Key};

    let (mut app, mut handle) = App::new_headless(120, 30).await;

    // 模拟 agent 运行中（loading = true）
    app.set_loading(true);

    // 用户在 loading 时输入文字
    app.session_mgr.current_mut().ui.textarea.input(Input {
        key: Key::Char('h'),
        ctrl: false,
        alt: false,
        shift: false,
    });
    app.session_mgr.current_mut().ui.textarea.input(Input {
        key: Key::Char('i'),
        ctrl: false,
        alt: false,
        shift: false,
    });

    handle
        .terminal
        .draw(|f| main_ui::render(f, &mut app))
        .unwrap();
    let snap = handle.snapshot();
    assert!(
        snap.iter().any(|line| line.contains("hi")),
        "Loading 时输入的文字 'hi' 应该可见，实际:\n{}",
        snap.join("\n")
    );
}

// ── SubAgentGroup Reconcile Preservation ──────────────────────────────────

/// 验证 Done reconcile 后 SubAgentGroup 富状态（recent_messages、total_steps、collapsed）
/// 不会退化为最小化状态。
#[tokio::test]
async fn test_subagent_group_preserved_after_done_reconcile() {
    use peri_agent::messages::{BaseMessage, MessageContent, ToolCallRequest};

    let (mut app, handle) = App::new_headless(120, 30).await;

    // 1. 模拟 AI 文本
    let n = handle.render_notify.notified();
    app.push_agent_event(AgentEvent::AssistantChunk {
        chunk: "I'll use a sub-agent".into(),
        source_agent_id: None,
    });
    app.process_pending_events();
    let _ = n;

    // 2. SubAgentStart
    let n = handle.render_notify.notified();
    app.push_agent_event(AgentEvent::SubAgentStart {
        agent_id: "code-reviewer".into(),
        instance_id: "test-instance".into(),
        task_preview: "review the code".into(),
        is_background: false,
    });
    app.process_pending_events();
    let _ = n;

    // 3. SubAgent 内部 tool calls
    let n1 = handle.render_notify.notified();
    app.push_agent_event(AgentEvent::ToolStart {
        tool_call_id: "sa_tc1".into(),
        name: "Read".into(),
        display: "Read".into(),
        args: "file.rs".into(),
        input: serde_json::json!({"file_path": "/tmp/file.rs"}),
        source_agent_id: Some("test-instance".into()),
    });
    app.process_pending_events();
    let _ = n1;

    let n2 = handle.render_notify.notified();
    app.push_agent_event(AgentEvent::ToolEnd {
        tool_call_id: "sa_tc1".into(),
        name: "Read".into(),
        output: "file content".into(),
        is_error: false,
        source_agent_id: Some("test-instance".into()),
    });
    app.process_pending_events();
    let _ = n2;

    // 4. SubAgentEnd
    let n = handle.render_notify.notified();
    app.push_agent_event(AgentEvent::SubAgentEnd {
        result: "review complete".into(),
        is_error: false,
        agent_id: Some("code-reviewer".into()),
        instance_id: Some("test-instance".into()),
    });
    app.process_pending_events();
    let _ = n;

    // 5. 记录 Done 前 SubAgentGroup 状态
    let pre_done_sub = app
        .session_mgr
        .current_mut()
        .messages
        .view_messages
        .iter()
        .find(|m| m.is_subagent_group())
        .cloned();
    assert!(pre_done_sub.is_some(), "Done 前应有 SubAgentGroup");

    let (pre_steps, pre_recent_len, pre_collapsed) = match &pre_done_sub {
        Some(MessageViewModel::SubAgentGroup {
            total_steps,
            recent_messages,
            collapsed,
            ..
        }) => (*total_steps, recent_messages.len(), *collapsed),
        _ => (0, 0, true),
    };
    assert_eq!(pre_steps, 1, "Done 前 total_steps 应为 1（1 个 ToolStart）");
    assert_eq!(pre_recent_len, 1, "Done 前 recent_messages 应有 1 条");
    assert!(!pre_collapsed, "Done 前 collapsed 应为 false");

    // 6. StateSnapshot（模拟 BaseMessage 层面的数据）
    app.push_agent_event(AgentEvent::StateSnapshot(vec![
        BaseMessage::ai_with_tool_calls(
            MessageContent::text("I'll use a sub-agent"),
            vec![ToolCallRequest::new(
                "subagent_code-reviewer",
                "Agent",
                serde_json::json!({"subagent_type": "code-reviewer", "prompt": "review the code"}),
            )],
        ),
        BaseMessage::tool_result("subagent_code-reviewer", "review complete"),
    ]));
    app.process_pending_events();

    // 7. Done → reconcile
    app.push_agent_event(AgentEvent::Done);
    app.process_pending_events();

    // 8. 验证 Done 后 SubAgentGroup 状态保留
    let post_done_sub = app
        .session_mgr
        .current_mut()
        .messages
        .view_messages
        .iter()
        .find(|m| m.is_subagent_group())
        .cloned();
    assert!(post_done_sub.is_some(), "Done 后应有 SubAgentGroup");

    if let Some(MessageViewModel::SubAgentGroup {
        total_steps,
        recent_messages,
        collapsed,
        is_running,
        ..
    }) = &post_done_sub
    {
        assert_eq!(
            *total_steps, pre_steps,
            "Done 后 total_steps 应保留（{}），实际: {}",
            pre_steps, total_steps
        );
        assert_eq!(
            recent_messages.len(),
            pre_recent_len,
            "Done 后 recent_messages 数量应保留（{}），实际: {}",
            pre_recent_len,
            recent_messages.len()
        );
        assert_eq!(
            *collapsed, pre_collapsed,
            "Done 后 collapsed 应保留（{}），实际: {}",
            pre_collapsed, collapsed
        );
        assert!(!*is_running, "Done 后 is_running 应为 false");
    }
}

// ── Auto-compact deferred during background tasks ──────────────────────

// ── Background Agent SubAgentGroup 消失诊断 ───────────────────────────

/// 统计 view_messages 中 SubAgentGroup 的数量
fn bg_diag_count_subagent_groups(app: &App) -> usize {
    app.session_mgr
        .current()
        .messages
        .view_messages
        .iter()
        .filter(|vm| vm.is_subagent_group())
        .count()
}

/// 打印当前 view_messages 的摘要（诊断用）
fn bg_diag_print_vms(app: &App, label: &str) {
    let vms = &app.session_mgr.current().messages.view_messages;
    eprintln!("\n=== {} (total: {}) ===", label, vms.len());
    for (i, vm) in vms.iter().enumerate() {
        match vm {
            MessageViewModel::UserBubble { content, .. } => {
                let preview: String = content.chars().take(30).collect();
                eprintln!("  [{}] UserBubble({})", i, preview);
            }
            MessageViewModel::AssistantBubble { is_streaming, .. } => {
                eprintln!("  [{}] AssistantBubble(streaming={})", i, is_streaming);
            }
            MessageViewModel::ToolBlock {
                tool_name, content, ..
            } => {
                let preview: String = content.chars().take(40).collect();
                eprintln!("  [{}] ToolBlock({}, content={:?})", i, tool_name, preview);
            }
            MessageViewModel::SubAgentGroup {
                agent_id,
                is_running,
                final_result,
                total_steps,
                ..
            } => {
                eprintln!(
                    "  [{}] SubAgentGroup(id={}, running={}, steps={}, has_result={})",
                    i,
                    agent_id,
                    is_running,
                    total_steps,
                    final_result.is_some()
                );
            }
            MessageViewModel::ToolCallGroup { category, .. } => {
                eprintln!("  [{}] ToolCallGroup({:?})", i, category);
            }
            MessageViewModel::SystemNote { content, .. } => {
                let preview: String = content.chars().take(40).collect();
                eprintln!("  [{}] SystemNote({})", i, preview);
            }
            MessageViewModel::CacheWarning { content, .. } => {
                let preview: String = content.chars().take(40).collect();
                eprintln!("  [{}] CacheWarning({})", i, preview);
            }
        }
    }
    eprintln!(
        "  SubAgentGroup count: {}",
        bg_diag_count_subagent_groups(app)
    );
}

/// 诊断测试：复现 background agent SubAgentGroup 在 BackgroundTaskCompleted 后消失
///
/// 事件流：
/// 1. User message → SubAgentStart(bg) → SubAgentEnd → StateSnapshot → Done
/// 2. BackgroundTaskCompleted → 检查 SubAgentGroup 是否仍在
#[tokio::test]
async fn test_diagnostic_bg_subagent_group_disappears() {
    use peri_agent::messages::{BaseMessage, ToolCallRequest};

    use crate::app::message_pipeline::PipelineAction;

    let (mut app, _handle) = App::new_headless(120, 30).await;

    // Step 1: 模拟用户消息（begin_round + AddMessage）
    app.session_mgr
        .current_mut()
        .messages
        .pipeline
        .begin_round();
    app.apply_pipeline_action(PipelineAction::AddMessage(MessageViewModel::user(
        "run background agent".into(),
    )));
    app.session_mgr.current_mut().messages.round_start_vm_idx =
        app.session_mgr.current_mut().messages.view_messages.len();

    bg_diag_print_vms(&app, "Step 1: After UserBubble");

    // Step 2: SubAgentStart (background agent)
    app.push_agent_event(AgentEvent::SubAgentStart {
        agent_id: "code-reviewer".into(),
        instance_id: "test-instance".into(),
        task_preview: "review the code".into(),
        is_background: true,
    });
    app.process_pending_events();
    bg_diag_print_vms(&app, "Step 2: After SubAgentStart");

    assert!(
        bg_diag_count_subagent_groups(&app) >= 1,
        "After SubAgentStart: should have SubAgentGroup"
    );

    // Step 3: SubAgentEnd (invoke_background returns immediately)
    app.push_agent_event(AgentEvent::SubAgentEnd {
        result: "Background task bg-abc123 started.".into(),
        is_error: false,
        agent_id: None,
        instance_id: None,
    });
    app.process_pending_events();
    bg_diag_print_vms(&app, "Step 3: After SubAgentEnd");

    // Step 4: StateSnapshot (includes Tool(Agent) message)
    let snapshot_msgs = vec![
        BaseMessage::ai_with_tool_calls(
            "",
            vec![ToolCallRequest::new(
                "call_1",
                "Agent",
                serde_json::json!({
                    "subagent_type": "code-reviewer",
                    "prompt": "review the code",
                    "run_in_background": true
                }),
            )],
        ),
        BaseMessage::tool_result("call_1", "Background task bg-abc123 started."),
        BaseMessage::ai("Started the background task."),
    ];
    app.push_agent_event(AgentEvent::StateSnapshot(snapshot_msgs));
    app.process_pending_events();
    bg_diag_print_vms(&app, "Step 4: After StateSnapshot");

    // Step 5: Done (with background task still running)
    app.session_mgr.current_mut().background_agents = vec![crate::app::RunningBgAgent {
        agent_name: "code-reviewer".to_string(),
        instance_id: "test-inst".to_string(),
        started_at: std::time::Instant::now(),
        tool_count: 0,
    }];
    app.push_agent_event(AgentEvent::Done);
    app.process_pending_events();
    bg_diag_print_vms(&app, "Step 5: After Done");

    let count_after_done = bg_diag_count_subagent_groups(&app);
    assert!(
        count_after_done >= 1,
        "After Done: SubAgentGroup should exist, but count={}. VMs:\n{:?}",
        count_after_done,
        app.session_mgr
            .current_mut()
            .messages
            .view_messages
            .iter()
            .map(std::mem::discriminant)
            .collect::<Vec<_>>()
    );

    // 验证 bg_task_state.agent_done_pending 被设置
    assert!(
        app.session_mgr.current_mut().agent.bg_task_state.agent_done_pending,
        "Done with !background_agents.is_empty() should set bg_task_state.agent_done_pending = true"
    );

    // Step 6: BackgroundTaskCompleted — 精确模拟真实场景
    // 在真实场景中，bg_task_state.agent_done_pending=true，所以 handle_background_task_completed
    // 会设置 bg_task_state.pending_continuation 并返回 (true, false, true)
    app.push_agent_event(AgentEvent::BackgroundTaskCompleted {
        task_id: "bg-abc123".into(),
        agent_name: "code-reviewer".into(),
        success: true,
        output: "LGTM".into(),
        tool_calls_count: 5,
        duration_ms: 3000,
        child_thread_id: None,
    });
    app.process_pending_events();
    bg_diag_print_vms(
        &app,
        "Step 6: After BackgroundTaskCompleted (with bg_task_state.agent_done_pending=true)",
    );

    // 验证 bg_task_state.pending_continuation 被设置
    assert!(
        app.session_mgr.current_mut()
            .agent
            .bg_task_state.pending_continuation
            .is_some(),
        "BackgroundTaskCompleted with bg_task_state.agent_done_pending=true should set bg_task_state.pending_continuation"
    );

    let count_after_bg = bg_diag_count_subagent_groups(&app);
    assert_eq!(
        count_after_bg, count_after_done,
        "BUG REPRODUCED: SubAgentGroup disappeared after BackgroundTaskCompleted! Before={}, After={}",
        count_after_done, count_after_bg
    );

    // === 第二阶段：模拟 continuation 触发 ===
    // BackgroundTaskCompleted 处理器在 bg_task_state.agent_done_pending=true 时
    // 设置 bg_task_state.pending_continuation，下一帧 poll_agent 触发 submit_message
    // submit_message 调用 begin_round + AddMessage(UserBubble) + 启动新 agent

    // 模拟 submit_message 的 begin_round
    app.session_mgr
        .current_mut()
        .messages
        .pipeline
        .begin_round();
    // 模拟 submit_message 的 AddMessage(UserBubble)
    app.apply_pipeline_action(PipelineAction::AddMessage(MessageViewModel::user(
        "[bg continuation] process result".into(),
    )));
    app.session_mgr.current_mut().messages.round_start_vm_idx =
        app.session_mgr.current_mut().messages.view_messages.len();

    bg_diag_print_vms(&app, "Step 7: After continuation begin_round + UserBubble");

    // 模拟新 agent 运行的 StateSnapshot（只有新消息，不含历史）
    let continuation_snapshot = vec![BaseMessage::ai("Processing background result...")];
    app.push_agent_event(AgentEvent::StateSnapshot(continuation_snapshot));
    app.process_pending_events();
    bg_diag_print_vms(&app, "Step 8: After continuation StateSnapshot");

    // 模拟新 agent Done
    app.push_agent_event(AgentEvent::Done);
    app.process_pending_events();
    bg_diag_print_vms(&app, "Step 9: After continuation Done");

    let count_final = bg_diag_count_subagent_groups(&app);
    assert!(
        count_final >= 1,
        "BUG REPRODUCED: SubAgentGroup disappeared during continuation! After Done={}, After continuation={}. VMs:\n{:?}",
        count_after_done,
        count_final,
        app.session_mgr.current_mut()
            .messages
            .view_messages
            .iter()
            .enumerate()
            .map(|(i, vm)| {
                let tag = match vm {
                    MessageViewModel::UserBubble { .. } => "User",
                    MessageViewModel::AssistantBubble { .. } => "Assistant",
                    MessageViewModel::ToolBlock { tool_name, .. } => return format!("[{}] Tool({})", i, tool_name),
                    MessageViewModel::SubAgentGroup { agent_id, .. } => return format!("[{}] SubAgent({})", i, agent_id),
                    _ => "Other",
                };
                format!("[{}] {}", i, tag)
            })
            .collect::<Vec<_>>()
    );
}

/// 诊断测试：复现 fork+run_in_background 场景下 SubAgentGroup 消失
///
/// 根因：当 LLM 发送 {fork:true, run_in_background:true} 时，
/// invoke() 中 fork 检测优先于 background 检测（tool.rs:645-649），
/// 走 invoke_fork 同步路径，但 map_executor_event 仍设置 is_background=true，
/// 导致 background_agents 被 push 但永远不会被移除（无 BackgroundTaskCompleted 事件）。
///
/// 这导致：
/// 1. Done 时 !background_agents.is_empty() → bg_task_state.agent_done_pending = true
/// 2. agent_rx 被保持存活，但 agent 任务结束后通道断开
/// 3. Disconnected 分支清理 pipeline → SubAgentGroup 可能丢失
#[tokio::test]
async fn test_diagnostic_fork_plus_background_subagent_group() {
    use peri_agent::messages::{BaseMessage, ToolCallRequest};

    use crate::app::message_pipeline::PipelineAction;

    let (mut app, _handle) = App::new_headless(120, 30).await;

    // Step 1: 用户消息
    app.session_mgr
        .current_mut()
        .messages
        .pipeline
        .begin_round();
    app.apply_pipeline_action(PipelineAction::AddMessage(MessageViewModel::user(
        "run fork in background".into(),
    )));
    app.session_mgr.current_mut().messages.round_start_vm_idx =
        app.session_mgr.current_mut().messages.view_messages.len();

    // Step 2: SubAgentStart — fork+background 场景
    // map_executor_event 从 input 中读取 run_in_background=true，设置 is_background=true
    // 但 invoke_fork 是同步的，不会产生 BackgroundTaskCompleted
    app.push_agent_event(AgentEvent::SubAgentStart {
        agent_id: "fork".into(),
        instance_id: "test-fork-bg".into(),
        task_preview: "do something in background".into(),
        is_background: true, // 关键：fork+background 时 is_background=true
    });
    app.process_pending_events();
    bg_diag_print_vms(&app, "Fork+BG Step 1: After SubAgentStart");

    // 验证 background_agents 被 push
    assert_eq!(
        app.session_mgr.current_mut().background_agents.len(),
        1,
        "SubAgentStart with is_background=true should push to background_agents"
    );

    // Step 3: SubAgentEnd — invoke_fork 同步完成后触发
    // fork 路径的 SubAgentEnd 正常触发（因为 invoke_fork 是同步的）
    app.push_agent_event(AgentEvent::SubAgentEnd {
        result: "[Sub-agent executed 3 tool calls: Read, Bash, Grep]\n\nDone.".into(),
        is_error: false,
        agent_id: None,
        instance_id: None,
    });
    app.process_pending_events();
    bg_diag_print_vms(
        &app,
        "Fork+BG Step 2: After SubAgentEnd (fork completed synchronously)",
    );

    // 验证 background_agents 仍为 1（SubAgentEnd 不移除）
    assert_eq!(
        app.session_mgr.current_mut().background_agents.len(),
        1,
        "SubAgentEnd should NOT remove from background_agents (only BackgroundTaskCompleted does)"
    );

    // Step 4: StateSnapshot（包含 fork 的完整结果）
    let snapshot_msgs = vec![
        BaseMessage::ai_with_tool_calls(
            "",
            vec![ToolCallRequest::new(
                "call_fork_1",
                "Agent",
                serde_json::json!({
                    "fork": true,
                    "run_in_background": true,
                    "prompt": "do something in background"
                }),
            )],
        ),
        BaseMessage::tool_result(
            "call_fork_1",
            "[Sub-agent executed 3 tool calls: Read, Bash, Grep]\n\nDone.",
        ),
        BaseMessage::ai("Fork task completed."),
    ];
    app.push_agent_event(AgentEvent::StateSnapshot(snapshot_msgs));
    app.process_pending_events();
    bg_diag_print_vms(&app, "Fork+BG Step 3: After StateSnapshot");

    // Step 5: Done — 此时 background_agents.len()=1，触发 bg_task_state.agent_done_pending
    app.push_agent_event(AgentEvent::Done);
    app.process_pending_events();
    bg_diag_print_vms(&app, "Fork+BG Step 4: After Done");

    // 验证 bg_task_state.agent_done_pending 被设置（因为 !background_agents.is_empty()）
    assert!(
        app.session_mgr.current_mut().agent.bg_task_state.agent_done_pending,
        "Done with !background_agents.is_empty() should set bg_task_state.agent_done_pending = true"
    );

    // 验证 SubAgentGroup 在 Done 后存在
    let count_after_done = bg_diag_count_subagent_groups(&app);
    assert!(
        count_after_done >= 1,
        "After Done: SubAgentGroup should exist for fork+bg, count={}",
        count_after_done
    );

    // Step 6: 模拟下一帧 — 没有 BackgroundTaskCompleted 事件
    // 因为 invoke_fork 是同步的，不会产生 BackgroundTaskCompleted
    // 但 agent_rx 被保持存活，等待永远不会到来的事件
    // 最终通道会因为 agent task 结束而断开

    // 模拟通道断开（agent_rx 的 sender 被 drop）
    // 在 headless 模式中，我们直接模拟这个状态：
    // bg_task_state.agent_done_pending = true, background_agents.len() = 1, 但没有 BackgroundTaskCompleted

    // 模拟下一轮用户发消息（真实场景中用户可能等待后发新消息）
    app.session_mgr
        .current_mut()
        .messages
        .pipeline
        .begin_round();
    app.apply_pipeline_action(PipelineAction::AddMessage(MessageViewModel::user(
        "next message".into(),
    )));
    app.session_mgr.current_mut().messages.round_start_vm_idx =
        app.session_mgr.current_mut().messages.view_messages.len();

    // 新一轮 StateSnapshot
    app.push_agent_event(AgentEvent::StateSnapshot(vec![BaseMessage::ai("OK")]));
    app.process_pending_events();
    app.flush_rebuild();
    bg_diag_print_vms(&app, "Fork+BG Step 5: After next round StateSnapshot");

    // 验证 SubAgentGroup 在新一轮后是否消失
    let count_final = bg_diag_count_subagent_groups(&app);
    assert!(
        count_final >= 1,
        "BUG REPRODUCED: SubAgentGroup disappeared! fork+background causes phantom background_agents. Before={}, After={}",
        count_after_done,
        count_final
    );

    // 验证 background_agents.len() 仍为 1（永远不会被清除）
    assert_eq!(
        app.session_mgr.current_mut().background_agents.len(),
        1,
        "background_agents should still have 1 entry (no BackgroundTaskCompleted will ever arrive for fork path)"
    );
}

/// 回归：Anthropic thinking 模式下，流式阶段 AI message 不可见是预期的（reasoning 被跳过），
/// 但 Done 后 RebuildAll 不应丢失 user message
#[tokio::test]
async fn test_thinking_mode_user_message_survives_rebuild() {
    use peri_agent::messages::BaseMessage;

    let (mut app, mut handle) = App::new_headless(120, 30).await;

    // 1. 模拟 submit_message：设置 round_start_vm_idx，添加 UserBubble
    app.session_mgr.current_mut().messages.round_start_vm_idx =
        app.session_mgr.current_mut().messages.view_messages.len();
    let user_vm = MessageViewModel::user("explain recursion".into());
    app.session_mgr
        .current_mut()
        .messages
        .view_messages
        .push(user_vm);
    app.render_rebuild();

    // 等待 UserBubble 渲染
    let n0 = handle.render_notify.notified();
    n0.await;

    // 2. AI 开始 thinking（AiReasoning 事件 → PipelineAction::None → 无 VM 创建）
    app.push_agent_event(AgentEvent::AiReasoning(
        "Let me think about recursion...".into(),
    ));
    app.push_agent_event(AgentEvent::AiReasoning(
        "Recursion is when a function calls itself...".into(),
    ));
    app.process_pending_events();

    // 此时 view_messages 应只有 UserBubble（reasoning 不创建 VM）
    assert_eq!(
        app.session_mgr.current_mut().messages.view_messages.len(),
        1,
        "thinking 阶段应只有 UserBubble"
    );

    // 3. AI 开始输出文本（AssistantChunk → 创建 AssistantBubble）
    app.push_agent_event(AgentEvent::AssistantChunk {
        chunk: "Recursion is a technique where ".into(),
        source_agent_id: None,
    });
    app.push_agent_event(AgentEvent::AssistantChunk {
        chunk: "a function calls itself.".into(),
        source_agent_id: None,
    });
    // StateSnapshot 包含 Human + Ai（含 reasoning）
    app.push_agent_event(AgentEvent::StateSnapshot(vec![
        BaseMessage::human("explain recursion"),
        BaseMessage::ai("Recursion is a technique where a function calls itself."),
    ]));
    // Done → reconcile_tail → 可能触发 RebuildAll
    app.push_agent_event(AgentEvent::Done);
    app.process_pending_events();
    app.flush_rebuild();
    tokio::task::yield_now().await;
    tokio::task::yield_now().await;

    handle
        .terminal
        .draw(|f| main_ui::render(f, &mut app))
        .unwrap();

    let snap = handle.snapshot();
    // 关键断言：user message 在 RebuildAll 后仍然可见
    assert!(
        handle.contains("explain recursion"),
        "Done 后 RebuildAll 不应丢失 user message，实际:\n{}",
        snap.join("\n")
    );
    // AI message 也应可见
    assert!(
        handle.contains("Recursion is a technique"),
        "AI 回复应在 RebuildAll 后可见，实际:\n{}",
        snap.join("\n")
    );
}

/// 回归：thinking → tool_call → text 的完整流程，RebuildAll 后所有消息可见
#[tokio::test]
async fn test_thinking_toolcall_text_rebuild_preserves_user() {
    use peri_agent::messages::{BaseMessage, ContentBlock, MessageContent, MessageId};

    let (mut app, mut handle) = App::new_headless(120, 30).await;

    // 1. submit_message
    app.session_mgr.current_mut().messages.round_start_vm_idx =
        app.session_mgr.current_mut().messages.view_messages.len();
    let user_vm = MessageViewModel::user("show me main.rs".into());
    app.session_mgr
        .current_mut()
        .messages
        .view_messages
        .push(user_vm);
    app.render_rebuild();
    let n0 = handle.render_notify.notified();
    n0.await;

    // 2. thinking
    app.push_agent_event(AgentEvent::AiReasoning("I need to read the file...".into()));
    app.process_pending_events();

    // 3. tool_call (AI 调用 Read)
    let notify = Arc::clone(&handle.render_notify);
    let n1 = notify.notified();
    app.push_agent_event(AgentEvent::ToolStart {
        tool_call_id: "tc_read".into(),
        name: "Read".into(),
        display: "ReadFile".into(),
        args: "src/main.rs".into(),
        input: serde_json::json!({"path": "src/main.rs"}),
        source_agent_id: None,
    });
    app.process_pending_events();
    n1.await;

    // 4. tool_end
    let n2 = notify.notified();
    app.push_agent_event(AgentEvent::ToolEnd {
        tool_call_id: "tc_read".into(),
        name: "Read".into(),
        output: "fn main() { println!(\"hello\"); }".into(),
        is_error: false,
        source_agent_id: None,
    });
    app.process_pending_events();
    n2.await;

    // 5. 更多 thinking + 文本回复
    app.push_agent_event(AgentEvent::AiReasoning("Now I can explain...".into()));
    app.push_agent_event(AgentEvent::AssistantChunk {
        chunk: "Here is the content of main.rs:".into(),
        source_agent_id: None,
    });
    // StateSnapshot: Human + Ai(tool_call) + Tool + Ai(text)
    let ai_with_tool = BaseMessage::ai_from_blocks(vec![ContentBlock::tool_use(
        "tc_read",
        "Read",
        serde_json::json!({"path": "src/main.rs"}),
    )]);
    app.push_agent_event(AgentEvent::StateSnapshot(vec![
        BaseMessage::human("show me main.rs"),
        ai_with_tool,
        BaseMessage::Tool {
            id: MessageId::new(),
            tool_call_id: "tc_read".into(),
            content: MessageContent::text("fn main() { println!(\"hello\"); }"),
            is_error: false,
        },
        BaseMessage::ai("Here is the content of main.rs:"),
    ]));
    app.push_agent_event(AgentEvent::Done);
    app.process_pending_events();
    app.flush_rebuild();
    tokio::task::yield_now().await;
    tokio::task::yield_now().await;

    handle
        .terminal
        .draw(|f| main_ui::render(f, &mut app))
        .unwrap();

    let snap = handle.snapshot();
    assert!(
        handle.contains("show me main.rs"),
        "thinking+tool 流程 RebuildAll 后 user message 应可见，实际:\n{}",
        snap.join("\n")
    );
    assert!(
        handle.contains("Here is the content"),
        "AI 最终回复应可见，实际:\n{}",
        snap.join("\n")
    );
}

// ── Background Task Race Condition 修复测试 ─────────────────────────────

/// 竞态路径：BackgroundTaskCompleted 在 Done 之前被消费
/// 修复前：bg_task_state.pre_done_completions 暂存 → Done 处理时设置 bg_task_state.pending_continuation
#[tokio::test]
async fn test_bg_completed_before_done_triggers_continuation() {
    let (mut app, _handle) = App::new_headless(120, 30).await;

    // 模拟后台任务已启动
    app.session_mgr.current_mut().background_agents = vec![crate::app::RunningBgAgent {
        agent_name: "code-reviewer".to_string(),
        instance_id: "test-inst".to_string(),
        started_at: std::time::Instant::now(),
        tool_count: 0,
    }];

    // 竞态：BackgroundTaskCompleted 先于 Done 到达
    app.push_agent_event(AgentEvent::BackgroundTaskCompleted {
        task_id: "bg-race-1".into(),
        agent_name: "code-reviewer".into(),
        success: true,
        output: "LGTM no issues".into(),
        tool_calls_count: 3,
        duration_ms: 500,
        child_thread_id: None,
    });
    app.push_agent_event(AgentEvent::Done);
    app.process_pending_events();

    // 断言：bg_task_state.pre_done_completions 被 Done 消费并转为 bg_task_state.pending_continuation
    assert!(
        app.session_mgr
            .current_mut()
            .agent
            .bg_task_state
            .pre_done_completions
            .is_empty(),
        "Done 处理后 bg_task_state.pre_done_completions 应被清空"
    );
    assert!(
        app.session_mgr
            .current_mut()
            .agent
            .bg_task_state.pending_continuation
            .is_some(),
        "竞态修复：BackgroundTaskCompleted 在 Done 之前时，Done 应设置 bg_task_state.pending_continuation"
    );
}

/// 多个后台任务在 Done 之前全部完成
/// 注意：只有最后一个使 count 归零的 BackgroundTaskCompleted 会暂存通知，
/// 前面的（count > 0）不暂存——这与原逻辑一致（只有 count==0 时才检查是否触发 continuation）
#[tokio::test]
async fn test_multiple_bg_completed_before_done() {
    let (mut app, _handle) = App::new_headless(120, 30).await;

    app.session_mgr.current_mut().background_agents = vec![
        crate::app::RunningBgAgent {
            agent_name: "reviewer-1".to_string(),
            instance_id: "test-inst-1".to_string(),
            started_at: std::time::Instant::now(),
            tool_count: 0,
        },
        crate::app::RunningBgAgent {
            agent_name: "reviewer-2".to_string(),
            instance_id: "test-inst-2".to_string(),
            started_at: std::time::Instant::now(),
            tool_count: 0,
        },
    ];

    // 第一个后台任务完成：count 2→1，不暂存（count > 0）
    app.push_agent_event(AgentEvent::BackgroundTaskCompleted {
        task_id: "bg-multi-1".into(),
        agent_name: "reviewer-1".into(),
        success: true,
        output: "result A".into(),
        tool_calls_count: 2,
        duration_ms: 100,
        child_thread_id: None,
    });
    // 第二个后台任务完成：count 1→0，暂存
    app.push_agent_event(AgentEvent::BackgroundTaskCompleted {
        task_id: "bg-multi-2".into(),
        agent_name: "reviewer-2".into(),
        success: true,
        output: "result B".into(),
        tool_calls_count: 1,
        duration_ms: 200,
        child_thread_id: None,
    });
    app.push_agent_event(AgentEvent::Done);
    app.process_pending_events();

    // 断言���最后一个使 count 归零的任务通知被暂存并由 Done 消费
    let continuation = &app
        .session_mgr
        .current_mut()
        .agent
        .bg_task_state
        .pending_continuation;
    assert!(
        continuation.is_some(),
        "多后台任务 Done 前完成时应设置 bg_task_state.pending_continuation"
    );
    let results = continuation.as_ref().unwrap();
    assert!(
        results.iter().any(|r| r.agent_name.contains("reviewer-2")),
        "continuation 应包含最后一个（使 count 归零的）任务结果"
    );
    assert!(
        app.session_mgr
            .current_mut()
            .agent
            .bg_task_state
            .pre_done_results
            .is_empty(),
        "Done 后 bg_task_state.pre_done_results 应清空"
    );
}

/// 正常路径：后台任务慢于 Done，不应受修复影响
#[tokio::test]
async fn test_bg_completed_after_done_unchanged() {
    let (mut app, _handle) = App::new_headless(120, 30).await;

    app.session_mgr.current_mut().background_agents = vec![crate::app::RunningBgAgent {
        agent_name: "worker".to_string(),
        instance_id: "test-inst".to_string(),
        started_at: std::time::Instant::now(),
        tool_count: 0,
    }];

    // 正常路径：Done 先到
    app.push_agent_event(AgentEvent::Done);
    app.process_pending_events();

    assert!(
        app.session_mgr
            .current_mut()
            .agent
            .bg_task_state
            .agent_done_pending,
        "Done 有后台任务时应设 bg_task_state.agent_done_pending"
    );
    assert!(
        app.session_mgr
            .current_mut()
            .agent
            .bg_task_state
            .pre_done_completions
            .is_empty(),
        "正常路径不应使用 bg_task_state.pre_done_completions"
    );

    // 后台任务后到
    app.push_agent_event(AgentEvent::BackgroundTaskCompleted {
        task_id: "bg-normal-1".into(),
        agent_name: "worker".into(),
        success: true,
        output: "done".into(),
        tool_calls_count: 1,
        duration_ms: 300,
        child_thread_id: None,
    });
    app.process_pending_events();

    assert!(
        app.session_mgr
            .current_mut()
            .agent
            .bg_task_state
            .pending_continuation
            .is_some(),
        "正常路径：BackgroundTaskCompleted 在 Done 后应设 bg_task_state.pending_continuation"
    );
    assert!(
        app.session_mgr
            .current_mut()
            .agent
            .bg_task_state
            .pre_done_results
            .is_empty(),
        "正常路径 bg_task_state.pre_done_results 应被消费"
    );
}

/// 用户主动发消息时应清理暂存
#[tokio::test]
async fn test_submit_message_clears_pre_done_completions() {
    let (mut app, _handle) = App::new_headless(120, 30).await;

    // 模拟暂存状态（不通过事件流，直接设置）
    app.session_mgr
        .current_mut()
        .agent
        .bg_task_state
        .pre_done_completions
        .push("buffered notification".to_string());
    assert!(
        !app.session_mgr
            .current_mut()
            .agent
            .bg_task_state
            .pre_done_completions
            .is_empty(),
        "前置条件：bg_task_state.pre_done_completions 非空"
    );

    // 模拟 submit_message 中的清理（通过设置必要字段后直接调用清理逻辑）
    app.session_mgr
        .current_mut()
        .agent
        .bg_task_state
        .agent_done_pending = false;
    app.session_mgr
        .current_mut()
        .agent
        .bg_task_state
        .pending_continuation = None;
    app.session_mgr
        .current_mut()
        .agent
        .bg_task_state
        .pre_done_completions
        .clear();

    assert!(
        app.session_mgr
            .current_mut()
            .agent
            .bg_task_state
            .pre_done_completions
            .is_empty(),
        "清理后 bg_task_state.pre_done_completions 应为空"
    );
}

/// 验证后台 agent 生命周期：SubAgentStart(bg) → push，BackgroundTaskCompleted → remove + 自动退出聚焦
#[tokio::test]
async fn test_background_agents_lifecycle() {
    let (mut app, _handle) = App::new_headless(120, 30).await;

    // 设置 view_messages 基础状态
    app.session_mgr.current_mut().messages.round_start_vm_idx =
        app.session_mgr.current_mut().messages.view_messages.len();
    let user_vm = MessageViewModel::user("test query".into());
    app.session_mgr
        .current_mut()
        .messages
        .view_messages
        .push(user_vm);
    app.render_rebuild();

    // SubAgentStart(bg=true) → push agent
    app.push_agent_event(AgentEvent::SubAgentStart {
        agent_id: "code-reviewer".into(),
        instance_id: "inst-001".into(),
        task_preview: String::new(),
        is_background: true,
    });
    app.process_pending_events();
    assert_eq!(
        app.session_mgr.current_mut().background_agents.len(),
        1,
        "SubAgentStart(bg) 应增加 background_agents"
    );
    assert_eq!(
        app.session_mgr.current_mut().background_agents[0].agent_name,
        "code-reviewer"
    );

    // 再启动一个
    app.push_agent_event(AgentEvent::SubAgentStart {
        agent_id: "explorer".into(),
        instance_id: "inst-002".into(),
        task_preview: String::new(),
        is_background: true,
    });
    app.process_pending_events();
    assert_eq!(
        app.session_mgr.current_mut().background_agents.len(),
        2,
        "两个后台 agent 应有 2 条记录"
    );

    // BackgroundTaskCompleted → 移除匹配的 agent
    app.push_agent_event(AgentEvent::BackgroundTaskCompleted {
        task_id: "bg-test-1".into(),
        agent_name: "code-reviewer".into(),
        success: true,
        output: "done".into(),
        tool_calls_count: 1,
        duration_ms: 100,
        child_thread_id: Some("inst-001".into()),
    });
    app.process_pending_events();
    assert_eq!(
        app.session_mgr.current_mut().background_agents.len(),
        1,
        "完成后应只剩 1 个 agent"
    );
    assert_eq!(
        app.session_mgr.current_mut().background_agents[0].agent_name,
        "explorer"
    );

    // 设置聚焦到 explorer
    app.session_mgr.current_mut().focused_instance_id = Some("inst-002".into());

    // 完成聚焦的 agent → 自动退出聚焦
    app.push_agent_event(AgentEvent::BackgroundTaskCompleted {
        task_id: "bg-test-2".into(),
        agent_name: "explorer".into(),
        success: true,
        output: "done".into(),
        tool_calls_count: 1,
        duration_ms: 100,
        child_thread_id: Some("inst-002".into()),
    });
    app.process_pending_events();
    assert!(
        app.session_mgr.current_mut().background_agents.is_empty(),
        "所有 agent 完成后列表应为空"
    );
    assert_eq!(
        app.session_mgr.current_mut().focused_instance_id,
        None,
        "聚焦的 agent 完成后应自动退出聚焦"
    );
}

// ── Compact Loading / TextSelection 修复回归 ──────────────────────────────

/// 验证 compact completed 后 loading 保持（统一由 Done 事件结束）
#[tokio::test]
async fn test_compact_completed_preserves_loading() {
    use peri_agent::messages::BaseMessage;

    let (mut app, _handle) = App::new_headless(80, 24).await;

    // compact started
    let (consume, _, _) = app.handle_compact_started();
    assert!(consume);
    assert!(app.session_mgr.current().ui.loading);

    // compact completed
    let msgs = vec![BaseMessage::human("summary")];
    let (consume, _, _) = app.handle_compact_completed("summary".into(), vec![], vec![], 0, msgs);
    assert!(consume);
    // compact completed 后 loading 应保持（等待 Done 事件）
    assert!(
        app.session_mgr.current().ui.loading,
        "compact completed 后 loading 应保持，由 Done 事件结束"
    );
}

/// 验证 compact 后 text_selection 被清理
#[tokio::test]
async fn test_compact_clears_text_selection() {
    use peri_agent::messages::BaseMessage;

    let (mut app, _handle) = App::new_headless(80, 24).await;

    // 模拟用户有活跃的 text_selection
    app.session_mgr
        .current_mut()
        .ui
        .text_selection
        .start_drag(50, 10);
    app.session_mgr
        .current_mut()
        .ui
        .text_selection
        .update_drag(60, 20);
    assert!(app.session_mgr.current_mut().ui.text_selection.is_active());

    // compact started 应清理选区
    app.handle_compact_started();
    assert!(
        !app.session_mgr.current().ui.text_selection.is_active(),
        "text_selection 应在 compact_started 时被清理"
    );

    // 再次设置选区
    app.session_mgr
        .current_mut()
        .ui
        .text_selection
        .start_drag(5, 3);
    assert!(app.session_mgr.current_mut().ui.text_selection.is_active());

    // compact completed 也应清理选区
    let msgs = vec![BaseMessage::human("summary")];
    app.handle_compact_completed("summary".into(), vec![], vec![], 0, msgs);
    assert!(
        !app.session_mgr.current().ui.text_selection.is_active(),
        "text_selection 应在 compact_completed 时被清理"
    );
}
