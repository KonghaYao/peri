//! 公共执行编排辅助与 forwarder 收尾契约。

use super::{
    await_workflow_forwarder, requested_model,
    result::{reported_model, workflow_forwarder_dead_result},
    tool_name_in,
};
use crate::agent::workflow::WorkflowAgentDefinition;

#[test]
fn agent_type_tool_matching_is_case_insensitive() {
    assert!(tool_name_in(&["Read".into(), "Grep".into()], "read"));
    assert!(tool_name_in(&["*".into()], "Write"));
    assert!(!tool_name_in(&["Read".into()], "Write"));
}

/// [回归测试] MCP 迁移后，旧 agent.md / allowedTools 的允许和禁止列表
/// 必须仍匹配同一能力；否则允许列表丢工具、禁止列表放行写入和 shell。
#[test]
fn workflow_tool_policy_matches_builtin_names_in_both_directions() {
    for instance in peri_acp_types::builtin_mcp::BUILTIN_MCP_INSTANCES {
        for tool in instance.tools {
            assert!(
                tool_name_in(&[tool.original_name.into()], tool.effective_name),
                "旧声明必须覆盖 MCP 工具：{}",
                tool.effective_name
            );
            assert!(
                tool_name_in(&[tool.effective_name.into()], tool.original_name),
                "MCP 声明必须覆盖原始工具：{}",
                tool.original_name
            );
            assert!(tool_name_in(
                &[tool.original_name.to_lowercase()],
                tool.effective_name
            ));
        }
    }
}

/// 外部服务同名工具不是 builtin 能力，禁止通过拆前缀扩大授权。
#[test]
fn workflow_tool_policy_preserves_external_names_and_wildcard_rules() {
    assert!(!tool_name_in(&["Bash".into()], "mcp__external__Bash"));
    assert!(!tool_name_in(&["Read".into()], "mcp__workspace__Write"));
    assert!(tool_name_in(&["*".into()], "mcp__workspace__Write"));
    assert!(!tool_name_in(
        &["*".into(), "Read".into()],
        "mcp__workspace__Write"
    ));
    assert!(tool_name_in(
        &["mcp__external__Bash".into()],
        "MCP__EXTERNAL__BASH"
    ));
    assert!(!tool_name_in(&[], "mcp__workspace__Read"));
}

#[test]
fn requested_model_prefers_workflow_value() {
    let definition = WorkflowAgentDefinition {
        model: Some("haiku".into()),
        ..Default::default()
    };

    assert_eq!(
        requested_model(Some("sonnet"), Some(&definition)),
        Some("sonnet")
    );
}

#[test]
fn requested_model_inherit_overrides_agent_definition() {
    let definition = WorkflowAgentDefinition {
        model: Some("haiku".into()),
        ..Default::default()
    };

    assert_eq!(requested_model(Some("inherit"), Some(&definition)), None);
}

#[test]
fn requested_model_trims_concrete_model_name() {
    assert_eq!(
        requested_model(Some("  claude-sonnet-4-5  "), None),
        Some("claude-sonnet-4-5")
    );
}

#[test]
fn requested_model_uses_agent_definition_when_omitted() {
    let definition = WorkflowAgentDefinition {
        model: Some("haiku".into()),
        ..Default::default()
    };

    assert_eq!(requested_model(None, Some(&definition)), Some("haiku"));
}

#[test]
fn result_model_falls_back_to_effective_model() {
    assert_eq!(
        reported_model(None, "claude-haiku-4-5"),
        Some("claude-haiku-4-5".into())
    );
    assert_eq!(
        reported_model(Some("provider-reported".into()), "claude-haiku-4-5"),
        Some("provider-reported".into())
    );
}

#[tokio::test]
async fn workflow_drops_last_event_bus_owner_before_awaiting_forwarder() {
    let (bus, mut handles) =
        crate::agent::events_v2::EventBus::new(crate::agent::events_v2::EventBusConfig::default());
    let handle = tokio::spawn(async move {
        while handles.render_rx.recv().await.is_some() {}
        while handles.state_rx.recv().await.is_some() {}
        while handles.observe_rx.recv().await.is_ok() {}
    });
    tokio::time::timeout(
        std::time::Duration::from_secs(1),
        await_workflow_forwarder(bus.into(), handle),
    )
    .await
    .expect("dropping the separately-held EventBus must close all channels")
    .expect("normal forwarder completion");
}

#[tokio::test]
async fn workflow_forwarder_join_error_maps_to_dead_failure() {
    let (bus, _handles) =
        crate::agent::events_v2::EventBus::new(crate::agent::events_v2::EventBusConfig::default());
    let handle = tokio::spawn(std::future::pending());
    handle.abort();
    let failure = await_workflow_forwarder(bus.into(), handle)
        .await
        .expect_err("aborted forwarder must fail the workflow run");
    assert_eq!(
        failure.kind,
        peri_acp_types::session::ExecutionFailureKind::Internal
    );
    assert!(matches!(
        workflow_forwarder_dead_result(),
        peri_acp_types::workflow::AgentRunResult::Dead { reason: Some(reason), .. }
            if reason == "event-forwarder-failed"
    ));
}
