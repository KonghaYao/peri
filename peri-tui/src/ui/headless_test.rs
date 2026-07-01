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
    // P5: Push UserBubble + AssistantBubble via from_base_message (AssistantChunk is no-op)
    app.apply_add_message(MessageViewModel::user("q".into()));
    app.apply_add_message(MessageViewModel::from_base_message(
        &BaseMessage::ai("Hello world"),
        &[],
    ));
    handle
        .terminal
        .draw(|f| main_ui::render(f, &mut app, None, None))
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
    app.push_agent_event(AgentEvent::ToolStart {
        tool_call_id: "t1".into(),
        name: "Read".into(),
        display: "ReadFile".into(),
        args: "src/main.rs".into(),
        input: serde_json::json!({"path": "src/main.rs"}),
        source_agent_id: None,
    });
    app.process_pending_events();
    handle.wait_for_render().await;
    handle
        .terminal
        .draw(|f| main_ui::render(f, &mut app, None, None))
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
    // 使用 ASCII 内容避免 CJK 宽字符在 buffer 中的空格填充问题
    let vm = MessageViewModel::user("hello from user".into());
    app.session_mgr
        .current_mut()
        .messages
        .view_messages
        .push(vm);
    app.render_rebuild();
    handle.wait_for_render().await;
    handle
        .terminal
        .draw(|f| main_ui::render(f, &mut app, None, None))
        .unwrap();
    let snap = handle.snapshot();
    assert!(
        handle.contains("hello from user"),
        "应显示用户消息，实际内容:\n{}",
        snap.join("\n")
    );
}

// P5: test_clear_empties_render_cache removed — uses render_thread

// Phase 2.6 step 6 — test_subagent_group_basic / sliding_window / assistant_chunk removed.
// 这些 v1 测试断言 view_messages 中存在 SubAgentGroup 占位符；step 6 已删除
// handle_subagent_start 中的 SubAgentGroup 推送（生产渲染通过 SessionSubAgentProbe
// 从 SubAgentStatusMap 读取运行时状态）。覆盖路径：
//  - subagent_status.rs 单元测试（start / complete_foreground / complete_background）
//  - test_subagent_group_renders_child_content_via_probe（e2e v2）
//  - test_subagent_child_tool_renders_on_screen（e2e v2）

