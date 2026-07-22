//! Tests for input_area

use super::*;
use crate::app::panel_types::PanelKind;
use crate::kit::atoms::{VIEW_MODELS, ViewModelsSnapshot};
use serial_test::serial;

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
    // UserBubble 通过 LOCAL_EVENT_TX 异步发送；assert INPUT_BUFFER 接收了文本
    assert_eq!(INPUT_BUFFER.state().read().len(), 1);
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
