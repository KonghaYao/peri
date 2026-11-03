//! `MessageSource` 的 wire 字面量：新增来源不得改变旧行的可读性。
//!
//! MQ 行的 `source` 以 serde 的蛇形字面量落库；旧版本写下的行必须继续反序列化，
//! 新来源只做追加。

use super::MessageSource;

#[test]
fn session_recovery_round_trips_as_a_snake_case_literal() {
    let encoded = serde_json::to_string(&MessageSource::SessionRecovery).unwrap();
    assert_eq!(encoded, "\"session_recovery\"");
    let decoded: MessageSource = serde_json::from_str(&encoded).unwrap();
    assert_eq!(decoded, MessageSource::SessionRecovery);
}

#[test]
fn existing_sources_keep_their_stored_literals() {
    for (source, literal) in [
        (MessageSource::UserInput, "\"user_input\""),
        (MessageSource::SubAgentComplete, "\"sub_agent_complete\""),
        (MessageSource::ShellComplete, "\"shell_complete\""),
        (MessageSource::GoalSteering, "\"goal_steering\""),
        (MessageSource::CronTrigger, "\"cron_trigger\""),
        (MessageSource::WorkflowComplete, "\"workflow_complete\""),
        (MessageSource::SystemInjected, "\"system_injected\""),
    ] {
        assert_eq!(serde_json::to_string(&source).unwrap(), literal);
        let decoded: MessageSource = serde_json::from_str(literal).unwrap();
        assert_eq!(decoded, source);
    }
}

#[test]
fn legacy_mcp_notification_alias_still_decodes() {
    let decoded: MessageSource = serde_json::from_str("\"dynamic_mcp_notification\"").unwrap();
    assert_eq!(decoded, MessageSource::McpNotification);
}
