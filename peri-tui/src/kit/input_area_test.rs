//! Tests for input_area

use super::*;
use crate::app::panel_types::PanelKind;
use crate::kit::atoms::{VIEW_MODELS, ViewModelsSnapshot};
use crate::kit::slash_projection::SlashCommandEntry;
use serial_test::serial;
use unicode_width::UnicodeWidthStr;

#[test]
fn test_apply_slash_selection_replaces_only_current_token() {
    let mut s = TextAreaState::default();
    s.insert_str("run /hel after");
    s.cursor = 8;
    apply_slash_selection(&mut s, "help");
    assert_eq!(s.text, "run /help  after");
    assert_eq!(s.cursor, 10);
}

#[test]
fn test_apply_slash_selection_preserves_cjk_before_token() {
    let mut s = TextAreaState::default();
    s.insert_str("你好 /he 后面");
    s.cursor = 6;
    apply_slash_selection(&mut s, "help");
    assert_eq!(s.text, "你好 /help  后面");
    assert_eq!(s.cursor, 9);
}

#[test]
fn test_submit_request_history_aliases() {
    assert_eq!(
        parse_submit_request("/history"),
        Some(SubmitRequest::OpenPanel(PanelKind::ThreadBrowser))
    );
    assert_eq!(
        parse_submit_request("/his"),
        Some(SubmitRequest::OpenPanel(PanelKind::ThreadBrowser))
    );
}

#[test]
fn test_detect_slash_token_rejects_path_or_comment() {
    assert!(detect_slash_token("src/foo", 7).is_none());
    assert!(detect_slash_token("//", 2).is_none());
}

#[test]
fn test_parse_submit_request_opens_model_panel() {
    assert_eq!(
        parse_submit_request("/model"),
        Some(SubmitRequest::OpenPanel(PanelKind::Model))
    );
}

#[test]
fn test_parse_submit_request_resolves_history_aliases() {
    assert_eq!(
        parse_submit_request("/history"),
        Some(SubmitRequest::OpenPanel(PanelKind::ThreadBrowser))
    );
    assert_eq!(
        parse_submit_request("/his"),
        Some(SubmitRequest::OpenPanel(PanelKind::ThreadBrowser))
    );
}

#[test]
fn test_detect_slash_token_accepts_line_start() {
    assert_eq!(
        detect_slash_token("hello\n/com", 10),
        Some(("com".to_string(), 6))
    );
}

fn reset_popup_atoms() {
    *AT_MENTION_ACTIVE.state().write() = false;
    *SLASH_HINT_ACTIVE.state().write() = false;
    MENTION_PREFIX.state().write().clear();
    SLASH_PREFIX.state().write().clear();
}

fn reset_submit_side_effect_state() {
    crate::kit::atoms::init_atoms();
    *VIEW_MODELS.state().write() = ViewModelsSnapshot::default();
    INPUT_BUFFER.state().write().clear();
    crate::kit::atoms::INPUT_HISTORY.state().write().clear();
    crate::kit::atoms::INPUT_HISTORY_INDEX
        .state()
        .write()
        .take();
    crate::kit::atoms::OPEN_PANELS.state().write().clear();
    crate::kit::atoms::ACTIVE_PANEL.state().write().take();
    *crate::kit::atoms::NOTIFICATION.state().write() = None;
    ACP_STATE.state().write().is_loading = false;
}

fn make_submit_recorder() -> std::sync::Arc<parking_lot::Mutex<Vec<SubmitRequest>>> {
    std::sync::Arc::new(parking_lot::Mutex::new(Vec::new()))
}

fn recorded_submit(
    recorder: &std::sync::Arc<parking_lot::Mutex<Vec<SubmitRequest>>>,
) -> Option<SubmitRequest> {
    recorder.lock().pop()
}

#[test]
#[serial]
fn test_update_popup_prefix_slash_token_at_cursor() {
    crate::kit::atoms::init_atoms();
    reset_popup_atoms();
    let mut s = TextAreaState::default();
    s.insert_str("say /hel");
    update_popup_prefix(&s);
    assert!(!*AT_MENTION_ACTIVE.state().read());
    assert!(*SLASH_HINT_ACTIVE.state().read());
    assert_eq!(SLASH_PREFIX.state().read().as_str(), "hel");
}

#[test]
#[serial]
fn test_update_popup_prefix_slash_with_space_disables_after_token() {
    crate::kit::atoms::init_atoms();
    reset_popup_atoms();
    let mut s = TextAreaState::default();
    s.insert_str("say /hel o");
    update_popup_prefix(&s);
    assert!(!*SLASH_HINT_ACTIVE.state().read());
}

