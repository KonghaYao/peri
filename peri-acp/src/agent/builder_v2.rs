//! StageContext builder（P5 后的单一路径）
//!
//! 从 `AcpAgentConfig` 构造 `StageContext`，复用 `builder::build_agent()` 的中间件链
//! 与 LLM 构造（产出 `AgentComponents`），避免重复实现 700+ 行装配逻辑。
//!
//! ## 工具注入
//!
//! `run_react_loop` 每轮从 `shared_tools`（`SharedToolMap`）按名读取工具，
//! 不会每轮重新填充。因此 `build_stage_context` 内部显式调用
//! `chain.collect_tools(cwd)` 把 middleware 提供的工具 + `register_tool` 注册的
//! `AskUserQuestion` 一次性 merge 到 `shared_tools`（已存在的同名工具不覆盖，
//! 保留 deferred / 外部注册版本）。

use std::sync::Arc;

use parking_lot::RwLock;
use peri_agent::{
    agent::{
        events::ExecutorEvent,
        events_v2::{EventBus, EventBusConfig, EventHandles},
        react::ReactLLM,
        stages::{SharedToolMap, StageContext},
    },
    group::pipeline::AgentId,
    session::Session as V2Session,
};

use crate::agent::builder::{build_agent, AcpAgentConfig};
use crate::session::agent_pool::{AgentPool, CachedLlmInstances};

/// v2 builder 产物
pub struct V2AgentOutput {
    /// 已配置的 StageContext（用于 run_react_loop）
    pub context: StageContext,
    /// v2 Session（持有 transcript + queue + store）
    pub session: Arc<V2Session>,
    /// EventBus 消费端（转 ExecutorEvent 用）
    pub event_handles: EventHandles,
    /// Todo 更新通道（spawn todo forwarder 用）
    pub todo_rx: tokio::sync::mpsc::Receiver<Vec<peri_middlewares::tools::TodoItem>>,
    /// 后台任务完成事件接收端（spawn bg event pump 用）
    pub bg_event_rx: tokio::sync::mpsc::UnboundedReceiver<ExecutorEvent>,
}

