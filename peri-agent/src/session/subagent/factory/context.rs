//! Shared child-session construction with frozen data and independent queue.
//! History ownership and first-run versus resume injection remain caller decisions.

use peri_acp_types::identity::AgentId;
use peri_acp_types::store::{InheritedContext, PersistedPayload};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

use super::super::types::{
    SubagentChainAssembler, SubagentChainContext, SubagentHost, SubagentLlmSource,
};
use super::super::v2_bridge::{build_v2_subagent_context, V2SubagentContext};
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
    llm: SubagentLlmSource,
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
    normalize_persisted_identity: bool,
) -> Result<(Arc<Session>, V2SubagentContext), Box<dyn std::error::Error + Send + Sync>> {
    // 子身份（H1/M3）：唯一事实源 = 子 FrozenContext.system_prompt（子能力投影
    // 后的身份字节）。装配不重新探测、不复制父字节——父字节在 spawn/resume/cold
    // 三处入口就已换成子身份投影。
    let identity_system = frozen.system_prompt.to_string();
    let cancel_arc: Arc<CancellationToken> = Arc::new(cancel_token.clone());
    let mut host = parent_host.as_deref().cloned().unwrap_or_default();
    let binding = host
        .mcp_pool
        .as_ref()
        .and_then(|pool| pool.agent_session_binding(&child_thread_id));
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
    host.on_bg_complete = Some(crate::session::bg_complete::task_bg_complete_callback(
        crate::agent::async_tasks::delivery::SessionTerminalDelivery::for_queue(
            session.queue().clone(),
        ),
    ));
    if let (Some(pool), Some(manager)) = (&host.mcp_pool, &host.task_manager) {
        let inbox = peri_acp_types::session::SessionInbox::new(Arc::new(session.queue().clone()));
        pool.bind_agent_session(&child_thread_id, inbox.handle(), manager.clone());
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

    // H1：bridge 在有子链的一方装配——base system = 子身份投影；动态后缀由
    // 每次 ModelRequest 同步读取 `chain.collect_prompt_contributions()`
    // （与主链同一语义：before_agent 之后收集，不提前拍快照）。
    // `Prebuilt`（嵌入/测试已装配 ReactLLM）原样透传。
    let chain = Arc::new(chain);
    let contribution_chain = Arc::clone(&chain);
    let llm = llm.into_react_llm(
        &identity_system,
        Arc::new(move || contribution_chain.collect_prompt_contributions()),
        // 归一化：身份 System 随 transcript 持久化（spawn 6b 写入；旧会话的历史
        // 同形），模型投影吸收与身份逐字相同的那一条（恰一条），身份在请求面
        // 只由 bridge base system 出现一次。其余 System 消息（命令反馈等）不受
        // 影响。
        normalize_persisted_identity,
    );

    // StageContext 构造（v2_bridge 迁移；tool_invocation_resolver 参数化；
    // 复用上面预创建的 session——transcript 已装载 ancestor 并绑定持久化）
    let tools = tools
        .into_iter()
        .filter(|tool| tool_filter(tool.as_ref()))
        .collect();
    let v2_ctx = build_v2_subagent_context(
        Some(session.clone()),
        llm,
        Arc::clone(&chain),
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

    Ok((session, v2_ctx))
}

/// Build the immutable child snapshot from already-resolved parent/fallback values.
/// Local CLAUDE data remains a distinct chain input, just as in spawn/resume.
///
/// `identity_system`（H1/M3）= 子能力投影后的身份字节，来自注入的
/// `system_builder`（定义型带 overrides / fork 无 overrides）。子
/// `FrozenContext.system_prompt` 是身份的单一事实源：**不再复制父字节**——
/// 复制父 prompt 会把父能力声明（如 HITL/子代理）带进子请求面。
pub(super) fn inherited_frozen_context(
    parent: Option<&Arc<Session>>,
    identity_system: Option<&str>,
    frozen_claude_md: &Option<String>,
    frozen_skill_summary: &Option<String>,
    frozen_date: &Option<String>,
) -> FrozenContext {
    FrozenContext {
        system_prompt: identity_system.map(Arc::from).unwrap_or_default(),
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
