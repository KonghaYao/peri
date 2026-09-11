//! Shared child-session construction with frozen data and independent queue.
//! History ownership and first-run versus resume injection remain caller decisions.

use peri_acp_types::identity::AgentId;
use peri_acp_types::store::{InheritedContext, PersistedPayload};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

use super::super::types::{SubagentChainAssembler, SubagentChainContext};
use super::super::v2_bridge::{build_v2_subagent_context, V2SubagentContext};
use crate::agent::react::ReactLLM;
use crate::agent::{CompactConfig, ContextBudget};
use crate::error_suggest::{ErrorSuggestRegistry, ToolRegistrySnapshot};
use crate::session::{FrozenContext, MessageQueue, Session};
use crate::thread::ThreadStore;
use crate::tools::{BaseTool, ToolInvocationResolver};

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
pub(super) fn build_subagent_session_v2(
    cwd: String,
    frozen: FrozenContext,
    cancel_token: CancellationToken,
    child_thread_id: String,
    thread_store: Option<Arc<dyn ThreadStore>>,
    inherited: InheritedContext,
    own: Vec<PersistedPayload>,
    llm: Box<dyn ReactLLM + Send + Sync>,
    chain_assembler: Arc<dyn SubagentChainAssembler>,
    tools: Vec<Arc<dyn BaseTool>>,
    tool_filter: Arc<dyn Fn(&str) -> bool + Send + Sync>,
    session_mcp_capability: Option<Arc<dyn peri_acp_types::ports::SessionMcpCapabilityPort>>,
    skill_names: Vec<String>,
    frozen_claude_md: Option<String>,
    frozen_claude_local_md: Option<String>,
    frozen_skill_summary: Option<String>,
    tool_invocation_resolver: Option<Arc<dyn ToolInvocationResolver>>,
    error_suggest_registry: Option<Arc<ErrorSuggestRegistry>>,
    tool_registry_snapshot: Option<ToolRegistrySnapshot>,
    compact_config: Option<CompactConfig>,
    context_budget: Option<ContextBudget>,
    compact_llm: Option<Arc<dyn peri_model::Model>>,
    agent_id: Option<AgentId>,
) -> (Arc<Session>, V2SubagentContext) {
    let cancel_arc: Arc<CancellationToken> = Arc::new(cancel_token.clone());
    // SubAgent 独立 MessageQueue（不与 main agent 共享）
    let queue = MessageQueue::new();
    let session = Session::new_with_cancel_and_queue(
        Arc::from(cwd.as_str()),
        frozen,
        Some(child_thread_id.clone()),
        cancel_arc,
        queue,
    );

    // transcript 绑定（ancestor 先于 with_persistence，顺序不可反）
    {
        let transcript_arc = session.transcript();
        let mut transcript = transcript_arc.write();
        let old = std::mem::take(&mut *transcript);
        let mut with_ancestor = old
            .with_ancestor_payloads(inherited.payloads)
            .with_own_payloads(own);
        with_ancestor.set_flags_batch(inherited.flags);
        *transcript = match thread_store {
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
    let v2_ctx = build_v2_subagent_context(
        Some(session.clone()),
        llm,
        chain,
        tools,
        tool_filter,
        session_mcp_capability,
        &cwd,
        cancel_token,
        tool_invocation_resolver,
        error_suggest_registry,
        tool_registry_snapshot,
        compact_config,
        context_budget,
        compact_llm,
        agent_id,
    );

    (session, v2_ctx)
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
