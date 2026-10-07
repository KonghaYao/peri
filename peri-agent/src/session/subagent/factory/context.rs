//! Shared child-session construction with frozen data and independent queue.
//! History ownership and first-run versus resume injection remain caller decisions.

use peri_acp_types::identity::AgentId;
use peri_acp_types::store::{InheritedContext, PersistedPayload};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

use super::super::types::{SubagentChainAssembler, SubagentChainContext, SubagentHost};
use super::super::v2_bridge::{build_v2_subagent_context, V2SubagentContext};
use crate::agent::react::ReactLLM;
use crate::agent::{CompactConfig, ContextBudget};
use crate::session::{FrozenContext, MessageQueue, Session};
use crate::tools::{BaseTool, ToolInvocationResolver};
use peri_acp_types::session_resources::SessionResources;

// ─── 共享 session 构造（spawn / resume 共用，D1） ───────────────────────────

/// 构造子 session + 链装配 + v2_ctx（[`super::spawn::spawn_subagent_impl`] 与
/// [`super::resume::resume_subagent_impl`] 共用的装配块）。
///
/// - session 以 `child_thread_id` 为 thread_id（subagent 必有持久化 thread；
///   thread_id = agent_id）；
/// - transcript 先装载只读 inherited snapshot、再装载当前 thread own history，
///   恢复各自 flags 后绑定持久化；装载不发送 append，避免跨 thread 的原 ID 写入；
/// - 链装配（skill_names / frozen 注入链上下文；链序由 assembler 实现方保持）；
/// - `build_v2_subagent_context` 构造 StageContext。
///
/// 父身份解析与消息注入（parent_messages / system_prompt / prompt）差异
/// 留在调用方；冻结快照与取消 token 的共享派生由本模块集中实现。
#[allow(clippy::too_many_arguments)]
pub(super) async fn build_subagent_session_v2(
    cwd: String,
    frozen: FrozenContext,
    cancel_token: CancellationToken,
    child_thread_id: String,
    parent_host: Option<Arc<SubagentHost>>,
    session_resources: Option<Arc<dyn SessionResources>>,
    inherited: InheritedContext,
    own: Vec<PersistedPayload>,
    llm: Box<dyn ReactLLM + Send + Sync>,
    chain_assembler: Arc<dyn SubagentChainAssembler>,
    tools: Vec<Arc<dyn BaseTool>>,
    tool_filter: crate::session::tool_catalog::ToolFilter,
    session_mcp_capability: Option<Arc<dyn peri_acp_types::ports::SessionMcpCapabilityPort>>,
    skill_names: Vec<String>,
    frozen_claude_md: Option<String>,
    frozen_claude_local_md: Option<String>,
    frozen_skill_summary: Option<String>,
    tool_invocation_resolver: Option<Arc<dyn ToolInvocationResolver>>,
    compact_config: Option<CompactConfig>,
    context_budget: Option<ContextBudget>,
    compact_llm: Option<Arc<dyn peri_model::Model>>,
    agent_id: Option<AgentId>,
) -> Result<(Arc<Session>, V2SubagentContext), Box<dyn std::error::Error + Send + Sync>> {
    let cancel_arc: Arc<CancellationToken> = Arc::new(cancel_token.clone());
    let mut host = parent_host.as_deref().cloned().unwrap_or_default();
    let lifecycle = match &session_resources {
        Some(resources) => {
            let control = resources.load_session_control(&child_thread_id).await?;
            if control.status != peri_acp_types::session_resources::ControlStatus::Active {
                return Err(
                    "Incomplete: child control is not Active; explicit Reopen required".into(),
                );
            }
            Some(control.lifecycle)
        }
        None => None,
    };
    let binding = match (&host.mcp_pool, lifecycle) {
        (Some(pool), Some(lifecycle)) => {
            pool.clone()
                .agent_session_binding_for_lifecycle(&child_thread_id, lifecycle)
                .await?
        }
        (Some(_), None) => return Err("Incomplete: child lifecycle identity unavailable".into()),
        (None, _) => None,
    };
    let queue = binding
        .as_ref()
        .map(|(inbox, _)| inbox.queue().clone())
        .unwrap_or_else(MessageQueue::new);
    let session = Session::new_with_cancel_and_queue(
        Arc::from(cwd.as_str()),
        frozen,
        Some(child_thread_id.clone()),
        cancel_arc,
        queue,
    );
    host.session_resources = session_resources.clone();
    host.close_state = Arc::default();
    host.task_manager = Some(match binding {
        Some((_, manager)) => {
            let manager: Arc<dyn std::any::Any + Send + Sync> = manager;
            Arc::downcast::<crate::agent::async_tasks::TaskManager>(manager)
                .map_err(|_| "subagent session task directory has incompatible manager")?
        }
        None => Arc::new(crate::agent::async_tasks::TaskManager::new()),
    });
    host.parent_thread_id = Some(child_thread_id.clone());
    host.on_bg_complete = match (&session_resources, lifecycle) {
        (Some(resources), Some(lifecycle)) => {
            Some(crate::session::bg_complete::durable_bg_complete_callback(
                crate::agent::async_tasks::durable_task_terminal_delivery(
                    resources.clone(),
                    child_thread_id.clone(),
                    lifecycle,
                    session.queue().clone(),
                ),
            ))
        }
        _ => None,
    };
    if let (Some(pool), Some(lifecycle), Some(manager)) =
        (&host.mcp_pool, lifecycle, &host.task_manager)
    {
        let inbox = peri_acp_types::session::SessionInbox::new(Arc::new(session.queue().clone()));
        pool.bind_agent_session_for_lifecycle(
            &child_thread_id,
            lifecycle,
            inbox.handle(),
            manager.clone(),
        )?;
        let resources = session_resources
            .clone()
            .ok_or("Incomplete: child session resources unavailable")?;
        pool.bind_agent_session_resources(&child_thread_id, lifecycle, resources)?;
    }
    session.set_subagent_host(host);

    // transcript 绑定（ancestor 先于 with_persistence，顺序不可反）
    {
        let transcript_arc = session.transcript();
        let mut transcript = transcript_arc.write();
        let old = std::mem::take(&mut *transcript);
        let mut with_ancestor = old
            .with_ancestor_payloads(inherited.payloads)
            .with_own_payloads(own);
        with_ancestor.set_flags_batch(inherited.flags);
        *transcript = match session_resources {
            Some(ref store) => {
                with_ancestor.with_persistence(Arc::clone(store), child_thread_id.clone())
            }
            None => with_ancestor,
        };
    }

    // 子链装配（frozen 数据注入链上下文；链序由 assembler 实现方保持）。
    // meta_harness_disabled 从 frozen 状态投影（spawn/resume 两条路径都复制
    // 父 meta_harness，子链独立装配必须同样过滤——设计 §2.5）。
    let chain = chain_assembler.assemble(&SubagentChainContext {
        cwd: cwd.clone(),
        skill_names,
        frozen_claude_md,
        frozen_claude_local_md,
        frozen_skill_summary,
        meta_harness_disabled: session
            .store()
            .frozen
            .meta_harness
            .disabled_middlewares
            .clone(),
    });

    // StageContext 构造（v2_bridge 迁移；tool_invocation_resolver 参数化；
    // 复用上面预创建的 session——transcript 已装载 ancestor 并绑定持久化）
    let tools = tools
        .into_iter()
        .filter(|tool| tool_filter(tool.as_ref()))
        .collect();
    let mut v2_ctx = build_v2_subagent_context(
        Some(session.clone()),
        llm,
        chain,
        chain_assembler.bind_tools(&session, tools, Arc::clone(&tool_filter)),
        tool_filter,
        session_mcp_capability,
        &cwd,
        cancel_token,
        tool_invocation_resolver,
        compact_config,
        context_budget,
        compact_llm,
        agent_id,
    );
    v2_ctx.context.recipient_lifecycle = lifecycle;

    Ok((session, v2_ctx))
}

