use crate::app::panel_types::PanelKind;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubmitRequest {
    AgentText(String),
    OpenPanel(PanelKind),
    SessionControl(SessionControlRequest),
    ViewAction(ViewActionRequest),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionControlRequest {
    Clear,
    Rewind(RewindRequest),
    ToggleSetup,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RewindRequest {
    pub target_message_id: String,
    pub revert_files: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewActionRequest {
    CycleProvider,
    CyclePermissionMode,
    ExportText(ExportMode),
    /// /exit 命令：退出程序。
    Exit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportMode {
    All,
    Screen,
}

/// 统一 slash submit parser。
///
/// 优先级固定为：session control / view action / panel / agent text。
/// 这样可以避免未来本地 panel alias 意外抢占控制命令，同时保持本地 UI slash
/// 在 Enter 提交时优先于远端 ACP command/skill。未命中的 slash 一律按 AgentText
/// 处理，不报本地 command 错误。
pub fn parse_submit_request(input: &str) -> Option<SubmitRequest> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }

    let command = trimmed.split_whitespace().next().unwrap_or("");

    if is_clear_command(command) {
        return Some(SubmitRequest::SessionControl(SessionControlRequest::Clear));
    }

    if is_setup_command(command) {
        return Some(SubmitRequest::SessionControl(
            SessionControlRequest::ToggleSetup,
        ));
    }

    if is_rewind_or_undo_command(command) {
        return Some(SubmitRequest::SessionControl(
            SessionControlRequest::Rewind(parse_rewind_args(trimmed)),
        ));
    }

    if let Some(action) = parse_view_action(command, trimmed) {
        return Some(SubmitRequest::ViewAction(action));
    }

    if command.starts_with('/')
        && let Some(kind) = crate::kit::panel_registry::panel_for_slash_command(command)
    {
        return Some(SubmitRequest::OpenPanel(kind));
    }

    Some(SubmitRequest::AgentText(trimmed.to_string()))
}

fn is_clear_command(command: &str) -> bool {
    matches!(command, "/clear" | "/cls" | "/reset")
}

fn is_rewind_or_undo_command(command: &str) -> bool {
    matches!(command, "/rewind" | "/undo")
}

fn is_setup_command(command: &str) -> bool {
    matches!(command, "/setup")
}

fn parse_view_action(command: &str, input: &str) -> Option<ViewActionRequest> {
    match command {
        "/provider" => Some(ViewActionRequest::CycleProvider),
        "/mode" => Some(ViewActionRequest::CyclePermissionMode),
        "/debug-export-text" => Some(ViewActionRequest::ExportText(parse_export_mode(input))),
        "/exit" | "/quit" => Some(ViewActionRequest::Exit),
        _ => None,
    }
}

fn parse_rewind_args(input: &str) -> RewindRequest {
    let parts: Vec<&str> = input.split_whitespace().collect();
    let target_message_id = parts.get(1).map(|s| s.to_string()).unwrap_or_default();
    let revert_files = parts.contains(&"--revert-files");
    RewindRequest {
        target_message_id,
        revert_files,
    }
}

fn parse_export_mode(input: &str) -> ExportMode {
    match input.split_whitespace().nth(1) {
        Some("screen") => ExportMode::Screen,
        _ => ExportMode::All,
    }
}

#[cfg(test)]
#[path = "submit_request_test.rs"]
mod tests;
