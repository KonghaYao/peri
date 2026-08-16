use std::sync::Arc;

use peri_acp_types::identity::AgentId;

use super::types::{SubagentLifecycleStart, SubagentLifecycleStop};
use super::util::extract_last_ai_text;
use super::v2_bridge::{forward_subagent_start_v1, forward_subagent_stop_v1, V2SubagentContext};
use super::{
    build_subagent_start_v2, build_subagent_stop_v2, emit_subagent_start_v2, emit_subagent_stop_v2,
    on_subagent_stop_handler, DeregisterGuard,
};
use crate::agent::events::AgentEventHandler;
use crate::agent::stages::{run_react_loop, LoopResult};
use crate::agent::subagent_event_forwarder::spawn_subagent_event_forwarder;
use crate::agent::LangfuseBridgeLike;
use crate::session::factory::{DeregisterRuntimeFn, RegisterRuntimeFn};
use crate::session::Session;
use crate::thread::ThreadStore;

// ─── 同步运行 ────────────────────────────────────────────────────────────────

/// 同步子 agent：当前调用内 run_react_loop，完成后收尾。
#[allow(clippy::too_many_arguments)]
pub(super) async fn run_sync_subagent(
    child_thread_id: &str,
    agent_name: &str,
    cwd: &str,
    max_iterations: usize,
    event_handler: Option<Arc<dyn AgentEventHandler>>,
    on_subagent_start: Option<SubagentLifecycleStart>,
    on_subagent_stop: Option<SubagentLifecycleStop>,
    thread_store: Option<Arc<dyn ThreadStore>>,
    register_runtime: Option<RegisterRuntimeFn>,
    deregister_runtime: Option<DeregisterRuntimeFn>,
    langfuse_bridge: Option<Arc<dyn LangfuseBridgeLike>>,
    parent_agent_id: Option<AgentId>,
    v2_ctx: V2SubagentContext,
    session: Arc<Session>,
) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
    let agent_name = agent_name.to_string();
    let cwd = cwd.to_string();

    // 启动注册（active_agents，与 DeregisterGuard drop 配对）
    if let Some(register) = &register_runtime {
        register(
            child_thread_id.to_string(),
            (*session.config().cancel_token).clone(),
            "cascade".into(),
        );
    }
    let _deregister_guard = DeregisterGuard {
        thread_id: child_thread_id.to_string(),
        deregister: deregister_runtime,
    };

    // lifecycle hook（SubagentStart）
    if let Some(ref on_start) = on_subagent_start {
        on_start(&agent_name, &cwd);
    }

    // v2 SubagentStart（C2）：parent_agent_id 未注入时静默跳过（helper 内 warn）
    emit_subagent_start_v2(
        &v2_ctx.event_bus,
        v2_ctx.context.turn_id(),
        parent_agent_id,
        v2_ctx.agent_id,
        &agent_name,
        false,
    );
    // v1 协议化载体直发（SubagentStarted）：发射语义单一事实源为 v2 事件构造
    // （ObserveEvent 身份透传：child_agent_id → instance_id），经
    // `observe_event_to_executor` 同步映射后直发父 handler——同步保证 Started
    // 恒先于本 turn 后续事件到达父协议化链路（v1 ExecutorEvent 中间态已退役，
    // 仅保留 ACP 协议序列化面映射，`2026-07-18-executor-event-retirement.md`）。
    forward_subagent_start_v1(
        event_handler.as_ref(),
        build_subagent_start_v2(
            v2_ctx.context.turn_id(),
            parent_agent_id,
            v2_ctx.agent_id,
            &agent_name,
            false,
        ),
    );

    // v2 事件转发器：子 EventBus → 父事件 handler（TUI 可见子 agent 工具调用/AI 文本）
    let _forwarder_handle = spawn_subagent_event_forwarder(
        v2_ctx.event_handles,
        event_handler.clone(),
        langfuse_bridge,
        child_thread_id.to_string(),
    );

    // 运行 v2 ReAct 循环
    let subagent_turn_id = v2_ctx.context.turn_id();
    let loop_result = run_react_loop(v2_ctx.context, max_iterations).await;

    // v2 SubagentStop（C3）：一个 emit 点覆盖 Completed / Interrupted / Error 三路
    let (stop_result, stop_is_error) = match &loop_result {
        LoopResult::Completed => (
            extract_last_ai_text(&session)
                .chars()
                .take(500)
                .collect::<String>(),
            false,
        ),
        LoopResult::Interrupted => ("interrupted".to_string(), true),
        LoopResult::Error(e) => (
            format!("{} execution failed: {}", agent_name, e)
                .chars()
                .take(500)
                .collect::<String>(),
            true,
        ),
    };
    // v2 SubagentStop（C3）：一个 emit 点覆盖 Completed / Interrupted / Error 三路。
    // 恢复路径复用本 Stop 发射点（R-L4）：stop_result 不含 child_thread_id——
    // issue 验收仅要求工具返回文本与 bg 通知文本携带 thread_id，事件侧不加。
    emit_subagent_stop_v2(
        &v2_ctx.event_bus,
        subagent_turn_id,
        parent_agent_id,
        v2_ctx.agent_id,
        &agent_name,
        &stop_result,
        stop_is_error,
    );
    // v1 协议化载体直发（SubagentStopped）：与 Started 同源（v2 事件构造 +
    // observe_event_to_executor 同步映射），保证 Stopped 在 turn 收尾前到达
    // 父协议化链路（TUI 容器销毁 / depth 配对）。
    forward_subagent_stop_v1(
        event_handler.as_ref(),
        build_subagent_stop_v2(
            subagent_turn_id,
            parent_agent_id,
            v2_ctx.agent_id,
            &agent_name,
            &stop_result,
            stop_is_error,
        ),
    );

    let (final_text, interrupted) = match loop_result {
        LoopResult::Completed => {
            let text = extract_last_ai_text(&session);
            (text, false)
        }
        LoopResult::Interrupted => (String::new(), true),
        LoopResult::Error(e) => {
            // child_thread_id 前缀：错误路径（LLM 网络错误等）必须可恢复——主 agent
            // 凭返回值中的 thread_id 找回执行现场（与 define.rs 成功路径
            // `child_thread_id: {id}\n{result}` 格式一致，多行展示）
            let error_summary = format!(
                "child_thread_id: {}\n{} execution failed: {}",
                child_thread_id, agent_name, e
            );
            let error_result: String = error_summary.chars().take(500).collect();
            // 统一后处理（hook + thread_store；v1 协议化直发已在 emit_subagent_stop_v2
            // 之后经 forward_subagent_stop_v1 发出）
            on_subagent_stop_handler(
                &on_subagent_stop,
                &thread_store,
                &agent_name,
                child_thread_id,
                &error_result,
                true,
                &cwd,
            )
            .await;
            return Err(error_summary.into());
        }
    };

    let output_summary: String = if interrupted {
        "interrupted".to_string()
    } else {
        final_text.chars().take(500).collect()
    };
    on_subagent_stop_handler(
        &on_subagent_stop,
        &thread_store,
        &agent_name,
        child_thread_id,
        &output_summary,
        interrupted,
        &cwd,
    )
    .await;

    Ok(interrupted)
}