/// Build the immutable child snapshot from already-resolved parent/fallback values.
/// Local CLAUDE data remains a distinct chain input, just as in spawn/resume.
pub(super) fn inherited_frozen_context(
    parent: Option<&Arc<Session>>,
    frozen_claude_md: &Option<String>,
    frozen_skill_summary: &Option<String>,
    frozen_date: &Option<String>,
) -> FrozenContext {
    FrozenContext {
        system_prompt: parent
            .map(|p| Arc::clone(&p.store().frozen.system_prompt))
            .unwrap_or_default(),
        claude_md: frozen_claude_md
            .as_ref()
            .map(|s| Arc::from(s.as_str()))
            .unwrap_or_default(),
        skill_summary: frozen_skill_summary
            .as_ref()
            .map(|s| Arc::from(s.as_str()))
            .unwrap_or_default(),
        date: frozen_date
            .as_ref()
            .map(|s| Arc::from(s.as_str()))
            .unwrap_or_default(),
        language: parent.and_then(|p| p.store().frozen.language.clone()),
        // MetaHarness 冻结状态随父 session 复制（ARC-FROZEN-001：不重读配置/磁盘）
        meta_harness: parent
            .map(|p| p.store().frozen.meta_harness.clone())
            .unwrap_or_default(),
        // 冻结运行环境同样随父 session 复制：子 Agent 只消费继承的冻结输入，
        // 不在恢复/派生时重探（H3）。
        runtime_env: parent.and_then(|p| p.store().frozen.runtime_env.clone()),
    }
}

/// Derive a fresh child token; Independent never reuses the fallback parent token.
pub(super) fn derive_cancel_token(
    parent: Option<&Arc<Session>>,
    fallback: Option<CancellationToken>,
    policy: peri_acp_types::thread::CancelPolicy,
) -> CancellationToken {
    match policy {
        peri_acp_types::thread::CancelPolicy::Cascade => parent
            .map(|p| p.config().cancel_token.child_token())
            .or_else(|| fallback.map(|t| t.child_token()))
            .unwrap_or_default(),
        peri_acp_types::thread::CancelPolicy::Independent => CancellationToken::new(),
    }
}