#[test]
#[serial]
fn test_update_popup_prefix_mention_trigger() {
    crate::kit::atoms::init_atoms();
    reset_popup_atoms();
    let mut s = TextAreaState::default();
    s.insert_str("see @auth");
    update_popup_prefix(&s);
    assert!(*AT_MENTION_ACTIVE.state().read());
    assert_eq!(MENTION_PREFIX.state().read().as_str(), "auth");
}

#[test]
#[serial]
fn test_update_popup_prefix_mention_with_space_disables() {
    crate::kit::atoms::init_atoms();
    reset_popup_atoms();
    let mut s = TextAreaState::default();
    s.insert_str("see @auth service");
    update_popup_prefix(&s);
    assert!(!*AT_MENTION_ACTIVE.state().read());
}

#[test]
#[serial]
fn test_submit_text_model_opens_panel_without_history_or_bubble() {
    reset_submit_side_effect_state();
    submit_text("/model".to_string());
    assert_eq!(
        *crate::kit::atoms::ACTIVE_PANEL.state().read(),
        Some(PanelKind::Model)
    );
    assert!(crate::kit::atoms::INPUT_HISTORY.state().read().is_empty());
    assert!(VIEW_MODELS.state().read().items.is_empty());
}

#[test]
#[serial]
fn test_submit_text_clear_sends_session_control_without_history_or_bubble() {
    reset_submit_side_effect_state();
    let recorder = make_submit_recorder();
    dispatch_submit_request(parse_submit_request("/clear").unwrap(), false, |request| {
        recorder.lock().push(request)
    });
    assert!(crate::kit::atoms::INPUT_HISTORY.state().read().is_empty());
    assert!(VIEW_MODELS.state().read().items.is_empty());
    assert_eq!(
        recorded_submit(&recorder),
        Some(SubmitRequest::SessionControl(
            crate::kit::submit_request::SessionControlRequest::Clear,
        ))
    );
}

#[test]
#[serial]
fn test_submit_text_provider_sends_view_action_without_history_or_bubble() {
    reset_submit_side_effect_state();
    let recorder = make_submit_recorder();
    dispatch_submit_request(
        parse_submit_request("/provider").unwrap(),
        false,
        |request| recorder.lock().push(request),
    );
    assert!(crate::kit::atoms::INPUT_HISTORY.state().read().is_empty());
    assert!(VIEW_MODELS.state().read().items.is_empty());
    assert_eq!(
        recorded_submit(&recorder),
        Some(SubmitRequest::ViewAction(
            crate::kit::submit_request::ViewActionRequest::CycleProvider,
        ))
    );
}

#[test]
#[serial]
fn test_submit_text_compact_appends_bubble_and_history_and_sends_agent_text() {
    reset_submit_side_effect_state();
    let recorder = make_submit_recorder();
    dispatch_submit_request(
        parse_submit_request("/compact").unwrap(),
        false,
        |request| recorder.lock().push(request),
    );
    assert_eq!(crate::kit::atoms::INPUT_HISTORY.state().read().len(), 1);
    // UserBubble 通过 LOCAL_EVENT_TX 异步发送，不在此断言
    assert_eq!(
        recorded_submit(&recorder),
        Some(SubmitRequest::AgentText("/compact".to_string()))
    );
}

#[test]
#[serial]
fn test_submit_text_unknown_slash_appends_bubble_and_history_and_sends_agent_text() {
    reset_submit_side_effect_state();
    let recorder = make_submit_recorder();
    dispatch_submit_request(parse_submit_request("/foo").unwrap(), false, |request| {
        recorder.lock().push(request)
    });
    assert_eq!(crate::kit::atoms::INPUT_HISTORY.state().read().len(), 1);
    assert_eq!(
        recorded_submit(&recorder),
        Some(SubmitRequest::AgentText("/foo".to_string()))
    );
}

#[test]
#[serial]
fn test_submit_text_loading_unknown_slash_buffers_agent_text() {
    reset_submit_side_effect_state();
    ACP_STATE.state().write().is_loading = true;
    submit_text("/foo".to_string());
    assert_eq!(crate::kit::atoms::INPUT_HISTORY.state().read().len(), 1);
    // Slice 3 D4（§10 queued 反转）：loading 提交只入队，**不**发本地气泡——
    // transcript 不得提前出现 user bubble（drain 后才恰一次）。
    assert_eq!(INPUT_BUFFER.state().read().len(), 1);
    assert!(
        VIEW_MODELS.state().read().items.is_empty(),
        "loading 提交不提前进 transcript（排队项显示在 composer 上方队列）"
    );
}