/// 从 AcpAgentConfig 构造 StageContext
///
/// 内部调用 `build_agent` 提取 middleware chain + LLM + 共享组件（`AgentComponents`），
/// 然后构造 StageContext。
///
/// **`shared_queue`**：会话级共享的 v2 [`MessageQueue`]。每个 turn 调用本函数时
/// 必须传入**同一个**实例（来自 `AcpSession.v2_message_queue`），让本 turn 的
/// StageContext.queue 与会话级共享。否则每 turn 新建 Session（含新 queue），
/// 导致 SubAgent / Hook / GoalSteering 注入的 deferred / info 消息互不可见。
///
/// `MessageQueue` 内部 `Arc<Mutex<VecDeque>> + Arc<Notify>`，clone 共享底层；
/// 传入引用只是为了避免在签名里 move。
pub fn build_stage_context(
    cfg: AcpAgentConfig,
    cached_llm: Option<&CachedLlmInstances>,
    pool: &Arc<parking_lot::Mutex<AgentPool>>,
    shared_queue: &peri_agent::session::MessageQueue,
) -> (V2AgentOutput, Option<CachedLlmInstances>) {
    // 提取 LLM 用字段（在 cfg 被 build_agent 消费前）
    let cwd = cfg.cwd.clone();
    let session_id = cfg.session_id.clone();
    let cancel_token = cfg.cancel.clone();
    // compact_llm：优先取 cfg.auxiliary_model，否则回落到 cached auxiliary_model。
    // 必须在 build_agent 消费 cfg 前 clone（AgentComponents 不暴露 compact_llm）。
    let compact_llm_for_v2 = cfg
        .auxiliary_model
        .clone()
        .or_else(|| cached_llm.map(|c| c.auxiliary_model.clone()));

    // 提取 hooks 和模型名（在 cfg 被 build_agent 消费前）
    let hook_groups_flat: Vec<peri_middlewares::hooks::types::RegisteredHook> =
        cfg.hook_groups.iter().flatten().cloned().collect();
    let hook_model = cfg.provider.model_name().to_string();
    let hook_session_id = session_id.clone().unwrap_or_default();

    // 调用 build_agent 构造完整 agent（含中间件链 + LLM）
    let (agent_output, new_cached) = build_agent(cfg, cached_llm, pool);

    // 直接消费 AgentComponents
    let crate::agent::builder::AgentComponents {
        llm,
        chain,
        shared_tools: shared_tools_opt,
        error_suggest_registry,
        tool_registry_snapshot,
        system_prompt,
        context_budget,
        compact_config,
    } = agent_output.components;

    let shared_tools: SharedToolMap =
        shared_tools_opt.unwrap_or_else(|| Arc::new(RwLock::new(std::collections::HashMap::new())));

    // run_react_loop 每轮从 shared_tools 按名读取工具，不会每轮重新填充。
    // 这里一次性把 middleware 提供的工具（FilesystemTools / Terminal / Web /
    // Todo / Cron / Hook / SubAgent / Mcp / ToolSearch / Lsp / Goal / Workflow）
    // 以及 register_tool 注册的 AskUserQuestion 注入到 shared_tools。
    //
    // 已存在的同名工具不覆盖（deferred tools 优先保留外部注册版本）。
    {
        let middleware_tools = chain.collect_tools(&cwd);
        let mut tools = shared_tools.write();
        for tool in middleware_tools {
            let arc: std::sync::Arc<dyn peri_agent::tools::BaseTool> = std::sync::Arc::from(tool);
            tools.entry(arc.name().to_string()).or_insert_with(|| arc);
        }
    }

    // 构造 v2 Session（复用外部 cancel token + 会话级共享 MessageQueue）。
    // 用 new_with_cancel_and_queue 把 shared_queue 装进 Session，使
    // session.queue() 与外部共享同一份消息流。
    let cwd_arc: Arc<str> = Arc::from(cwd.as_str());
    let frozen = peri_agent::session::FrozenContext::builder().build();
    let cancel_arc = Arc::new(cancel_token);
    let session = V2Session::new_with_cancel_and_queue(
        cwd_arc,
        frozen,
        None,
        cancel_arc,
        shared_queue.clone(),
    );

    let turn = session.start_turn();
    let transcript = session.transcript();
    // session.queue() 已装入 shared_queue；clone 仍与外部共享同一份底层。
    let queue = session.queue().clone();

    // 创建 EventBus
    let (event_bus, event_handles) = EventBus::new(EventBusConfig::default());

    // session_context 键值
    let session_context = Arc::new(RwLock::new({
        let mut map = std::collections::HashMap::new();
        if let Some(sid) = &session_id {
            map.insert("session_id".to_string(), sid.clone());
        }
        map
    }));

    // 复用 build_agent 产出的 LLM（RetryableLLM<BaseModelReactLLM> 已实现 ReactLLM）
    let react_llm: Arc<dyn ReactLLM + Send + Sync> = Arc::new(llm);

    // 构造 StageContext
    let mut builder = StageContext::builder(turn, transcript, queue)
        .with_agent_id(AgentId::new())
        .with_llm(react_llm)
        .with_tools(shared_tools)
        .with_middleware_chain(Arc::new(chain))
        .with_event_bus(Arc::new(event_bus))
        .with_session_context(session_context)
        .with_tool_registry_snapshot((*tool_registry_snapshot).clone());

    if let Some(reg) = error_suggest_registry {
        builder = builder.with_error_suggest_registry(reg);
    }
    if let Some(budget) = context_budget {
        builder = builder.with_context_budget(budget);
    }
    if let Some(cc) = compact_config {
        builder = builder.with_compact_config(cc);
    }
    if let Some(llm) = compact_llm_for_v2 {
        builder = builder.with_compact_llm(llm);
    }
    if let Some(sp) = system_prompt {
        builder = builder.with_system_prompt(sp);
    }

    // 注入 compact plugin hook 回调（PreCompact / PostCompact）
    if !hook_groups_flat.is_empty() {
        {
            let hooks = hook_groups_flat.clone();
            let h_cwd = cwd.clone();
            let h_sid = hook_session_id.clone();
            let h_model = hook_model.clone();
            builder = builder.with_compact_pre_hook(Arc::new(move || {
                let hooks = hooks.clone();
                let cwd = h_cwd.clone();
                let sid = h_sid.clone();
                let model = h_model.clone();
                tokio::spawn(async move {
                    peri_middlewares::hooks::stage_firing::fire_pre_compact(
                        &hooks, &cwd, &sid, "", &model, 0,
                    )
                    .await;
                });
            }));
        }
        {
            let hooks = hook_groups_flat.clone();
            let h_cwd = cwd.clone();
            let h_sid = hook_session_id.clone();
            let h_model = hook_model.clone();
            builder = builder.with_compact_post_hook(Arc::new(
                move |_compacted: bool, affected_count: usize| {
                    let hooks = hooks.clone();
                    let cwd = h_cwd.clone();
                    let sid = h_sid.clone();
                    let model = h_model.clone();
                    tokio::spawn(async move {
                        peri_middlewares::hooks::stage_firing::fire_post_compact(
                            &hooks,
                            &cwd,
                            &sid,
                            "",
                            &model,
                            affected_count,
                        )
                        .await;
                    });
                },
            ));
        }
    }

    let context = builder.build();

    (
        V2AgentOutput {
            context,
            session,
            event_handles,
            todo_rx: agent_output.todo_rx,
            bg_event_rx: agent_output.bg_event_rx,
        },
        new_cached,
    )
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_v2_context_has_null_llm_by_default() {
        // 不传 llm 时，StageContext 应使用 NullReactLLM
        let cwd: Arc<str> = Arc::from("/tmp");
        let frozen = peri_agent::session::FrozenContext::builder().build();
        let session = V2Session::new(cwd, frozen, None);
        let turn = session.start_turn();
        let ctx =
            StageContext::builder(turn, session.transcript(), session.queue().clone()).build();
        assert_eq!(ctx.llm.model_name(), "null");
    }
}