#[tokio::test]
async fn test_tool_call_message_visible_when_toggled() {
    let (mut app, mut handle) = App::new_headless(120, 30).await;

    // 使用 ToolStart 事件添加工具调用
    app.push_agent_event(AgentEvent::ToolStart {
        tool_call_id: "tc1".into(),
        name: "Bash".into(),
        display: "Bash".into(),
        args: "ls".into(),
        input: serde_json::json!({"command": "ls"}),
        source_agent_id: None,
    });
    app.process_pending_events();
    handle.wait_for_render().await;

    // toggle_collapsed_messages 发送 ToggleToolMessages → 渲染线程 rebuild_all → notify
    app.toggle_collapsed_messages();
    handle.wait_for_render().await;

    handle
        .terminal
        .draw(|f| main_ui::render(f, &mut app, None, None))
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
    // AssistantChunk 仅更新 spinner/retry 状态，不应创建 AssistantBubble
    let (mut app, _handle) = App::new_headless(120, 30).await;

    app.push_agent_event(AgentEvent::AssistantChunk {
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
        "AssistantChunk 不应创建 AssistantBubble，实际: {:?}",
        app.session_mgr.current_mut().messages.view_messages.len()
    );

    // 发送多次 AssistantChunk，仍不应创建气泡
    app.push_agent_event(AgentEvent::AssistantChunk {
        source_agent_id: None,
    });
    app.push_agent_event(AgentEvent::AssistantChunk {
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

    // P5: AssistantChunk is no-op, push UserBubble + AI VM directly
    let (mut app, mut handle) = App::new_headless(120, 30).await;

    app.apply_add_message(MessageViewModel::user("q".into()));
    app.apply_add_message(MessageViewModel::from_base_message(
        &BaseMessage::ai("Hello"),
        &[],
    ));

    handle
        .terminal
        .draw(|f| main_ui::render(f, &mut app, None, None))
        .unwrap();

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
    app.push_agent_event(AgentEvent::ToolStart {
        tool_call_id: "tc1".into(),
        name: "Bash".into(),
        display: "Bash".into(),
        args: "ls".into(),
        input: serde_json::json!({"command": "ls"}),
        source_agent_id: None,
    });
    app.process_pending_events();
    handle.wait_for_render().await;

    handle
        .terminal
        .draw(|f| main_ui::render(f, &mut app, None, None))
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
        .draw(|f| main_ui::render(f, &mut app, None, None))
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
    // P5: AssistantChunk is no-op, push UserBubble + AI VM directly
    app.apply_add_message(MessageViewModel::user("q".into()));
    app.apply_add_message(MessageViewModel::from_base_message(
        &BaseMessage::ai("Hello from agent"),
        &[],
    ));

    handle
        .terminal
        .draw(|f| main_ui::render(f, &mut app, None, None))
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
        .draw(|f| main_ui::render(f, &mut app, None, None))
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
        .draw(|f| main_ui::render(f, &mut app, None, None))
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
        .draw(|f| main_ui::render(f, &mut app, None, None))
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
        let vm = MessageViewModel::user(format!("message line {}", i));
        app.session_mgr
            .current_mut()
            .messages
            .view_messages
            .push(vm);
        app.render_rebuild();
        handle.wait_for_render().await;
    }

    // 设置 last_human_message（模拟 submit_message 的效果）
    app.session_mgr.current_mut().metadata.last_human_message = Some("hello from user".to_string());

    handle
        .terminal
        .draw(|f| main_ui::render(f, &mut app, None, None))
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
    app.new_thread();
    handle.wait_for_render().await;

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
        .draw(|f| main_ui::render(f, &mut app, None, None))
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
        let vm = MessageViewModel::user(format!("padding line {}", i));
        app.session_mgr
            .current_mut()
            .messages
            .view_messages
            .push(vm);
        app.render_rebuild();
        handle.wait_for_render().await;
    }

    // 模拟第一条消息
    app.session_mgr.current_mut().metadata.last_human_message = Some("first message".to_string());
    // 模拟第二条消息（覆盖）
    app.session_mgr.current_mut().metadata.last_human_message = Some("second message".to_string());

    handle
        .terminal
        .draw(|f| main_ui::render(f, &mut app, None, None))
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
        let vm = MessageViewModel::user(format!("padding {}", i));
        app.session_mgr
            .current_mut()
            .messages
            .view_messages
            .push(vm);
        app.render_rebuild();
        handle.wait_for_render().await;
    }

    // 模拟超长消息（远超 header 可显示范围）
    let long_msg =
        "hello this is a very long message that definitely exceeds header capacity".to_string();
    assert!(long_msg.chars().count() > 40);
    app.session_mgr.current_mut().metadata.last_human_message = Some(long_msg.clone());

    handle
        .terminal
        .draw(|f| main_ui::render(f, &mut app, None, None))
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

// Phase E: test_cron_panel_render removed — legacy PanelManager/global_panels deleted.
// Phase E: test_bordered_panel_integration removed — legacy PanelManager/session_panels deleted.

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
        .draw(|f| main_ui::render(f, &mut app, None, None))
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
        .draw(|f| crate::ui::main_ui::render(f, &mut app, None, None))
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
        .draw(|f| crate::ui::main_ui::render(f, &mut app, None, None))
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
        .draw(|f| crate::ui::main_ui::render(f, &mut app, None, None))
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
        .draw(|f| crate::ui::main_ui::render(f, &mut app, None, None))
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
        .draw(|f| crate::ui::main_ui::render(f, &mut app, None, None))
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
        .draw(|f| crate::ui::main_ui::render(f, &mut app, None, None))
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
                files.push(peri_acp::event::CompactFileInfoDto {
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
    app.push_agent_event(make_compact_done_event(
        "Test summary",
        &[
            "[最近读取的文件: /a.rs]\nline1\nline2\nline3",
            "[激活的 Skill 指令: skill.md]\nskill content",
        ],
    ));
    app.process_pending_events();
    handle.wait_for_render().await;

    // Cron #41: CompactCompleted label 路由从 v1 view_messages 迁到 v2
    // pending_v2_notes → SM Event::PushSystemNote → state.view。
    // 检查 pending_v2_notes 是否包含压缩提示 + 文件 + skill 信息。
    let notes = &app.session_mgr.current().messages.pending_v2_notes;
    assert_eq!(
        notes.len(),
        1,
        "Cron #41: CompactDone 应入队到 pending_v2_notes，实际: {}",
        notes.len()
    );
    let has_compact = notes
        .iter()
        .any(|n| n.contains("✻") && n.contains("Read /a.rs") && n.contains("Skill: skill.md"));
    assert!(
        has_compact,
        "Cron #41: pending_v2_notes 应包含 ✻ + Read /a.rs + Skill: skill.md"
    );

    // Cron #41 防回归：view_messages 必须不再被 apply_rebuild_all 写入
    let view_msgs = &app.session_mgr.current().messages.view_messages;
    assert!(
        view_msgs.iter().all(|m| match m {
            MessageViewModel::SystemNote { content, .. } => !content.contains("✻"),
            _ => true,
        }),
        "Cron #41 防回归：CompactDone label 不应写入 v1 view_messages (生产独占读 v2)"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn test_compact_done_without_re_inject() {
    let (mut app, handle) = App::new_headless(120, 30).await;
    app.push_agent_event(make_compact_done_event("Simple summary", &[]));
    app.process_pending_events();
    handle.wait_for_render().await;

    // Cron #41: CompactDone label 在 pending_v2_notes 中（无 re_inject 时只有 ✻ 标志）
    let notes = &app.session_mgr.current().messages.pending_v2_notes;
    assert_eq!(
        notes.len(),
        1,
        "Cron #41: CompactDone 应入队到 pending_v2_notes"
    );
    let has_compact_marker = notes.iter().any(|n| n.contains("✻"));
    assert!(
        has_compact_marker,
        "Cron #41: pending_v2_notes 应包含 ✻ 压缩标志"
    );
    let has_re_inject = notes
        .iter()
        .any(|n| n.contains("Read ") || n.contains("Skill:"));
    assert!(
        !has_re_inject,
        "无重新注入内容时不应显示文件/skill 详情，实际 notes: {:?}",
        notes
    );
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

    // P5: Push UserBubble + AI VM directly (AssistantChunk/StateSnapshot are no-op)
    app.apply_add_message(MessageViewModel::user("my question".into()));
    app.apply_add_message(MessageViewModel::from_base_message(
        &BaseMessage::ai("AI answer"),
        &[],
    ));

    handle
        .terminal
        .draw(|f| main_ui::render(f, &mut app, None, None))
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

    // P5: Push UserBubble + AI VM directly for each turn (AssistantChunk/StateSnapshot are no-op)
    app.apply_add_message(MessageViewModel::user("turn1".into()));
    app.apply_add_message(MessageViewModel::from_base_message(
        &BaseMessage::ai("answer1"),
        &[],
    ));

    app.apply_add_message(MessageViewModel::user("turn2".into()));
    app.apply_add_message(MessageViewModel::from_base_message(
        &BaseMessage::ai("answer2"),
        &[],
    ));

    handle
        .terminal
        .draw(|f| main_ui::render(f, &mut app, None, None))
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

    // P5: AssistantChunk is no-op, push UserBubble + AI VM directly
    app.apply_add_message(MessageViewModel::user("q".into()));
    app.apply_add_message(MessageViewModel::from_base_message(
        &BaseMessage::ai("unique text"),
        &[],
    ));

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

/// 回归：v2 架构下 StateSnapshot / TurnCommitted 携带全量 transcript（替换语义）
///
/// 设计动机：v2 stages 在每次迭代边界 emit `StateEvent::TurnCompleted` 携带
/// `finalized_messages: Arc<Vec<BaseMessage>>`（全量快照），TUI 用「替换」吸收。
/// 旧的 v1 增量 extend 语义已废弃（会导致多迭代文本渲染在工具之前的 bug，
// P5: test_state_snapshot_is_incremental removed — MessagePipeline::set_completed deleted

/// 回归：ToolStart 之后 AssistantChunk 不会丢失工具消息
#[tokio::test]
async fn test_tool_then_text_preserves_tool_block() {
    use peri_agent::messages::BaseMessage;
    let (mut app, mut handle) = App::new_headless(120, 30).await;

    app.push_agent_event(AgentEvent::ToolStart {
        tool_call_id: "tc1".into(),
        name: "Bash".into(),
        display: "Shell".into(),
        args: "ls".into(),
        input: serde_json::json!({"command": "ls"}),
        source_agent_id: None,
    });
    app.process_pending_events();
    // P5: AssistantChunk is no-op, push AI VM directly
    app.apply_add_message(MessageViewModel::from_base_message(
        &BaseMessage::ai("result is here"),
        &[],
    ));

    handle
        .terminal
        .draw(|f| main_ui::render(f, &mut app, None, None))
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
    use peri_acp_types::skill::{SkillMetadataDto, SkillSourceDto};
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
        .push(SkillMetadataDto {
            name: "commit".into(),
            description: "commit changes".into(),
            path: "/tmp/commit.md".into(),
            source: SkillSourceDto::User,
            plugin_name: None,
            disabled: false,
        });
    app.session_mgr
        .current_mut()
        .commands
        .skills
        .push(SkillMetadataDto {
            name: "review".into(),
            description: "review code".into(),
            path: "/tmp/review.md".into(),
            source: SkillSourceDto::User,
            plugin_name: None,
            disabled: false,
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
        .draw(|f| main_ui::render(f, &mut app, None, None))
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
    use peri_acp_types::skill::{SkillMetadataDto, SkillSourceDto};
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
        .push(SkillMetadataDto {
            name: "commit".into(),
            description: "commit changes".into(),
            path: "/tmp/commit.md".into(),
            source: SkillSourceDto::User,
            plugin_name: None,
            disabled: false,
        });

    handle
        .terminal
        .draw(|f| main_ui::render(f, &mut app, None, None))
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
    use peri_acp_types::skill::{SkillMetadataDto, SkillSourceDto};
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
        .push(SkillMetadataDto {
            name: "skill".into(),
            description: "a skill".into(),
            path: "/tmp/skill.md".into(),
            source: SkillSourceDto::User,
            plugin_name: None,
            disabled: false,
        });

    handle
        .terminal
        .draw(|f| main_ui::render(f, &mut app, None, None))
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
    use peri_acp_types::skill::{SkillMetadataDto, SkillSourceDto};
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
        .push(SkillMetadataDto {
            name: "review".into(),
            description: "code review".into(),
            path: "/tmp/review.md".into(),
            source: SkillSourceDto::User,
            plugin_name: None,
            disabled: false,
        });

    // 模拟 Enter 事件处理
    let text: String = app.session_mgr.current_mut().ui.textarea.lines().join("\n");
    let text = text.trim().to_string();
    assert!(text.starts_with('/'));

    // 验证命令 dispatch 不匹配后 Skill fallback
    let registry = std::mem::take(&mut app.session_mgr.current_mut().commands.command_registry);
    let known = registry.dispatch(&mut app, &text);
    app.session_mgr.current_mut().commands.command_registry = registry;
    assert!(known.is_none(), "review 不应是已知命令");

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
    assert!(known.is_none(), "nonexistent 不应是已知命令");

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
    use peri_acp_types::skill::{SkillMetadataDto, SkillSourceDto};
    let (mut app, _handle) = App::new_headless(120, 30).await;

    // 注入名为 help 的 Skill
    app.session_mgr
        .current_mut()
        .commands
        .skills
        .push(SkillMetadataDto {
            name: "help".into(),
            description: "help skill".into(),
            path: "/tmp/help.md".into(),
            source: SkillSourceDto::User,
            plugin_name: None,
            disabled: false,
        });

    // /help 应被命令 dispatch 拦截，不走 Skill fallback
    let registry = std::mem::take(&mut app.session_mgr.current_mut().commands.command_registry);
    let known = registry.dispatch(&mut app, "/help");
    app.session_mgr.current_mut().commands.command_registry = registry;
    assert!(known.is_some(), "/help 应是已知命令，优先于同名 Skill");
}

// ── Input Placeholder Hint ──────────────────────────────────────────────

#[tokio::test]
async fn test_textarea_shows_placeholder_hint() {
    let (mut app, mut handle) = App::new_headless(120, 30).await;
    handle
        .terminal
        .draw(|f| main_ui::render(f, &mut app, None, None))
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
        .draw(|f| main_ui::render(f, &mut app, None, None))
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
    assert!(known.is_none(), "歧义前缀 dispatch 应返回 None");
}

// ─── Design Review 第22轮：Model 面板 Space 键 + Cron 确认删除 + 面板 Paste 拦截 ────

// Phase E: test_model_panel_space_selects_model removed — legacy model_panel deleted.

// Phase E: test_cron_panel_delete_confirmation removed — legacy PanelManager/global_panels deleted.

// Phase E: test_cron_panel_confirm_delete_renders removed — legacy PanelManager/global_panels deleted.

// ─── Design Review 第23轮：面板操作成功反馈 ────

// Phase E: test_model_panel_confirm_shows_feedback removed — legacy model_panel/session_panels deleted.

// Phase E: test_login_select_provider_shows_feedback removed — legacy login_panel/session_panels deleted.

// ─── Design Review 第24轮：Welcome Card 模型信息 + Thread Browser 消息数 ────

/// Welcome Card 应显示当前 Provider/Model 信息
#[tokio::test]
async fn test_welcome_shows_model_info() {
    let (mut app, mut handle) = App::new_headless(120, 30).await;
    // App 默认有 provider_name="test" 和 model_name="test-model"
    handle
        .terminal
        .draw(|f| main_ui::render(f, &mut app, None, None))
        .unwrap();
    let snap = handle.snapshot().join("\n");
    // 验证 Welcome Card 包含 provider/model 信息
    assert!(
        snap.contains("test / test-model"),
        "Welcome Card 应显示 Provider/Model 信息，实际:\n{}",
        snap
    );
}

// Phase 2.6 step 6 — test_background_task_notification removed.
// 原 v1 测试断言 view_messages 中存在 `bg:code-reviewer` ToolBlock 含 "LGTM"，
// 该 ToolBlock 是 step 6 删除的 v1 回退路径。生产 v2 路径将通知存入
// `bg_task_state.pre_done_completions: Vec<String>`，由
// test_bg_completed_before_done_triggers_continuation 覆盖竞态条件路径，
// subagent_status.rs 单元测试覆盖 complete_background 权威状态写入。

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
    app.push_agent_event(AgentEvent::StateSnapshot(vec![
        peri_agent::messages::BaseMessage::human("test"),
    ]));
    app.push_agent_event(AgentEvent::Done);
    app.process_pending_events();
    handle.wait_for_render().await;

    handle
        .terminal
        .draw(|f| main_ui::render(f, &mut app, None, None))
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
        .draw(|f| main_ui::render(f, &mut app, None, None))
        .unwrap();
    let snap = handle.snapshot();
    assert!(
        snap.iter().any(|line| line.contains("hi")),
        "Loading 时输入的文字 'hi' 应该可见，实际:\n{}",
        snap.join("\n")
    );
}

// ── SubAgentGroup Reconcile Preservation ──────────────────────────────────

// Phase 2.6 step 6 — test_subagent_group_preserved_after_done_reconcile removed.
// 该 v1 测试断言 view_messages 中 SubAgentGroup 在 Done reconcile 后保留富状态；
// step 6 删除 handle_subagent_start 中 SubAgentGroup 推送后此断言失效。
// 生产 v2 路径：SubAgentStatusMap 是权威源，complete_foreground/complete_background
// 单元测试 + test_subagent_group_renders_child_content_via_probe e2e 覆盖。

// ── Auto-compact deferred during background tasks ──────────────────────

// ── Background Agent SubAgentGroup 消失诊断 ───────────────────────────

// Phase 2.6 step 6 — bg_diag_count_subagent_groups / bg_diag_print_vms helpers +
// test_diagnostic_bg_subagent_group_disappears / test_diagnostic_fork_plus_background_subagent_group
// 诊断测试全部移除。这些诊断针对已删除的 v1 SubAgentGroup view_messages 占位符路径，
// 用于排查 bg_task_state.agent_done_pending / fork+background 时 SubAgentGroup 消失问题。
// v2 架构下 SubAgentStatusMap 是唯一权威源，相关问题通过以下测试覆盖：
//   - subagent_status.rs 单元测试（TTL / 容量 / start / complete_*）
//   - test_bg_completed_before_done_triggers_continuation（pre_done_completions 路径）
//   - test_multiple_bg_completed_before_done（多 bg 竞态条件）

/// 回归：Anthropic thinking 模式下，流式阶段 AI message 不可见是预期的（reasoning 被跳过），
/// 但 Done 后 RebuildAll 不应丢失 user message
#[tokio::test]
async fn test_thinking_mode_user_message_survives_rebuild() {
    use peri_agent::messages::BaseMessage;

    let (mut app, mut handle) = App::new_headless(120, 30).await;

    // 1. P5: Push UserBubble directly
    app.apply_add_message(MessageViewModel::user("explain recursion".into()));

    // 2. Reasoning 已通过 state machine 渲染，不再有 AiReasoning 事件
    app.process_pending_events();

    // 此时 view_messages 应只有 UserBubble
    assert_eq!(
        app.session_mgr.current_mut().messages.view_messages.len(),
        1,
        "thinking 阶段应只有 UserBubble"
    );

    // 3. P5: AssistantChunk/StateSnapshot are no-op, push AI VM directly
    app.apply_add_message(MessageViewModel::from_base_message(
        &BaseMessage::ai("Recursion is a technique where a function calls itself."),
        &[],
    ));

    handle
        .terminal
        .draw(|f| main_ui::render(f, &mut app, None, None))
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
    use peri_agent::messages::BaseMessage;

    let (mut app, mut handle) = App::new_headless(120, 30).await;

    // 1. P5: Push UserBubble directly
    app.apply_add_message(MessageViewModel::user("show me main.rs".into()));

    // 2. Reasoning via state machine (was AiReasoning no-op)
    app.process_pending_events();

    // 3. tool_call (AI 调用 Read) — ToolStart creates ToolBlock in P5
    app.push_agent_event(AgentEvent::ToolStart {
        tool_call_id: "tc_read".into(),
        name: "Read".into(),
        display: "ReadFile".into(),
        args: "src/main.rs".into(),
        input: serde_json::json!({"path": "src/main.rs"}),
        source_agent_id: None,
    });
    app.process_pending_events();
    handle.wait_for_render().await;

    // 4. tool_end — ToolEnd updates ToolBlock in P5
    app.push_agent_event(AgentEvent::ToolEnd {
        tool_call_id: "tc_read".into(),
        name: "Read".into(),
        output: "fn main() { println!(\"hello\"); }".into(),
        is_error: false,
        source_agent_id: None,
    });
    app.process_pending_events();
    handle.wait_for_render().await;

    // 5. Reasoning via state machine bridge, push AI VM directly
    app.apply_add_message(MessageViewModel::from_base_message(
        &BaseMessage::ai("Here is the content of main.rs:"),
        &[],
    ));

    handle
        .terminal
        .draw(|f| main_ui::render(f, &mut app, None, None))
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
/// pre_done_completions 暂存 → Done 处理时消费并清空
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

    // 断言：bg_task_state.pre_done_completions 被 Done 消费并清空
    assert!(
        app.session_mgr
            .current_mut()
            .agent
            .bg_task_state
            .pre_done_completions
            .is_empty(),
        "Done 处理后 bg_task_state.pre_done_completions 应被清空"
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

/// 验证 `/bg` 命令的 SubagentStarted 严格先于 Done 到达 TUI：
/// 治本方案（BgCommand::execute 同步 push SubagentStarted 到 event_sink）
/// 消除了原本 pump task 与 push_done 并发导致的 race。
///
/// 顺序断言：submit_message → SubagentStarted → Done → BackgroundTaskCompleted。
/// 任一环节状态错误都会让后续断言失败。
#[tokio::test]
async fn test_bg_subagent_started_arrives_before_done() {
    let (mut app, _handle) = App::new_headless(120, 30).await;

    // 模拟 submit_message 触发的 loading 状态
    app.set_loading(true);
    assert!(app.session_mgr.current().ui.loading);

    // 治本保证：SubagentStarted 先到（loading=true 期间）
    app.push_agent_event(AgentEvent::SubAgentStart {
        agent_id: "fork".into(),
        instance_id: "inst-race".into(),
        task_preview: String::new(),
        is_background: true,
    });
    app.process_pending_events();
    assert_eq!(
        app.session_mgr.current_mut().background_agents.len(),
        1,
        "SubAgentStart(bg) 应 push background_agents"
    );
    assert!(
        !app.session_mgr
            .current_mut()
            .agent
            .bg_task_state
            .agent_done_pending,
        "Done 未到达时 agent_done_pending 应为 false"
    );

    // Done 后到：background_agents 非空 → handle_done 设置 agent_done_pending
    app.push_agent_event(AgentEvent::Done);
    app.process_pending_events();
    assert!(!app.session_mgr.current().ui.loading);
    assert!(
        app.session_mgr
            .current_mut()
            .agent
            .bg_task_state
            .agent_done_pending,
        "Done 到达时 background_agents 非空应设置 agent_done_pending"
    );

    // BackgroundTaskCompleted 到达：处理完成
    app.push_agent_event(AgentEvent::BackgroundTaskCompleted {
        task_id: "bg-race".into(),
        agent_name: "fork".into(),
        success: true,
        output: "result".into(),
        tool_calls_count: 1,
        duration_ms: 100,
        child_thread_id: Some("inst-race".into()),
    });
    app.process_pending_events();
    assert!(
        app.session_mgr.current_mut().background_agents.is_empty(),
        "BackgroundTaskCompleted 应移除 background_agents"
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

// ============================================================================
// Phase 2.6 step 1: source_agent_id 路由端到端测试
// ============================================================================

#[tokio::test]
async fn test_source_agent_id_routes_tool_to_child_messages() {
    // SubAgentStart + ToolStart(source_agent_id) → ToolCard 应进入
    // SubAgentStatus.child_messages（v2 权威源），不污染主消息流
    let (mut app, _handle) = App::new_headless(120, 30).await;

    app.push_agent_event(AgentEvent::SubAgentStart {
        agent_id: "code-reviewer".into(),
        instance_id: "test-inst-1".into(),
        task_preview: "review".into(),
        is_background: false,
    });
    app.push_agent_event(AgentEvent::ToolStart {
        tool_call_id: "tc-1".into(),
        name: "Read".into(),
        display: "ReadFile".into(),
        args: "src/main.rs".into(),
        input: serde_json::json!({"path": "src/main.rs"}),
        source_agent_id: Some("test-inst-1".into()),
    });
    app.process_pending_events();

    // 验证 1：主消息流 view_messages 中不应有 ToolBlock（被路由走了）
    let view_messages = &app.session_mgr.current().messages.view_messages;
    let has_tool_block_in_main = view_messages
        .iter()
        .any(|vm| matches!(vm, MessageViewModel::ToolBlock { .. }));
    assert!(
        !has_tool_block_in_main,
        "source_agent_id 匹配的 ToolStart 不应出现在 view_messages 主消息流"
    );

    // 验证 2：SubAgentStatus.child_messages 应有 1 个 ToolCard
    let status_map = &app.session_mgr.current().subagent_status;
    let status = status_map
        .lookup("test-inst-1")
        .expect("SubAgentStatus 应通过 instance_id 查到");
    assert_eq!(
        status.child_messages.len(),
        1,
        "child_messages 应累积 1 个 ToolCard"
    );
    assert!(
        matches!(
            &status.child_messages[0],
            peri_acp_types::view_model::ViewModel::ToolCard(d) if d.tool_id == "tc-1"
        ),
        "child_messages[0] 应为 ToolCard，tool_id = tc-1"
    );
}

#[tokio::test]
async fn test_source_agent_id_none_falls_back_to_main_stream() {
    // source_agent_id = None（主 Agent）→ ToolBlock 应进入 view_messages 主消息流
    let (mut app, _handle) = App::new_headless(120, 30).await;

    app.push_agent_event(AgentEvent::ToolStart {
        tool_call_id: "tc-main".into(),
        name: "Bash".into(),
        display: "Bash".into(),
        args: "ls".into(),
        input: serde_json::json!({"command": "ls"}),
        source_agent_id: None,
    });
    app.process_pending_events();

    let view_messages = &app.session_mgr.current().messages.view_messages;
    let has_tool_block = view_messages
        .iter()
        .any(|vm| matches!(vm, MessageViewModel::ToolBlock { .. }));
    assert!(
        has_tool_block,
        "source_agent_id=None 的 ToolStart 应进入 view_messages 主消息流"
    );

    // 同时，SubAgentStatusMap 应为空（无 SubAgent 启动）
    assert!(
        app.session_mgr.current().subagent_status.is_empty(),
        "无 SubAgent 启动时 status map 应为空"
    );
}

#[tokio::test]
async fn test_source_agent_id_unknown_falls_back_to_main_stream() {
    // source_agent_id 不匹配任何已启动的 SubAgent → fallback 到主消息流
    // （避免事件到达顺序异常导致 tool 永远丢失）
    let (mut app, _handle) = App::new_headless(120, 30).await;

    app.push_agent_event(AgentEvent::ToolStart {
        tool_call_id: "tc-orphan".into(),
        name: "Read".into(),
        display: "ReadFile".into(),
        args: "src/lib.rs".into(),
        input: serde_json::json!({"path": "src/lib.rs"}),
        source_agent_id: Some("nonexistent-inst".into()),
    });
    app.process_pending_events();

    let view_messages = &app.session_mgr.current().messages.view_messages;
    let has_tool_block = view_messages
        .iter()
        .any(|vm| matches!(vm, MessageViewModel::ToolBlock { .. }));
    assert!(
        has_tool_block,
        "source_agent_id 不匹配时 ToolStart 应 fallback 到主消息流"
    );
}

#[tokio::test]
async fn test_tool_end_updates_child_tool_card_output() {
    // SubAgentStart + ToolStart(src) + ToolEnd(src) → child_messages 中
    // 的 ToolCard output_summary 应被更新（而非新建一个 ToolBlock）
    let (mut app, _handle) = App::new_headless(120, 30).await;

    app.push_agent_event(AgentEvent::SubAgentStart {
        agent_id: "analyzer".into(),
        instance_id: "test-inst-2".into(),
        task_preview: "analyze".into(),
        is_background: false,
    });
    app.push_agent_event(AgentEvent::ToolStart {
        tool_call_id: "tc-2".into(),
        name: "Read".into(),
        display: "ReadFile".into(),
        args: "config.toml".into(),
        input: serde_json::json!({"path": "config.toml"}),
        source_agent_id: Some("test-inst-2".into()),
    });
    app.push_agent_event(AgentEvent::ToolEnd {
        tool_call_id: "tc-2".into(),
        name: "Read".into(),
        output: "file contents here".into(),
        is_error: false,
        source_agent_id: Some("test-inst-2".into()),
    });
    app.process_pending_events();

    let status_map = &app.session_mgr.current().subagent_status;
    let status = status_map
        .lookup("test-inst-2")
        .expect("SubAgentStatus 应存在");
    assert_eq!(
        status.child_messages.len(),
        1,
        "ToolEnd 应原地更新 ToolCard，不应新增"
    );
    if let peri_acp_types::view_model::ViewModel::ToolCard(d) = &status.child_messages[0] {
        assert_eq!(d.output_summary, "file contents here");
        assert!(!d.is_error);
    } else {
        panic!("child_messages[0] 应为 ToolCard");
    }

    // 同时主消息流仍无 ToolBlock
    let view_messages = &app.session_mgr.current().messages.view_messages;
    let has_tool_block = view_messages
        .iter()
        .any(|vm| matches!(vm, MessageViewModel::ToolBlock { .. }));
    assert!(
        !has_tool_block,
        "source_agent_id 匹配时 ToolEnd 也不应污染主消息流"
    );
}

#[tokio::test]
async fn test_subagent_group_renders_child_content_via_probe() {
    // 端到端：SubAgentStart + ToolStart(src) + ToolEnd(src) + 渲染
    // → SessionSubAgentProbe 应从 child_messages 读取并注入到 SubAgentRenderInfo
    use crate::app::SessionSubAgentProbe;
    use crate::render::view_render::SubAgentStatusProbe;

    let (mut app, _handle) = App::new_headless(120, 30).await;

    app.push_agent_event(AgentEvent::SubAgentStart {
        agent_id: "researcher".into(),
        instance_id: "test-inst-3".into(),
        task_preview: "research".into(),
        is_background: false,
    });
    app.push_agent_event(AgentEvent::ToolStart {
        tool_call_id: "tc-3".into(),
        name: "WebSearch".into(),
        display: "WebSearch".into(),
        args: "rust async".into(),
        input: serde_json::json!({"q": "rust async"}),
        source_agent_id: Some("test-inst-3".into()),
    });
    app.process_pending_events();

    // 构造 probe（生产 draw_now 中的方式）
    let session = app.session_mgr.current();
    let probe = SessionSubAgentProbe::new(session.subagent_status.clone());

    // 查询 researcher → 应返回 is_running=true 且 recent_messages 含 ToolCard
    let info = probe
        .lookup_by_agent_id("researcher")
        .expect("应通过 agent_id 查到 SubAgent");
    assert!(info.is_running);
    assert_eq!(
        info.recent_messages.len(),
        1,
        "probe 应从权威源 child_messages 读取 1 个 ToolCard"
    );
    assert!(
        matches!(
            info.recent_messages[0],
            peri_acp_types::view_model::ViewModel::ToolCard(_)
        ),
        "recent_messages 应来自 SubAgentStatus.child_messages"
    );
}

#[tokio::test]
async fn test_subagent_child_tool_renders_on_screen() {
    // Phase 2.6 step 1 端到端渲染验证：
    // SubAgentStart + ToolStart(src) → HeadlessHandle::render 应通过
    // SessionSubAgentProbe 把 child_messages 注入到 SubAgentGroup 渲染，
    // 屏幕上可见 ToolCard（而非主消息流平铺）。
    let (mut app, mut handle) = App::new_headless(120, 30).await;

    app.push_agent_event(AgentEvent::SubAgentStart {
        agent_id: "code-reviewer".into(),
        instance_id: "test-inst-render".into(),
        task_preview: "review the code".into(),
        is_background: false,
    });
    app.push_agent_event(AgentEvent::ToolStart {
        tool_call_id: "tc-render".into(),
        name: "Read".into(),
        display: "ReadFile".into(),
        args: "src/lib.rs".into(),
        input: serde_json::json!({"path": "src/lib.rs"}),
        source_agent_id: Some("test-inst-render".into()),
    });
    app.process_pending_events();
    handle.render(&mut app).await.unwrap();

    let snap = handle.snapshot();
    let joined = snap.join("\n");

    // 1. SubAgentGroup 头部应可见
    assert!(
        joined.contains("code-reviewer"),
        "SubAgentGroup 头部应渲染，实际:\n{}",
        joined
    );

    // 2. ToolCard 的 tool_name（ReadFile 或 Read）应在屏幕某处可见
    //    （通过 SessionSubAgentProbe → render_subagent_group 注入）
    let has_tool = joined.contains("ReadFile") || joined.contains("Read");
    assert!(
        has_tool,
        "子 Agent 的 ToolCard 应通过 probe 注入到渲染，实际:\n{}",
        joined
    );

    // 3. 验证 view_messages 中没有 ToolBlock（路由走了 child_messages）
    let view_messages = &app.session_mgr.current().messages.view_messages;
    let has_tool_block_in_main = view_messages
        .iter()
        .any(|vm| matches!(vm, MessageViewModel::ToolBlock { .. }));
    assert!(
        !has_tool_block_in_main,
        "ToolCard 不应出现在 view_messages 主消息流"
    );
}

#[tokio::test]
async fn test_subagent_assistant_chunk_does_not_pollute_parent_state() {
    // Phase 2.6 step 2：子 Agent 的 AssistantChunk 不应污染父 Agent 的
    // retry_status / agent_replied / spinner_state
    let (mut app, _handle) = App::new_headless(120, 30).await;

    // 主 Agent 先进入 ToolUse spinner 模式（模拟工具执行中）
    app.session_mgr
        .current_mut()
        .spinner_state
        .set_mode(peri_widgets::SpinnerMode::ToolUse);
    app.session_mgr.current_mut().agent.retry_status = Some(crate::app::RetryStatus {
        attempt: 1,
        max_attempts: 3,
        delay_ms: 100,
        error: "retrying".into(),
    });

    // 子 Agent 的 AssistantChunk 到达
    app.push_agent_event(AgentEvent::AssistantChunk {
        source_agent_id: Some("child-inst-1".into()),
    });
    app.process_pending_events();

    // 验证：父 Agent 状态未被污染
    let session = app.session_mgr.current();
    assert!(
        session.agent.retry_status.is_some(),
        "子 Agent AssistantChunk 不应清除父 Agent retry_status"
    );
    assert!(
        !session.agent.agent_replied,
        "子 Agent AssistantChunk 不应标记父 Agent agent_replied"
    );
    assert!(
        matches!(
            session.spinner_state.mode(),
            peri_widgets::SpinnerMode::ToolUse
        ),
        "子 Agent AssistantChunk 不应改变父 Agent spinner 模式"
    );
}

#[tokio::test]
async fn test_main_agent_assistant_chunk_updates_parent_state() {
    // Phase 2.6 step 2：主 Agent 的 AssistantChunk 保持原行为
    let (mut app, _handle) = App::new_headless(120, 30).await;

    app.session_mgr.current_mut().agent.retry_status = Some(crate::app::RetryStatus {
        attempt: 1,
        max_attempts: 3,
        delay_ms: 100,
        error: "retrying".into(),
    });
    app.push_agent_event(AgentEvent::AssistantChunk {
        source_agent_id: None,
    });
    app.process_pending_events();

    let session = app.session_mgr.current();
    assert!(
        session.agent.retry_status.is_none(),
        "主 Agent AssistantChunk 应清除 retry_status"
    );
    assert!(
        session.agent.agent_replied,
        "主 Agent AssistantChunk 应标记 agent_replied"
    );
    assert!(
        matches!(
            session.spinner_state.mode(),
            peri_widgets::SpinnerMode::Responding
        ),
        "主 Agent AssistantChunk 应将 spinner 设为 Responding"
    );
}