/// Slice 3 D4：排队上限 32 条——超出时队首被挤出（VecDeque FIFO 上限）。
#[test]
#[serial]
fn test_submit_text_loading_queue_caps_at_32() {
    reset_submit_side_effect_state();
    ACP_STATE.state().write().is_loading = true;
    for i in 0..33 {
        submit_text(format!("/c{i}"));
    }
    let state = INPUT_BUFFER.state();
    let buf = state.read();
    assert_eq!(buf.len(), 32, "排队上限 32 条");
    assert_eq!(buf.front().unwrap(), "/c1", "超出上限时队首（最旧）被挤出");
    assert_eq!(buf.back().unwrap(), "/c32");
}

/// Slice 3 D4：非 loading 提交路径不变——本地气泡 + AgentText 双发。
#[test]
#[serial]
fn test_submit_text_not_loading_sends_bubble_and_agent_text() {
    reset_submit_side_effect_state();
    let recorder = make_submit_recorder();
    dispatch_submit_request(parse_submit_request("/foo").unwrap(), false, |request| {
        recorder.lock().push(request)
    });
    assert!(INPUT_BUFFER.state().read().is_empty(), "非 loading 不入队");
    assert_eq!(
        recorded_submit(&recorder),
        Some(SubmitRequest::AgentText("/foo".to_string()))
    );
    // 本地气泡经 LOCAL_EVENT_TX 异步发送（send_local_user_bubble），
    // transcript 由 acp_bridge 异步写入——本测试不直接断言 VIEW_MODELS
    // （OnceLock 通道不可重置），由 acp_events_test 的 drain 测试覆盖。
}

/// Slice 3a：prompt 前缀宽度 = outer1 + accent1 + gap（§3.1 对齐）。
/// gap=1（Compact/Narrow）→ 3 列；gap=2（Wide/Standard）→ 4 列；
/// prompt_and_border_width 各加右预留 2 列。
#[test]
fn test_prompt_prefix_aligns_with_grid_content_start() {
    crate::kit::atoms::init_atoms();
    let narrow = GridSpec::grid_for(40); // Compact, gap=1
    let wide = GridSpec::grid_for(120); // Wide, gap=2
    assert_eq!(narrow.gap, 1);
    assert_eq!(wide.gap, 2);
    assert_eq!(prompt_and_border_width(narrow), 5);
    assert_eq!(prompt_and_border_width(wide), 6);

    // 正文起点 = 前缀宽度 = transcript first_prefix_width（outer+accent+gap）。
    let lines = build_composer_lines(vec![Line::from("hi".to_string())], false, narrow);
    let first = &lines[0].spans[0];
    assert_eq!(first.content, " ❯ ", "gap=1：前缀 3 列（1 空 + ❯ + 1 空）");
    assert_eq!(first.content.width(), narrow.first_prefix_width());

    let lines = build_composer_lines(vec![Line::from("hi".to_string())], false, wide);
    let first = &lines[0].spans[0];
    assert_eq!(first.content, " ❯  ", "gap=2：前缀 4 列（1 空 + ❯ + 2 空）");
    assert_eq!(first.content.width(), wide.first_prefix_width());

    // 续行前缀与首行同宽（accent 位置留空）。
    let multi = build_composer_lines(
        vec![Line::from("a".to_string()), Line::from("b".to_string())],
        false,
        wide,
    );
    assert_eq!(multi[1].spans[0].content.width(), wide.first_prefix_width());
}

/// Slice 3b：composer 上方 queued 队列行——`· {text}` 截断 + `· · ·` 溢出行。
/// 组件层先 take(QUEUE_VISIBLE_MAX=5) 再渲染：5 条 + 溢出标记 = 6 行。
#[test]
fn test_build_queue_lines_caps_at_five_and_more_row() {
    crate::kit::atoms::init_atoms();
    let items: Vec<String> = (0..5).map(|i| format!("prompt {i}")).collect();
    let lines = build_queue_lines(&items, true, 40);
    assert_eq!(lines.len(), 6, "5 条 + 1 行溢出标记");
    assert!(lines[0].spans[0].content.starts_with('·'));
    assert!(
        lines[5].spans[0].content.contains("· · ·"),
        "溢出行 `· · ·`"
    );
    // 无溢出标记时只有 items 行。
    assert_eq!(build_queue_lines(&items, false, 40).len(), 5);
    // 空队列 → 无行（不占高度）。
    assert!(build_queue_lines(&[], false, 40).is_empty());
    // 超长文本按宽度截断（truncate_by_width：正文预算 max_width + 省略号 1 列）。
    let long = build_queue_lines(&["x".repeat(100)], false, 10);
    assert_eq!(long[0].spans[1].content.width(), 11);
}

