//! Agent 工具恒后台的 ACP 装配面证据（`full-async-tools` 移除后的回归）。
//!
//! `host/stage_builder.rs` 不再从 `FrozenContext::beta_flags` 派生 Agent 缺省：
//! 后台是唯一执行模式，链上 `Agent` 工具 schema 不再提供执行模式开关。旧会话的
//! 冻结 blob 仍可能携带已删除的 `full-async-tools` 键——它必须被忽略，不能改变
//! schema 或导致装配失败。

use std::sync::Arc;

use crate::session::executor::FrozenSessionData;
use peri_agent::session::exec::executor_helpers::StageBuildRequest;

use super::executor_flow_tests;

/// 最小事件处理器（装配只要求类型在场）。
struct NoopEventHandler;

impl peri_agent::agent::events::AgentEventHandler for NoopEventHandler {
    fn on_event(&self, _event: peri_agent::agent::events::ExecutorEvent) {}
}

/// 带旧 `full-async-tools` 冻结值的会话数据（模拟已删除 flag 的历史 blob）。
fn frozen_with_legacy_flag() -> FrozenSessionData {
    let beta_flags = peri_acp_types::beta_flags::BetaFlags::from_values([(
        "full-async-tools".to_string(),
        peri_acp_types::beta_flags::BetaFlagValue {
            enabled: true,
            origin: peri_acp_types::beta_flags::BetaFlagOrigin::Workspace,
        },
    )]);
    FrozenSessionData::from_frozen_parts(
        peri_agent::session::FrozenContext::builder()
            .system_prompt("LEGACY-BETA-FLAG-AGENT-SCHEMA")
            .claude_md("")
            .skill_summary("")
            .date("2026-10-08")
            .beta_flags(beta_flags)
            .build(),
        None,
    )
}

/// 空投影与旧 flag 冻结值都必须得到同一个不含模式开关的恒后台 schema。
#[tokio::test]
async fn agent_tool_schema_has_no_background_mode_regardless_of_legacy_flag() {
    let ctx = executor_flow_tests::make_session_context("legacy-beta-flag-agent-schema").await;
    let cases = [
        (
            "empty",
            FrozenSessionData::from_frozen_parts(
                peri_agent::session::FrozenContext::builder()
                    .system_prompt("EMPTY-BETA-FLAG-AGENT-SCHEMA")
                    .claude_md("")
                    .skill_summary("")
                    .date("2026-10-08")
                    .build(),
                None,
            ),
        ),
        ("legacy", frozen_with_legacy_flag()),
    ];
    for (label, frozen_session) in cases {
        let request = StageBuildRequest {
            cached_llm: None,
            frozen_session,
            event_handler: Arc::new(NoopEventHandler),
            agent_overrides: None,
            preload_skills: Vec::new(),
            child_handler_factory: None,
            auxiliary_model: None,
            thread_persistence: Default::default(),
            goal_controller: None,
            task_manager: None,
            on_bg_complete: None,
        };
        let (out, _) =
            executor_flow_tests::make_stage_build(&ctx)(request).expect("stage 装配必须成功");
        let tools = out.context.runtime.tools.read();
        let agent = tools
            .values()
            .find(|tool| tool.name() == "Agent")
            .unwrap_or_else(|| {
                panic!(
                    "{label}: 链装配必须提供 Agent 工具；当前工具：{:?}",
                    tools.keys().collect::<Vec<_>>()
                )
            });
        let params = agent.parameters();
        assert!(
            params["properties"].get("run_in_background").is_none(),
            "{label}: Agent 工具 schema 不得再暴露执行模式开关：{params}"
        );
    }
}
