use serde_json::json;

use super::{
    declared, escalation_servers, requires_answer, ApprovalDecision, ApprovalPolicy, RequiredAction,
};
use crate::domain::McpToolSpec;

fn tool(label: &str, require_approval: serde_json::Value) -> McpToolSpec {
    let mut spec = McpToolSpec::new(label, "https://mcp.example.test/mcp");
    spec.require_approval = Some(require_approval);
    spec
}

#[test]
fn string_forms_cover_every_tool_and_unknown_shapes_fail_closed() {
    assert!(!ApprovalPolicy::parse(None).escalates());
    assert_eq!(
        ApprovalPolicy::parse(Some(&json!("never"))),
        ApprovalPolicy::Never
    );
    assert_eq!(
        ApprovalPolicy::parse(Some(&json!("always"))),
        ApprovalPolicy::Always
    );
    assert!(ApprovalPolicy::parse(Some(&json!(true))).requires_approval("anything"));
    assert!(ApprovalPolicy::parse(Some(&json!({ "weird": 1 }))).requires_approval("x"));
}

#[test]
fn object_lists_win_over_the_default_and_unnamed_tools_wait() {
    let policy = ApprovalPolicy::parse(Some(&json!({
        "always": { "tool_names": ["write"] },
        "never": { "tool_names": ["read"] },
        "default": "never",
    })));
    assert!(policy.requires_approval("write"));
    assert!(!policy.requires_approval("read"));
    // `default: never` is a declared decision, not a failed match.
    assert!(!policy.requires_approval("search"));

    let no_default = ApprovalPolicy::parse(Some(&json!({
        "always": { "tool_names": ["write"] },
    })));
    assert!(no_default.requires_approval("write"));
    // Unknown tool, no default: fail-closed.
    assert!(no_default.requires_approval("search"));
}

#[test]
fn only_declaring_servers_are_escalated_so_a_same_named_tool_is_not_widened() {
    let tools = vec![
        tool("approved", json!("always")),
        tool("not-approved", json!("never")),
    ];
    let servers = escalation_servers(&tools);
    assert_eq!(servers, ["approved".to_owned()].into_iter().collect());
    let declarations = declared(&tools);
    assert_eq!(declarations.len(), 1);
    assert!(requires_answer(&declarations, "approved", "search"));
    // An undeclared server can only reach this path if something else
    // escalated it; it must wait rather than be allowed by name.
    assert!(requires_answer(&declarations, "not-approved", "search"));
}

#[test]
fn a_never_bound_tool_does_not_wait_and_an_unknown_decision_is_rejected() {
    let declarations = declared(&[tool(
        "approved",
        json!({ "never": { "tool_names": ["lookup"] } }),
    )]);
    assert!(!requires_answer(&declarations, "approved", "lookup"));
    assert!(requires_answer(&declarations, "approved", "other"));
    assert_eq!(
        ApprovalDecision::parse("approve"),
        Some(ApprovalDecision::Approve)
    );
    assert_eq!(
        ApprovalDecision::parse("deny"),
        Some(ApprovalDecision::Deny)
    );
    assert_eq!(
        ApprovalDecision::parse("cancel"),
        Some(ApprovalDecision::Cancel)
    );
    assert_eq!(ApprovalDecision::parse("maybe"), None);
}

#[test]
fn acp_responses_keep_deny_and_cancel_distinguishable() {
    assert_eq!(
        ApprovalDecision::Approve.acp_response(),
        json!({ "outcome": { "outcome": "selected", "optionId": "allow_once" } })
    );
    assert_eq!(
        ApprovalDecision::Deny.acp_response(),
        json!({ "outcome": { "outcome": "selected", "optionId": "reject_once" } })
    );
    // `cancel` is not a user rejection: the host receives its own outcome.
    assert_eq!(
        ApprovalDecision::Cancel.acp_response(),
        json!({ "outcome": { "outcome": "cancelled" } })
    );
}

#[test]
fn a_pending_action_matches_the_official_nested_shape_and_keeps_null_fields() {
    let complete = RequiredAction {
        request_id: "appr_1".to_owned(),
        turn_id: "turn_1".to_owned(),
        server_label: "server-a".to_owned(),
        tool: Some("search".to_owned()),
        arguments: json!({ "query": "x" }),
    };
    assert_eq!(
        complete.to_json(),
        json!({
            "type": "mcp_approval_request",
            "request_id": "appr_1",
            "turn_id": "turn_1",
            "request": {
                "type": "mcp_tool_call",
                "server_label": "server-a",
                "name": "search",
                "arguments": { "query": "x" },
                "message": "MCP tool `search` on server `server-a` requires approval",
            },
        })
    );

    // The official type allows `name: null` and `arguments: unknown`; the
    // gateway never invents them when the host did not provide them.
    let missing = RequiredAction {
        request_id: "appr_2".to_owned(),
        turn_id: "turn_2".to_owned(),
        server_label: String::new(),
        tool: None,
        arguments: serde_json::Value::Null,
    };
    let value = missing.to_json();
    assert!(value["request"]["name"].is_null());
    assert!(value["request"]["arguments"].is_null());
    assert_eq!(value["request"]["type"], "mcp_tool_call");
    assert!(value["request"]["message"]
        .as_str()
        .is_some_and(|message| message.contains("could not be determined")));
}