/// 渲染 Block 到 TestBackend，返回按行拼接的文本（titles 是私有字段，
/// 经真实渲染验证标题行内容与降级组合）。
fn render_block_text(block: &Block<'static>, w: u16, h: u16) -> Vec<String> {
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h))
        .expect("TestBackend 可创建");
    terminal
        .draw(|f| {
            f.render_widget(block.clone(), f.area());
        })
        .expect("渲染成功");
    let buf = terminal.backend().buffer();
    (0..h)
        .map(|y| {
            (0..w)
                .map(|x| buf[(x, y)].symbol().to_string())
                .collect::<String>()
        })
        .collect()
}

/// Slice 3a：composer 标题/footer 行（§10）——show_top/show_bottom 组合。
#[test]
fn test_build_composer_block_titles_and_degrades() {
    crate::kit::atoms::init_atoms();
    i18n::init(None);
    // 全显示：top 仅保留 session title；footer 左 files 右资源线（CPU·MEM·ctx）。
    let full = build_composer_block(
        false,
        "session",
        Some("@ 2 files"),
        Some(Line::from(" CPU 75% · MEM 512MB · 42% ctx ")),
        true,
        true,
        80,
    );
    let rows = render_block_text(&full, 80, 4);
    assert!(
        !rows[0].contains("Auto Mode") && !rows[0].contains("gpt-5"),
        "title_top 不应重复显示状态栏已有的 mode/model：{:?}",
        rows[0]
    );
    assert!(
        rows[0].contains("session"),
        "title_top 右侧保留 session title"
    );
    assert!(
        rows[3].contains("@ 2 files"),
        "title_bottom 左侧附件计数：{:?}",
        rows[3]
    );
    assert!(
        rows[3].contains("MEM 512MB") && rows[3].contains("42% ctx"),
        "title_bottom 右侧资源线（MEM · ctx）：{:?}",
        rows[3]
    );

    // h<12：隐藏 session title。
    let no_top = build_composer_block(
        false,
        "session",
        Some("f"),
        Some(Line::from(" c ")),
        false,
        true,
        80,
    );
    let rows = render_block_text(&no_top, 80, 4);
    assert!(
        !rows[0].contains("session") && !rows[0].contains("·"),
        "h<12 隐藏 title_top 整行：{:?}",
        rows[0]
    );

    // h<8：title_bottom 也隐藏。
    let all_hidden = build_composer_block(
        false,
        "session",
        Some("f"),
        Some(Line::from(" c ")),
        false,
        false,
        80,
    );
    let rows = render_block_text(&all_hidden, 80, 4);
    assert!(
        !rows[0].contains("session") && !rows[3].contains("files"),
        "h<8 全部标题行隐藏"
    );
}

#[test]
#[serial]
fn test_submit_text_loading_clear_shows_notification_without_history_or_buffer() {
    reset_submit_side_effect_state();
    ACP_STATE.state().write().is_loading = true;
    submit_text("/clear".to_string());
    assert!(crate::kit::atoms::INPUT_HISTORY.state().read().is_empty());
    assert!(VIEW_MODELS.state().read().items.is_empty());
    assert!(INPUT_BUFFER.state().read().is_empty());
    assert!(crate::kit::atoms::NOTIFICATION.state().read().is_some());
}
#[test]
#[serial]
fn test_filter_files_empty_prefix_returns_top_20() {
    crate::kit::atoms::init_atoms();
    // 写 25 个文件
    {
        let state = FILE_LIST.state();
        let mut list = state.write();
        *list = (0..25).map(|i| format!("file{i}.rs")).collect();
        list.sort();
    }
    let result = filter_files_for_mention("");
    assert_eq!(result.len(), 20);
}

/// C2 回归测试：filter_files_for_mention 按大小写不敏感子串过滤。
#[test]
#[serial]
fn test_filter_files_substring_case_insensitive() {
    crate::kit::atoms::init_atoms();
    *FILE_LIST.state().write() = vec![
        "auth.rs".into(),
        "oauth.rs".into(),
        "OAUTH.md".into(),
        "utils.rs".into(),
    ];
    let result = filter_files_for_mention("AUTH");
    // 三个含 auth/AUTH 的文件应被过滤出来（大小写不敏感）
    assert_eq!(result.len(), 3);
    assert!(result.contains(&"auth.rs".to_string()));
    assert!(result.contains(&"oauth.rs".to_string()));
    assert!(result.contains(&"OAUTH.md".to_string()));
}

