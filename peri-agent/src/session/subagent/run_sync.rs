use std::sync::Arc;

use peri_acp_types::identity::AgentId;

use super::factory::ResumeClaim;
use super::lifecycle::drain_subagent_events;
use super::types::{SubagentFailure, SubagentLifecycleStart, SubagentLifecycleStop};
use super::util::extract_last_ai_text;
use super::v2_bridge::{forward_subagent_start_v1, forward_subagent_stop_v1, V2SubagentContext};
use super::{
    build_subagent_start_v2, build_subagent_stop_v2_with_failure, emit_subagent_start_v2,
    emit_subagent_stop_v2_with_failure, on_subagent_stop_handler, DeregisterGuard,
    SubagentStopV2Input,
};
use crate::agent::events::AgentEventHandler;
use crate::agent::stages::{run_react_loop, LoopResult};
use crate::agent::subagent_event_forwarder::spawn_subagent_event_forwarder_for_completion;
use crate::agent::LangfuseBridgeLike;
use crate::session::factory::{DeregisterRuntimeFn, RegisterRuntimeFn};
use crate::session::Session;
use peri_acp_types::session_resources::SessionResources;

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
    session_resources: Option<Arc<dyn SessionResources>>,
    register_runtime: Option<RegisterRuntimeFn>,
    deregister_runtime: Option<DeregisterRuntimeFn>,
    langfuse_bridge: Option<Arc<dyn LangfuseBridgeLike>>,
    parent_agent_id: Option<AgentId>,
    parent_tool_call_id: Option<String>,
    mut v2_ctx: V2SubagentContext,
    session: Arc<Session>,
    mut resume_claim: Option<ResumeClaim>,
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
    let mut deregister_guard = DeregisterGuard {
        thread_id: child_thread_id.to_string(),
        deregister: deregister_runtime,
    };

    // The resumed claim crosses the first execution await with us. Its Drop
    // records cancellation without duplicating lifecycle Stop/hook delivery.
    if let Some(claim) = &mut resume_claim {
        claim.mark_running(v2_ctx.context.session.turn.clone(), session.clone());
    }
    let stop_resources = if resume_claim.is_some() {
        None // 认领持有终态写入（finish 内定向写状态），此处不重复写。
    } else {
        session_resources
    };

    let previous_observer = v2_ctx.context.sdk_admission_observed.take();
    let start_bus = v2_ctx.event_bus.clone();
    let start_handler = event_handler.clone();
    let start_agent_id = v2_ctx.agent_id;
    let start_name = agent_name.clone();
    let start_cwd = cwd.clone();
    // 真实子会话身份：hook 载荷 agent_id 的来源（不是 agent 名）。
    let start_thread_id = child_thread_id.to_string();
    v2_ctx.context.sdk_admission_observed = Some(Arc::new(move |admission| {
        if let Some(observe) = &previous_observer {
            observe(admission.clone());
        }
        if let Some(on_start) = &on_subagent_start {
            on_start(&start_thread_id, &start_name, &start_cwd);
        }
        emit_subagent_start_v2(
            &start_bus,
            admission.execution.turn_id,
            parent_agent_id,
            start_agent_id,
            &start_name,
            false,
            parent_tool_call_id.clone(),
        );
        forward_subagent_start_v1(
            start_handler.as_ref(),
            build_subagent_start_v2(
                admission.execution.turn_id,
                parent_agent_id,
                start_agent_id,
                &start_name,
                false,
                parent_tool_call_id.clone(),
            ),
        );
    }));

    // v2 事件转发器：子 EventBus → 父事件 handler（TUI 可见子 agent 工具调用/AI 文本）
    let forwarder_handle = spawn_subagent_event_forwarder_for_completion(
        v2_ctx.event_handles,
        event_handler.clone(),
        langfuse_bridge.clone(),
        child_thread_id.to_string(),
    );
    let child_agent_id = v2_ctx.agent_id;
    let event_bus = v2_ctx.event_bus;
    let owned_execution = super::settlement::OwnedSubagentExecution::capture(&v2_ctx.context);

    // 运行 v2 ReAct 循环
    let execution_turn = v2_ctx.context.session.turn.clone();
    let loop_result = run_react_loop(v2_ctx.context, max_iterations).await;
    if let LoopResult::Error(error) = &loop_result {
        if !execution_turn.cancel_token.is_cancelled()
            && !matches!(error, crate::error::AgentError::Interrupted)
        {
            tracing::error!(
                session_id = %child_thread_id,
                turn_id = %execution_turn.turn_id(),
                agent_name,
                category = error.category_name(),
                error = %error.user_facing_message(),
                causes = ?error.cause_chain(),
                "sync child execution failed"
            );
        }
    }
    let subagent_turn_id = execution_turn
        .work_admission()
        .map(|admission| admission.execution.turn_id);
    if let Err(error) = super::close::settle_explicit_close(
        &session,
        matches!(&loop_result, LoopResult::Interrupted),
    )
    .await
    {
        deregister_guard.deregister = None;
        if let Some(claim) = resume_claim.take() {
            claim.release().await;
        }
        return Err(error.into());
    }

    // v2 SubagentStop（C3）：一个 emit 点覆盖 Completed / Interrupted / Error 三路
    let stop_failure = match &loop_result {
        LoopResult::Error(error) => {
            SubagentFailure::safe_failure_from_error(child_thread_id, error)
        }
        _ => None,
    };
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
            format!(
                "{} execution failed: {}",
                agent_name,
                e.user_facing_message()
            )
            .chars()
            .take(500)
            .collect::<String>(),
            true,
        ),
    };
    // v2 SubagentStop（C3）：一个 emit 点覆盖 Completed / Interrupted / Error 三路。
    // 恢复路径复用本 Stop 发射点（R-L4）：stop_result 不含 child_thread_id——
    // issue 验收仅要求工具返回文本与 bg 通知文本携带 thread_id，事件侧不加。
    if let Some(subagent_turn_id) = subagent_turn_id {
        emit_subagent_stop_v2_with_failure(
            &event_bus,
            SubagentStopV2Input {
                turn_id: subagent_turn_id,
                parent_agent_id,
                child_agent_id,
                agent_name: &agent_name,
                result: &stop_result,
                is_error: stop_is_error,
                subagent_failure: stop_failure.clone(),
            },
        );
    }
    // Close the child producer after the terminal v2 event. Awaiting the
    // forwarder drains queued render/observe events (including visible deltas)
    // before the synchronous v1 stop reaches the parent handler.
    if let Err(error) = drain_subagent_events(event_bus, forwarder_handle, langfuse_bridge).await {
        // Retain cancellation and typed model diagnostics when execution had
        // already failed; a successful loop cannot hide forwarding failure.
        deregister_guard.deregister = None;
        if let Some(claim) = resume_claim.take() {
            claim.release().await;
        }
        return Err(Box::new(SubagentFailure::new(
            child_thread_id,
            &agent_name,
            error,
        )));
    }
    // v1 协议化载体直发（SubagentStopped）：与 Started 同源（v2 事件构造 +
    // observe_event_to_executor 同步映射），保证 Stopped 在 turn 收尾前到达
    // 父协议化链路（TUI 容器销毁 / depth 配对）。
    if let Some(subagent_turn_id) = subagent_turn_id {
        forward_subagent_stop_v1(
            event_handler.as_ref(),
            build_subagent_stop_v2_with_failure(
                subagent_turn_id,
                parent_agent_id,
                child_agent_id,
                &agent_name,
                &stop_result,
                stop_is_error,
                stop_failure,
            ),
        );
    }

    owned_execution.finish().await?;

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
            let failure = SubagentFailure::completed(child_thread_id, &agent_name, e);
            let error_summary = failure.to_string();
            let error_result: String = error_summary.chars().take(500).collect();
            // 统一后处理（hook + thread_store；v1 协议化直发已在 emit_subagent_stop_v2
            // 之后经 forward_subagent_stop_v1 发出）
            on_subagent_stop_handler(
                &on_subagent_stop,
                &stop_resources,
                &agent_name,
                child_thread_id,
                &error_result,
                true,
                &cwd,
            )
            .await;
            if let Some(claim) = resume_claim.take() {
                claim
                    .finish(peri_acp_types::thread::AgentStatus::Error)
                    .await?;
            }
            return Err(Box::new(failure));
        }
    };

    let output_summary: String = if interrupted {
        "interrupted".to_string()
    } else {
        final_text.chars().take(500).collect()
    };
    on_subagent_stop_handler(
        &on_subagent_stop,
        &stop_resources,
        &agent_name,
        child_thread_id,
        &output_summary,
        interrupted,
        &cwd,
    )
    .await;
    if let Some(claim) = resume_claim.take() {
        claim
            .finish(if interrupted {
                peri_acp_types::thread::AgentStatus::Error
            } else {
                peri_acp_types::thread::AgentStatus::Done
            })
            .await?;
    }

    Ok(interrupted)
}
