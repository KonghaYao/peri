//! Tests for submit_request

#[cfg(test)]
use super::*;

#[test]
fn test_parse_submit_request_returns_none_for_empty_input() {
    assert_eq!(parse_submit_request("   \n\t "), None);
}

#[test]
fn test_parse_submit_request_treats_plain_text_as_agent_text() {
    assert_eq!(
        parse_submit_request("hello world"),
        Some(SubmitRequest::AgentText("hello world".to_string()))
    );
}

#[test]
fn test_parse_submit_request_treats_compact_as_agent_text() {
    assert_eq!(
        parse_submit_request("/compact now"),
        Some(SubmitRequest::AgentText("/compact now".to_string()))
    );
}

#[test]
fn test_parse_submit_request_treats_unknown_slash_as_agent_text() {
    assert_eq!(
        parse_submit_request("/unknown"),
        Some(SubmitRequest::AgentText("/unknown".to_string()))
    );
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
fn test_parse_submit_request_matches_clear_aliases() {
    assert_eq!(
        parse_submit_request("/clear"),
        Some(SubmitRequest::SessionControl(SessionControlRequest::Clear))
    );
    assert_eq!(
        parse_submit_request("/cls"),
        Some(SubmitRequest::SessionControl(SessionControlRequest::Clear))
    );
    assert_eq!(
        parse_submit_request("/reset"),
        Some(SubmitRequest::SessionControl(SessionControlRequest::Clear))
    );
}

#[test]
fn test_parse_submit_request_matches_setup() {
    assert_eq!(
        parse_submit_request("/setup"),
        Some(SubmitRequest::SessionControl(
            SessionControlRequest::ToggleSetup
        ))
    );
    // /setup 后跟参数也识别
    assert_eq!(
        parse_submit_request("/setup my-custom-arg"),
        Some(SubmitRequest::SessionControl(
            SessionControlRequest::ToggleSetup
        ))
    );
}

#[test]
fn test_parse_submit_request_matches_rewind_aliases() {
    assert_eq!(
        parse_submit_request("/rewind abc --revert-files"),
        Some(SubmitRequest::SessionControl(
            SessionControlRequest::Rewind(RewindRequest {
                target_message_id: "abc".to_string(),
                revert_files: true,
            },)
        ))
    );
    assert_eq!(
        parse_submit_request("/undo xyz"),
        Some(SubmitRequest::SessionControl(
            SessionControlRequest::Rewind(RewindRequest {
                target_message_id: "xyz".to_string(),
                revert_files: false,
            },)
        ))
    );
}

#[test]
fn test_parse_submit_request_matches_view_actions() {
    assert_eq!(
        parse_submit_request("/provider"),
        Some(SubmitRequest::ViewAction(ViewActionRequest::CycleProvider))
    );
    assert_eq!(
        parse_submit_request("/mode"),
        Some(SubmitRequest::ViewAction(
            ViewActionRequest::CyclePermissionMode
        ))
    );
    assert_eq!(
        parse_submit_request("/debug-export-text"),
        Some(SubmitRequest::ViewAction(ViewActionRequest::ExportText(
            ExportMode::All,
        )))
    );
    assert_eq!(
        parse_submit_request("/debug-export-text screen"),
        Some(SubmitRequest::ViewAction(ViewActionRequest::ExportText(
            ExportMode::Screen,
        )))
    );
    assert_eq!(
        parse_submit_request("/exit"),
        Some(SubmitRequest::ViewAction(ViewActionRequest::Exit))
    );
    assert_eq!(
        parse_submit_request("/quit"),
        Some(SubmitRequest::ViewAction(ViewActionRequest::Exit))
    );
}