/// C2 回归测试：prefix 开头的文件优先于子串匹配的。
#[test]
#[serial]
fn test_filter_files_prefix_start_priority() {
    crate::kit::atoms::init_atoms();
    *FILE_LIST.state().write() = vec![
        "myauth.rs".into(), // 子串匹配
        "auth.rs".into(),   // 开头匹配，应优先
        "oauth.rs".into(),  // 子串匹配
    ];
    let result = filter_files_for_mention("auth");
    assert_eq!(result.first().unwrap(), "auth.rs");
}

/// M5：`exit_history_mode_if_active` 在 `INPUT_HISTORY_INDEX` 为 Some 时调用
/// `reset_history_cursor`，清空 index 与 DRAFT。为 None 时为 no-op。
#[test]
#[serial]
fn test_exit_history_mode_helper_resets_index_and_keeps_draft_unused() {
    use crate::kit::atoms::DRAFT as HISTORY_DRAFT;
    use crate::kit::atoms::INPUT_HISTORY_INDEX;
    crate::kit::atoms::init_atoms();
    // 先推入一条历史并进入 history 浏览模式（history_up 会保存 DRAFT）。
    crate::kit::input_history::push_history("a");
    let _ = crate::kit::input_history::history_up(Some("orig"));
    assert!(INPUT_HISTORY_INDEX.state().read().is_some());
    assert!(HISTORY_DRAFT.state().read().is_some());

    exit_history_mode_if_active();
    // helper 应清空 index + DRAFT，回到"编辑新文本"状态。
    assert!(INPUT_HISTORY_INDEX.state().read().is_none());
    assert!(HISTORY_DRAFT.state().read().is_none());

    // 非历史模式调用应为 no-op，不 panic。
    exit_history_mode_if_active();
    assert!(INPUT_HISTORY_INDEX.state().read().is_none());
}

/// L13：粘贴分支应清空 slash/mention 激活态而非重新检测。
///
/// 构造 mention 激活（`see @auth`），随后调用 reset_mention_popup + reset_slash_popup
/// （与粘贴分支等价的清理路径），断言 AT_MENTION_ACTIVE / SLASH_HINT_ACTIVE 均为 false。
#[test]
#[serial]
fn test_paste_does_not_trigger_slash_or_mention_popup() {
    crate::kit::atoms::init_atoms();
    reset_popup_atoms();
    let mut s = TextAreaState::default();
    s.insert_str("see @auth");
    update_popup_prefix(&s);
    // 触发了 mention 弹窗。
    assert!(*AT_MENTION_ACTIVE.state().read());

    // 模拟粘贴分支：先 reset，而不是 update_popup_prefix。
    reset_mention_popup();
    reset_slash_popup();
    assert!(!*AT_MENTION_ACTIVE.state().read());
    assert!(!*SLASH_HINT_ACTIVE.state().read());
}

// ── 会话标题标签 ─────────────────────────────────────────────────────────────

#[test]
fn test_stable_hash_is_deterministic() {
    // 同一标题跨调用 hash 稳定（跨进程稳定的前提）
    assert_eq!(stable_hash("修复登录"), stable_hash("修复登录"));
    // 不同标题大概率不同色（hash 不同）
    assert_ne!(stable_hash("修复登录"), stable_hash("重构状态机"));
    // 与直接实现比对，锁定算法不漂移（FNV-1a 64）
    assert_eq!(stable_hash("hello"), 0xa430_d846_80aa_bd0b_u64);
}

#[test]
fn test_truncate_title_to_width_handles_cjk() {
    // 32 个半角字符预算
    let s = "a".repeat(40);
    let t = truncate_title_to_width(&s, 32);
    assert!(t.ends_with('…'));
    assert_eq!(t.chars().count(), 33); // 32 + 省略号

    // CJK 双宽字符：16 个汉字 = 32 列，不截断
    let cjk = "字".repeat(16);
    assert_eq!(truncate_title_to_width(&cjk, 32), cjk);

    // 17 个汉字 = 34 列 → 截断到 32 列（16 字 + 省略号）
    let t = truncate_title_to_width(&"字".repeat(17), 32);
    assert!(t.ends_with('…'));
}

#[test]
fn test_readable_fg_contrast() {
    use ratatui::style::Color;
    // 深底 → 白字；浅底 → 黑字
    assert_eq!(readable_fg(Color::Rgb(18, 52, 26)), Color::White);
    assert_eq!(readable_fg(Color::Rgb(240, 215, 205)), Color::Black);
    // 非 RGB（如 Reset）fallback 白字
    assert_eq!(readable_fg(Color::Reset), Color::White);
}

