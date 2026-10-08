//! beta flag `full-async-tools` 的 ACP 装配面证据（Agent 侧）。
//!
//! `requests_beta_flags_test.rs` 覆盖「配置面 → 冻结值 → Bash / builtin 实例」；
//! 本文件覆盖 **Agent 侧**：`host/stage_builder.rs` 把**会话冻结**的 flag 投影为
//! 语义值，经 `StageBuildInput` / `AssemblyContext` / `ProductionChainAssembler`
//! 装配链，最终体现在链上 `Agent` 工具声明的 `run_in_background.default`。
//!
//! 接线断掉（投影不再读 `FrozenContext::beta_flags`、`AssemblyContext` 字段丢失、
//! `SubAgentMiddleware` 不再下传）时本用例报红——middleware 侧用例直接构造
//! `AssemblyContext`，覆盖不到 ACP 投影这一段。

use std::sync::Arc;

use crate::session::executor::FrozenSessionData;
use peri_agent::session::exec::executor_helpers::StageBuildRequest;

use super::executor_flow_tests::{make_session_context, make_stage_build};

/// 最小事件处理器（装配只要求类型在场）。
struct NoopEventHandler;

impl peri_agent::agent::events::AgentEventHandler for NoopEventHandler {
    fn on_event(&self, _event: peri_agent::agent::events::ExecutorEvent) {}
}

/// 带指定 beta flag 冻结值的会话冻结数据（其余字段取最小可用值）。
fn frozen_with_beta_flag(enabled: bool) -> FrozenSessionData {
    let beta_flags = if enabled {
        peri_acp_types::beta_flags::BetaFlags::from_values([(
            peri_acp_types::beta_flags::FULL_ASYNC_TOOLS.to_string(),
            peri_acp_types::beta_flags::BetaFlagValue {
                enabled: true,
                origin: peri_acp_types::beta_flags::BetaFlagOrigin::Workspace,
            },
        )])
    } else {
        peri_acp_types::beta_flags::BetaFlags::default()
    };
    FrozenSessionData::from_frozen_parts(
        peri_agent::session::FrozenContext::builder()
            .system_prompt("BETA-FLAG-AGENT-SCHEMA")
            .claude_md("")
            .skill_summary("")
            .date("2026-10-08")
            .beta_flags(beta_flags)
            .build(),
        None,
    )
}

/// 会话冻结的 flag 值必须到达链上 `Agent` 工具的 `run_in_background.default`。
#[tokio::test]
async fn frozen_beta_flag_reaches_agent_tool_schema() {
    let ctx = make_session_context("beta-flag-agent-schema").await;
    for (enabled, expected) in [(false, false), (true, true)] {
        let request = StageBuildRequest {
            cached_llm: None,
            frozen_session: frozen_with_beta_flag(enabled),
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
        let (out, _) = make_stage_build(&ctx)(request).expect("stage 装配必须成功");
        let tools = out.context.runtime.tools.read();
        let agent = tools
            .values()
            .find(|tool| tool.name() == "Agent")
            .unwrap_or_else(|| {
                panic!(
                    "链装配必须提供 Agent 工具；当前工具：{:?}",
                    tools.keys().collect::<Vec<_>>()
                )
            });
        assert_eq!(
            agent.parameters()["properties"]["run_in_background"]["default"],
            serde_json::json!(expected),
            "flag={enabled} 时 Agent 工具 schema 缺省必须为 {expected}"
        );
    }
}
