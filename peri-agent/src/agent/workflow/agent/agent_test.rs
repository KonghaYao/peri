//! 公共执行编排辅助与 forwarder 收尾契约。

use super::{
    await_workflow_forwarder, requested_model,
    result::{reported_model, workflow_forwarder_dead_result},
    select_workflow_system_prompt, tool_name_in, workflow_model_bridge,
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

// ─── M4：workflow 请求时贡献（生产 bridge 接线） ────────────────────────────

/// 贡献在 `before_agent` 之后填充（内部可变），模拟 Skills / ToolSearch 的
/// 真实时序：bridge 必须在每个 ModelRequest 读取当前值。
#[derive(Default)]
struct LateContributionMiddleware {
    contribution: std::sync::Arc<std::sync::Mutex<Option<String>>>,
}

#[async_trait::async_trait]
impl crate::middleware::r#trait::Middleware for LateContributionMiddleware {
    fn name(&self) -> &str {
        "LateContributionMiddleware"
    }

    fn prompt_contribution(&self) -> Option<String> {
        self.contribution.lock().unwrap().clone()
    }
}

/// 捕获每个请求 system 的 mock 模型（无工具调用，单轮完成）。
struct WorkflowCaptureModel {
    requests: std::sync::Mutex<Vec<peri_model::ModelRequest>>,
}

#[async_trait::async_trait]
impl peri_model::Model for WorkflowCaptureModel {
    fn capabilities(&self) -> peri_model::ModelCapabilities {
        peri_model::ModelCapabilities {
            supports_streaming: true,
            ..peri_model::ModelCapabilities::default()
        }
    }

    async fn stream(
        &self,
        request: peri_model::ModelRequest,
        cancellation: tokio_util::sync::CancellationToken,
    ) -> peri_model::ModelResult<peri_model::ModelStream> {
        self.requests.lock().unwrap().push(request);
        let response = peri_model::ModelResponse::new(
            peri_model::ModelMessage::assistant_text("done"),
            peri_model::StopReason::EndTurn,
            None,
            None,
        )?;
        Ok(peri_model::ModelStream::with_parent_cancellation(
            futures::stream::iter(vec![Ok(peri_model::ModelStreamEvent::Completed(response))]),
            cancellation,
        ))
    }
}

impl WorkflowCaptureModel {
    fn last_system(&self) -> String {
        self.requests
            .lock()
            .unwrap()
            .last()
            .map(|request| {
                request
                    .messages
                    .iter()
                    .filter_map(|message| match message {
                        peri_model::ModelMessage::System { content } => Some(
                            content
                                .iter()
                                .filter_map(|block| match block {
                                    peri_model::ContentBlock::Text { text } => Some(text.as_str()),
                                    _ => None,
                                })
                                .collect::<String>(),
                        ),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default()
    }
}

/// M4 验收：workflow bridge 的 base system 冻结，动态贡献在**请求时**读取
/// （`before_agent` 之后）；组合走统一权威（空行分隔、恰一次）。
#[tokio::test]
async fn workflow_bridge_reads_contributions_at_request_time() {
    let contribution = std::sync::Arc::new(std::sync::Mutex::new(None));
    let mut chain = crate::middleware::chain::MiddlewareChain::new();
    chain.add(Box::new(LateContributionMiddleware {
        contribution: std::sync::Arc::clone(&contribution),
    }));
    let chain = std::sync::Arc::new(chain);
    let model = std::sync::Arc::new(WorkflowCaptureModel {
        requests: std::sync::Mutex::new(Vec::new()),
    });
    let bridge = workflow_model_bridge(
        std::sync::Arc::clone(&model) as std::sync::Arc<dyn peri_model::Model>,
        "BASE_SYSTEM".to_string(),
        None,
        false,
        std::sync::Arc::clone(&chain),
        "workflow-session",
    );

    // 1) before_agent 之前：只有 base（不提前拍贡献快照，也不产生空段）。
    bridge
        .generate_reasoning(&[crate::messages::BaseMessage::human("hi")], &[], None)
        .await
        .unwrap();
    assert_eq!(model.last_system(), "BASE_SYSTEM");

    // 2) before_agent 填贡献后：同一个 bridge 的下一个请求带当前贡献。
    *contribution.lock().unwrap() = Some("LATE_SKILLS_SUMMARY".to_string());
    bridge
        .generate_reasoning(&[crate::messages::BaseMessage::human("again")], &[], None)
        .await
        .unwrap();
    let system = model.last_system();
    // 统一权威组合：静态 base + reserved boundary + 动态贡献（M1）。
    assert_eq!(
        system,
        format!(
            "BASE_SYSTEM{}\n\nLATE_SKILLS_SUMMARY",
            peri_model::prompt_cache::SYSTEM_PROMPT_DYNAMIC_BOUNDARY
        )
    );
    assert_eq!(
        system
            .matches(peri_model::prompt_cache::SYSTEM_PROMPT_DYNAMIC_BOUNDARY)
            .count(),
        1,
        "boundary marker 恰一个: {system}"
    );
    assert_eq!(
        system.matches("LATE_SKILLS_SUMMARY").count(),
        1,
        "贡献恰一次: {system}"
    );
}

/// [回归测试] 默认、agentType 与 fallback 渲染出的内部身份共用模型桥接
/// 组合入口；外部冻结字段不在渲染器中拼接。
#[tokio::test]
async fn workflow_bridge_appends_external_after_each_internal_projection() {
    let builder: crate::agent::workflow::WorkflowAgentPromptBuilder =
        std::sync::Arc::new(|_, _, _, _| "AGENT_TYPE_INTERNAL".into());
    let fallback: crate::agent::workflow::WorkflowSystemPromptFallback =
        std::sync::Arc::new(|_, _, _| "FALLBACK_INTERNAL".into());
    let cases = [
        (None, Some("DEFAULT_INTERNAL"), "DEFAULT_INTERNAL"),
        (
            Some(WorkflowAgentDefinition::default()),
            Some("DEFAULT_INTERNAL"),
            "AGENT_TYPE_INTERNAL",
        ),
        (None, None, "FALLBACK_INTERNAL"),
    ];
    for (definition, frozen_default, expected_internal) in cases {
        let internal = select_workflow_system_prompt(
            definition.as_ref(),
            frozen_default,
            &builder,
            &fallback,
            "/tmp",
            Some("2026-10-10"),
            None,
        );
        assert_eq!(internal, expected_internal);
        let chain = std::sync::Arc::new(crate::middleware::chain::MiddlewareChain::new());
        let model = std::sync::Arc::new(WorkflowCaptureModel {
            requests: std::sync::Mutex::new(Vec::new()),
        });
        let bridge = workflow_model_bridge(
            std::sync::Arc::clone(&model) as std::sync::Arc<dyn peri_model::Model>,
            internal,
            Some(std::sync::Arc::from("WORKFLOW_EXTERNAL")),
            false,
            chain,
            "workflow-session",
        );
        bridge
            .generate_reasoning(&[crate::messages::BaseMessage::human("go")], &[], None)
            .await
            .unwrap();
        let system = model.last_system();
        assert_eq!(system.matches("WORKFLOW_EXTERNAL").count(), 1);
        assert!(system.starts_with(expected_internal));
        assert!(
            system
                .find(peri_model::prompt_cache::SYSTEM_PROMPT_DYNAMIC_BOUNDARY)
                .unwrap()
                < system.find("WORKFLOW_EXTERNAL").unwrap()
        );
    }
}