#[test]
fn test_build_session_title_line_has_palette_bg() {
    crate::kit::atoms::init_atoms();
    let line = build_session_title_line("修复登录");
    let span = &line.spans[0];
    let style = span.style;
    // 底色来自主题 palette（非 Reset），前景与底色对比
    assert_ne!(style.bg, Some(ratatui::style::Color::Reset));
    assert_ne!(style.fg, Some(ratatui::style::Color::Reset));
    assert!(style.add_modifier.contains(ratatui::style::Modifier::BOLD));
    // 文本带两侧空格（标签内边距）
    assert!(span.content.starts_with(' '));
    assert!(span.content.ends_with(' '));
}

// ── 焦点回退：输入内容变化 → 清除消息区 entry 导航焦点 ──────────────────────

/// 点击 chat entry 展开后直接键入：清除 FOCUSED_ENTRY，Enter 才能回到提交语义。
#[test]
#[serial]
fn test_exit_entry_focus_on_edit_clears_focused_entry() {
    crate::kit::atoms::init_atoms();
    *crate::kit::atoms::FOCUSED_ENTRY.state().write() =
        Some(crate::kit::atoms::FocusedEntry { slot: 0, key: None });
    exit_entry_focus_on_edit();
    assert!(
        crate::kit::atoms::FOCUSED_ENTRY.state().read().is_none(),
        "输入内容变化后 entry 导航焦点必须清除（焦点回到输入态）"
    );
}

/// 无 entry 焦点时零副作用（保持 None，不产生无谓 wake 写入）。
#[test]
#[serial]
fn test_exit_entry_focus_on_edit_noop_when_no_focus() {
    crate::kit::atoms::init_atoms();
    *crate::kit::atoms::FOCUSED_ENTRY.state().write() = None;
    exit_entry_focus_on_edit();
    assert!(crate::kit::atoms::FOCUSED_ENTRY.state().read().is_none());
}

/// Phase 4 步骤 3：build_slash_items 纯投影映射——kind/level 直接来自结构化
/// 投影（无 SKILL_NAMES / MCP_SKILL_NAMES 反推）；label 经 display_name 按
/// level 变换（1 裸名 / 2 全名），insert_text == label（display 即 lexical）。
#[test]
#[serial]
fn test_build_slash_items_uses_projection_kind_level() {
    crate::kit::atoms::init_atoms();
    *AVAILABLE_SLASH_COMMANDS.state().write() = vec![
        SlashCommandEntry {
            fullname: "mcp:demo:hello".to_string(),
            description: "MCP skill".to_string(),
            kind: SlashActionKind::McpSkill,
            level: 2,
            ..Default::default()
        },
        SlashCommandEntry {
            fullname: "MySkill".to_string(),
            description: "本地 skill".to_string(),
            kind: SlashActionKind::Skill,
            level: 1,
            ..Default::default()
        },
        SlashCommandEntry {
            fullname: "core:compact".to_string(),
            description: "Compact".to_string(),
            kind: SlashActionKind::Command,
            level: 1,
            ..Default::default()
        },
    ];

    let items = build_slash_items();
    let find = |label: &str| {
        items
            .iter()
            .find(|i| i.label == label)
            .unwrap_or_else(|| panic!("未找到 slash 条目 {label}"))
    };
    // level 2 → 全名原样
    let mcp = find("mcp:demo:hello");
    assert_eq!(mcp.kind, SlashActionKind::McpSkill);
    assert_eq!(mcp.insert_text, "mcp:demo:hello");
    // 无冒号全名 → 原样（裸名即全名）
    let skill = find("MySkill");
    assert_eq!(skill.kind, SlashActionKind::Skill);
    assert_eq!(skill.insert_text, "MySkill");
    // level 1 → 最右冒号后段裸名；fullname 保留全名元数据
    let compact = find("compact");
    assert_eq!(compact.kind, SlashActionKind::Command);
    assert_eq!(compact.insert_text, "compact");
    assert_eq!(compact.fullname, "core:compact");
    // 双索引（步骤 4）：search_lowercase = label + fullname 小写合并——
    // level 1 裸名条目也能被全名前缀（如 /mcp:demo）模糊搜到
    assert_eq!(mcp.search_lowercase, "mcp:demo:hello mcp:demo:hello");
    assert_eq!(compact.search_lowercase, "compact core:compact");
    // 全名形态不得再出现（display 即 lexical，解析器严格命中）
    assert!(items.iter().all(|i| i.label != "core:compact"));
    // 步骤 6 收口：条目全部来自投影——无 PANELS/setup 本地合成
    assert_eq!(items.len(), 3, "不得存在投影之外的本地合成条目");
}

