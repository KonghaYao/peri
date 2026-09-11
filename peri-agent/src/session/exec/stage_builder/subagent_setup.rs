//! collect_tools 前注入父身份、运行时宿主与同一份冻结快照。
use super::{BgEventTx, StageBuildInput};
use crate::{
    agent::async_tasks::TaskManager,
    session::{
        exec::executor::FrozenSessionData,
        factory::{OnBgCompleteFn, SubAgentMiddlewarePort},
        subagent::SubagentHost,
        Session,
    },
};
use peri_acp_types::{frozen::ThreadPersistence, identity::AgentId};
use std::sync::Arc;

pub(super) struct SubagentDependencies<'a> {
    pub(super) frozen_session: &'a FrozenSessionData,
    pub(super) thread_persistence: &'a ThreadPersistence,
    pub(super) task_manager: &'a Option<Arc<TaskManager>>,
    pub(super) on_bg_complete: &'a Option<OnBgCompleteFn>,
    pub(super) bg_event_tx: BgEventTx,
}

pub(super) fn attach_subagent_host(
    input: &StageBuildInput,
    session: &Arc<Session>,
    main_agent_id: AgentId,
    subagent_mw: &Option<Arc<dyn SubAgentMiddlewarePort>>,
    dependencies: SubagentDependencies<'_>,
) {
    let SubagentDependencies {
        frozen_session,
        thread_persistence,
        task_manager,
        on_bg_complete,
        bg_event_tx,
    } = dependencies;
    // 注入父 agent 身份（C2）：SubAgentTool 持有同一共享 cell，
    // invoke 时（必然晚于本调用）读到已 set 的值——共享 cell 消除顺序问题。
    if let Some(mw) = subagent_mw {
        mw.set_parent_agent_id(main_agent_id);
    }

    // L3：注入子 agent 运行时宿主（SubagentHost）并挂到主 session。
    // SubAgentTool 经 parent_session 读取运行时通道（thread_store / task_manager /
    // bg_event_sender / register / deregister / langfuse）与 frozen 数据回退，
    // SubAgentMiddleware 不再逐字段透传（管理权移出）。
    {
        let host = SubagentHost {
            thread_store: thread_persistence.store.clone(),
            task_manager: task_manager.clone(),
            bg_event_sender: Some(bg_event_tx),
            on_bg_complete: on_bg_complete.clone(),
            register_runtime: thread_persistence.register_runtime.clone(),
            deregister_runtime: thread_persistence.deregister_runtime.clone(),
            // SubAgent Langfuse bridge：注入工厂构造独立 LangfuseBridge 实例
            // （采样决策继承自父 agent）。
            langfuse_bridge: input.langfuse_bridge_factory.as_ref().map(|f| f()),
            // Frozen CLAUDE.local.md 不在 FrozenContext（父 session 无此字段），
            // 由 session/new 冻结数据注入（不重读磁盘）。
            frozen_claude_local_md: frozen_session
                .claude_local_md()
                .map(|s| Arc::new(s.to_string())),
            // 16_workflow 已删除（C2）：子面向 prompt 与主 prompt 字节相同；
            // 主 session 挂载 host 时恒 None（spawn 主路径从 parent session
            // 直接读取 frozen system_prompt，不经本字段）。
            frozen_system_prompt: None,
            parent_thread_id: thread_persistence.parent_thread_id.clone(),
            frozen_claude_md: Some(Arc::new(frozen_session.v2_frozen().claude_md.to_string())),
            frozen_skill_summary: Some(Arc::new(
                frozen_session.v2_frozen().skill_summary.to_string(),
            )),
            session_mcp_capability: input.session_mcp_capability.clone(),
        };
        session.set_subagent_host(host);
        // 父 v2 session 注入 SubAgentMiddleware（与 set_parent_agent_id 同点；
        // build_tool 必然晚于本调用，读到已 set 的 session）
        if let Some(mw) = subagent_mw {
            mw.set_parent_session(session.clone());
        }
    }
}