/// Phase 4 步骤 6：纯投影收口——不预置投影则无任何条目（PANELS 合成与
/// /setup 硬编码已删除，history/setup 不再凭空出现）；预置投影时
/// label == insert_text（display 即 lexical）。
#[test]
#[serial]
fn test_build_slash_items_display_is_lexical() {
    crate::kit::atoms::init_atoms();
    // 显式清空投影——init_atoms 为空操作，不重置 atom；并行测试（如
    // acp_notifier 投影解析测试）可能已写入 AVAILABLE_SLASH_COMMANDS
    *AVAILABLE_SLASH_COMMANDS.state().write() = Vec::new();
    // 步骤 6：不预置投影 → 空列表（无本地合成兜底）
    let items = build_slash_items();
    assert!(
        items.is_empty(),
        "纯投影收口后不预置投影不得有任何条目（PANELS 合成与 /setup 硬编码已删除）"
    );
    assert!(
        items
            .iter()
            .all(|i| i.label != "history" && i.label != "setup"),
        "history/setup 条目只能来自投影，不得本地合成"
    );

    *AVAILABLE_SLASH_COMMANDS.state().write() = vec![
        SlashCommandEntry {
            fullname: "core:compact".to_string(),
            description: "Compact".to_string(),
            level: 1,
            ..Default::default()
        },
        SlashCommandEntry {
            fullname: "mcp:demo:hello".to_string(),
            description: "MCP skill".to_string(),
            level: 2,
            ..Default::default()
        },
    ];

    let items = build_slash_items();
    assert!(!items.is_empty(), "预置投影后应有条目");
    for item in &items {
        assert_eq!(
            item.label, item.insert_text,
            "label 必须等于 insert_text（display 即 lexical）"
        );
    }
}

/// 投影条目 aliases 必须生成独立补全条目：门控反转后旧 UI_COMMANDS 裸名
/// 广播（`history`）消失，别名只经 `_meta.periAliases` 挂载（ui:threads →
/// history/his/resume；core:clear → cls/reset）。若 build_slash_items 不消费
/// aliases，这些别名补全条目会随之丢失。
#[test]
#[serial]
fn test_build_slash_items_projection_aliases() {
    crate::kit::atoms::init_atoms();
    *AVAILABLE_SLASH_COMMANDS.state().write() = vec![
        SlashCommandEntry {
            fullname: "ui:threads".to_string(),
            description: "Thread browser".to_string(),
            kind: SlashActionKind::Panel,
            aliases: vec!["history".into(), "his".into(), "resume".into()],
            level: 1,
            ..Default::default()
        },
        SlashCommandEntry {
            fullname: "core:clear".to_string(),
            description: "Clear".to_string(),
            kind: SlashActionKind::Command,
            aliases: vec!["cls".into(), "reset".into()],
            level: 1,
            ..Default::default()
        },
    ];

    let items = build_slash_items();
    let find = |label: &str| {
        items
            .iter()
            .find(|i| i.label == label)
            .unwrap_or_else(|| panic!("未找到 slash 条目 {label}"))
    };
    // 主条目正常生成
    let threads = find("threads");
    assert_eq!(threads.fullname, "ui:threads");
    assert_eq!(threads.kind, SlashActionKind::Panel);
    // ui 域别名条目：继承主条目元数据，display 即 lexical
    for alias in ["history", "his", "resume"] {
        let item = find(alias);
        assert_eq!(item.insert_text, alias);
        assert_eq!(item.fullname, "ui:threads", "alias 条目归属主条目");
        assert_eq!(item.kind, SlashActionKind::Panel);
        assert_eq!(item.description, "Thread browser");
    }
    // core 域别名条目同样生成（提交时经 ACP 注册表 alias 索引解析）
    find("clear");
    for alias in ["cls", "reset"] {
        let item = find(alias);
        assert_eq!(item.fullname, "core:clear");
        assert_eq!(item.kind, SlashActionKind::Command);
    }
    // alias 可被补全过滤命中（search_lowercase 预计算含 alias）
    assert!(
        items
            .iter()
            .any(|i| i.search_lowercase.starts_with("history ")),
        "alias 必须进入 search_lowercase 双索引"
    );
}

/// Phase 4 步骤 6：双写窗口去重（R2 防御）——同 display 名多条时优先保留
/// 「携带 kind 元数据（非缺省回退）的 ui 域条目」：服务端旧 UI_COMMANDS
/// 裸名广播（`history`，无 _meta → kind 缺省回退 Command）与 TUI 上送注册
/// （`ui:history` 全名 + periKind=panel）并存时，后者胜出。
#[test]
#[serial]
fn test_build_slash_items_dedup_prefers_ui_kind_meta() {
    crate::kit::atoms::init_atoms();
    *AVAILABLE_SLASH_COMMANDS.state().write() = vec![
        // 服务端裸名广播（缺 _meta → 缺省回退形态 kind=Command / level=1）
        SlashCommandEntry {
            fullname: "history".to_string(),
            description: "legacy broadcast".to_string(),
            ..Default::default()
        },
        // TUI 上送注册（ui 域全名 + 显式 kind 元数据）
        SlashCommandEntry {
            fullname: "ui:history".to_string(),
            description: "Thread browser".to_string(),
            kind: SlashActionKind::Panel,
            level: 1,
            ..Default::default()
        },
    ];

    let items = build_slash_items();
    let history: Vec<&SlashCompletionItem> =
        items.iter().filter(|i| i.label == "history").collect();
    assert_eq!(
        history.len(),
        1,
        "同 display 名必须去重为一条（双写窗口防御）"
    );
    assert_eq!(
        history[0].kind,
        SlashActionKind::Panel,
        "优先保留携带 kind 元数据（非缺省回退）的 ui 域条目"
    );
    assert_eq!(history[0].fullname, "ui:history");
    assert_eq!(history[0].insert_text, "history");
}

// ── Phase 4 步骤 4：on_select 选中行为收敛（resolve_ui_command 统一拦截） ──

/// 构造测试条目：label 为显示形态，fullname 为唯一键。
fn slash_item(label: &str, fullname: &str) -> SlashCompletionItem {
    SlashCompletionItem {
        label: label.to_string(),
        insert_text: label.to_string(),
        description: String::new(),
        kind: SlashActionKind::Command,
        label_lowercase: label.to_lowercase(),
        fullname: fullname.to_string(),
        search_lowercase: SlashCompletionItem::make_search_lowercase(
            &label.to_lowercase(),
            fullname,
        ),
        args: None,
    }
}

/// 选中裸名 history（ui 域别名）→ resolve_ui_command 命中 → 清空输入框 +
/// open_panel（ThreadBrowser），不再回退 apply_slash_selection。
#[test]
#[serial]
fn test_slash_on_select_history_alias_opens_thread_browser() {
    crate::kit::atoms::init_atoms();
    reset_submit_side_effect_state();
    let mut s = TextAreaState::default();
    s.insert_str("/hist");
    s.cursor = 5;
    let item = slash_item("history", "ui:history");
    handle_slash_selection(&mut s, &item);
    assert!(s.text.is_empty(), "命中 ui 域命令后输入框必须清空");
    assert_eq!(*ACTIVE_PANEL.state().read(), Some(PanelKind::ThreadBrowser));
}

/// 选中 `ui:` 前缀显式形态（ui:model）→ resolve_ui_command 归一化命中 →
/// Model 面板。
#[test]
#[serial]
fn test_slash_on_select_ui_prefix_opens_model_panel() {
    crate::kit::atoms::init_atoms();
    reset_submit_side_effect_state();
    let mut s = TextAreaState::default();
    s.insert_str("/ui:mo");
    s.cursor = 6;
    let item = slash_item("model", "ui:model");
    handle_slash_selection(&mut s, &item);
    assert!(s.text.is_empty(), "命中 ui 域命令后输入框必须清空");
    assert_eq!(*ACTIVE_PANEL.state().read(), Some(PanelKind::Model));
}

/// 选中 setup（ui 域）→ 本地激活 Setup Wizard（不发 ACP）。
#[test]
#[serial]
fn test_slash_on_select_setup_activates_wizard() {
    crate::kit::atoms::init_atoms();
    reset_submit_side_effect_state();
    *WIZARD_ACTIVE.state().write() = false;
    let mut s = TextAreaState::default();
    s.insert_str("/set");
    s.cursor = 4;
    let item = slash_item("setup", "ui:setup");
    handle_slash_selection(&mut s, &item);
    assert!(s.text.is_empty(), "命中 ui 域命令后输入框必须清空");
    assert!(
        *WIZARD_ACTIVE.state().read(),
        "setup 选中后必须激活 Wizard（本地拦截，不发 ACP）"
    );
}

/// 未命中 ui 域（core:compact 全名形态，TUI 只拦截 ui 域）→
/// apply_slash_selection 落输入框（display 即 lexical）。
#[test]
#[serial]
fn test_slash_on_select_non_ui_command_applies_selection() {
    crate::kit::atoms::init_atoms();
    reset_submit_side_effect_state();
    *WIZARD_ACTIVE.state().write() = false;
    let mut s = TextAreaState::default();
    s.insert_str("/com");
    s.cursor = 4;
    let item = slash_item("compact", "core:compact");
    handle_slash_selection(&mut s, &item);
    assert_eq!(
        s.text, "/compact ",
        "未命中 ui 域应落输入框（display 即 lexical）"
    );
    assert_eq!(*ACTIVE_PANEL.state().read(), None);
    assert!(!*WIZARD_ACTIVE.state().read());
}
