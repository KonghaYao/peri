//! New child-thread creation and first-run message injection.

use std::sync::Arc;

use peri_acp_types::store::{InheritedContext, PersistedPayload};

use super::super::background::spawn_background_subagent;
use super::super::directives::{build_bg_fork_directive, build_fork_directive};
use super::super::run_sync::run_sync_subagent;
use super::super::types::{
    ForkDirectiveKind, SubagentRunMode, SubagentSpawnConfig, SubagentSpawned,
};
use super::super::v2_bridge::agent_id_from_child_thread;
use super::context::{build_subagent_session_v2, derive_cancel_token, inherited_frozen_context};
use crate::messages::BaseMessage;
use crate::session::queue::{MessageKind, MessageSource, QueuedMessage};
use crate::session::Session;
use crate::thread::ThreadMeta;

/// 父线程 ID 解析——spawn 写盘的**唯一取值点**（挂父子链）：
/// - 优先 parent session 的 `store().thread_id`：subagent 层 session 构造时以
///   child_thread_id 注入，恒为 `Some`（孙 agent 链命中此值）；
/// - 回退 `SubagentHost.parent_thread_id`：TUI 主 agent 的 `store().thread_id`
///   恒为 `None`（stage_builder 构造主 session 不传 thread_id，`SessionStore`
///   无 setter），executor 以 `ctx.thread_id` 注入 host
///   （`ThreadPersistence.parent_thread_id` → stage_builder → host）。
///
/// parent 为 `None` 时返回 `None`：spawn 侧继续走 `parent_thread_id_cfg` 回退。
/// （resume 路径不再做 parent 链校验——该解析链路在生产路径与写盘值常有
/// 偏差，误判拒绝；resume 仅以 thread_id 存在性 / status 为准）
fn parent_thread_id_of(parent: Option<&Arc<Session>>) -> Option<String> {
    parent
        .and_then(|p| p.store().thread_id.clone())
        .or_else(|| parent.and_then(|p| p.subagent_host().and_then(|h| h.parent_thread_id.clone())))
}

/// 启动子 agent（统一创建入口实现，L3）。
///
/// 流程（与迁移前四条路径语义一致）：
/// 1. 生成 child_thread_id / task_id
/// 2. 解析父侧数据（parent 优先；frozen copy 自 parent session，不重读磁盘）
/// 3. 创建子线程（thread_store Some 时；parent_thread_id 挂父子链）
/// 4. 构造子 session（frozen copy + transcript with_persistence 绑定存储）
/// 5. 注入 parent_messages / system_prompt 到 transcript，push prompt 到 queue
/// 6. 经 chain_assembler 装配子链（frozen 注入链上下文），构造 StageContext
/// 7. Sync：直接 run_react_loop；Background：tokio::spawn + TaskManager 注册
/// 8. 收尾：update_thread_status（done/cancelled/error）+ 事件 + hook 闭包
///
/// 并发限制（Background 最多 3 个活跃任务）：不做入口预检，由注册阶段的
/// `register_with_kind`（per-kind 上限）如实返回注册失败——与迁移前一致，
/// 预检（若有）位于调用方（llm_factory 之前），保证「预检 → 装配 → 注册」
/// 的确定性窗口不被重复预检破坏（S3.1 幽灵任务回归测试依赖此结构）。
#[allow(clippy::too_many_arguments)]
pub(super) async fn spawn_subagent_impl(
    parent: Option<&Arc<Session>>,
    config: SubagentSpawnConfig,
) -> Result<SubagentSpawned, Box<dyn std::error::Error + Send + Sync>> {
    // 解构 config：字段分散使用，避免部分 move 后整体借用冲突
    let SubagentSpawnConfig {
        agent_name,
        prompt,
        parent_messages,
        cancel_policy,
        max_iterations,
        fork_directive_kind,
        run_mode,
        skill_names,
        llm,
        chain_assembler,
        tools,
        tool_filter,
        system_prompt,
        error_suggest_registry,
        tool_registry_snapshot,
        tool_invocation_resolver,
        compact_config,
        context_budget,
        compact_llm,
        thread_store,
        event_handler,
        bg_event_sender,
        task_manager,
        on_bg_complete,
        langfuse_bridge,
        on_subagent_start,
        on_subagent_stop,
        register_runtime,
        deregister_runtime,
        parent_agent_id,
        cancel_token: cancel_token_cfg,
        cwd: cwd_cfg,
        parent_thread_id: parent_thread_id_cfg,
        frozen_claude_md: frozen_claude_md_cfg,
        frozen_claude_local_md: frozen_claude_local_md_cfg,
        frozen_skill_summary: frozen_skill_summary_cfg,
        frozen_date: frozen_date_cfg,
    } = config;

    // 并发限制由注册阶段兜底（register_with_kind per-kind 上限，错误如实返回），
    // 不在入口预检：middlewares 路径的预检位于 llm_factory 之前（execute_bg.rs），
    // 保证并发竞态窗口内错误语义与迁移前一致（"Failed to register"，S3.1）。

    // 2. 生成标识符
    let child_thread_id = uuid::Uuid::now_v7().to_string();
    let task_id = format!("bg-{}", uuid::Uuid::now_v7());

    // 3. 父侧数据解析（parent 优先；frozen data 从父 session copy）
    let cwd = parent
        .map(|p| p.store().cwd.to_string())
        .or(cwd_cfg)
        .ok_or("spawn_subagent: cwd 未提供（parent 缺失且 config.cwd 为 None）")?;
    let parent_thread_id = parent_thread_id_of(parent).or(parent_thread_id_cfg);
    let frozen_claude_md = parent
        .map(|p| p.store().frozen.claude_md.to_string())
        .or(frozen_claude_md_cfg);
    let frozen_skill_summary = parent
        .map(|p| p.store().frozen.skill_summary.to_string())
        .or(frozen_skill_summary_cfg);
    let frozen_date = parent
        .map(|p| p.store().frozen.date.to_string())
        .or(frozen_date_cfg);
    let frozen_claude_local_md = frozen_claude_local_md_cfg;

    // cancel token：Cascade = 父 cancel 传播（parent 优先，回退 config 注入的
    // 父 token；均缺失时新建），Independent = 新建（与迁移前语义一致）
    let cancel_policy = cancel_policy.as_cancel_policy();
    let cancel_token = derive_cancel_token(parent, cancel_token_cfg, cancel_policy);

    let mut inherited = InheritedContext {
        payloads: parent_messages
            .iter()
            .cloned()
            .map(PersistedPayload::Message)
            .collect(),
        flags: Default::default(),
    };
    if !inherited.payloads.is_empty() {
        if let Some(parent) = parent {
            let transcript = parent.transcript();
            let transcript = transcript.read();
            let canonical = transcript
                .persisted_payloads()
                .into_iter()
                .map(|payload| (payload.id(), payload))
                .collect::<std::collections::HashMap<_, _>>();
            for payload in &mut inherited.payloads {
                if let Some(original) = canonical.get(&payload.id()) {
                    *payload = original.clone();
                }
            }
            inherited.flags = inherited
                .payloads
                .iter()
                .filter_map(|payload| {
                    transcript
                        .get_flags(payload.id())
                        .map(|flags| (payload.id(), flags))
                })
                .collect();
        } else if let (Some(store), Some(parent_id)) = (&thread_store, &parent_thread_id) {
            let parent_context = store.load_inherited_context(parent_id).await?;
            let mut canonical = parent_context
                .payloads
                .into_iter()
                .map(|payload| (payload.id(), payload))
                .collect::<std::collections::HashMap<_, _>>();
            canonical.extend(
                store
                    .load_payloads(parent_id)
                    .await?
                    .into_iter()
                    .map(|payload| (payload.id(), payload)),
            );
            for payload in &mut inherited.payloads {
                if let Some(original) = canonical.get(&payload.id()) {
                    *payload = original.clone();
                }
            }
            inherited.flags = parent_context.flags;
            inherited
                .flags
                .extend(store.load_message_flags(parent_id).await?);
            let ids = inherited
                .payloads
                .iter()
                .map(PersistedPayload::id)
                .collect::<std::collections::HashSet<_>>();
            inherited.flags.retain(|id, _| ids.contains(id));
        }
    }

    // 4. 创建子线程（thread_store Some 时；None 跳过落库——仅测试/遗留路径）
    if let Some(ref store) = thread_store {
        let snapshot_id = parent_messages.last().map(|m| m.id().as_uuid().to_string());
        let mut child_meta = ThreadMeta::new(&cwd);
        child_meta.id = child_thread_id.clone();
        child_meta.parent_thread_id = parent_thread_id.clone();
        child_meta.snapshot_at_message_id = snapshot_id;
        child_meta.hidden = true;
        child_meta.cancel_policy = cancel_policy;
        child_meta.title = Some(agent_name.clone());
        store
            .create_thread(child_meta)
            .await
            .map_err(|e| format!("Failed to create child thread: {}", e))?;
        if let Err(error) = store
            .store_inherited_context(&child_thread_id, &inherited)
            .await
        {
            let cleanup = store.delete_thread(&child_thread_id).await;
            return Err(format!(
                "Failed to persist child inherited context: {error}; cleanup: {cleanup:?}"
            )
            .into());
        }
    }

    // 5. 构造子 session + 链装配 + v2_ctx（共享 helper [build_subagent_session_v2]：
    //    frozen 从父 copy 不重读磁盘，transcript 恢复只读 inherited snapshot 后绑定存储）
    //    注入 parent_messages / system_prompt / prompt 留在本函数——spawn 与
    //    resume 的消息注入差异大，不进 helper（D1）
    let frozen = inherited_frozen_context(
        parent,
        &frozen_claude_md,
        &frozen_skill_summary,
        &frozen_date,
    );
    let (session, v2_ctx) = build_subagent_session_v2(
        cwd.clone(),
        frozen,
        cancel_token.clone(),
        child_thread_id.clone(),
        thread_store.clone(),
        inherited,
        Vec::new(), // 新 child 没有 own history
        llm,
        chain_assembler,
        tools,
        tool_filter,
        parent
            .and_then(|session| session.subagent_host())
            .and_then(|host| host.session_mcp_capability.clone()),
        skill_names,
        frozen_claude_md,
        frozen_claude_local_md,
        frozen_skill_summary,
        tool_invocation_resolver,
        error_suggest_registry,
        tool_registry_snapshot,
        compact_config,
        context_budget,
        compact_llm,
        Some(agent_id_from_child_thread(&child_thread_id)),
    );

    let transcript = session.transcript();

    // 父上下文已作为只读 ancestor 装载；不可用原 ID append 到 child messages。

    // 6b. SubAgent system_prompt（身份构建）注入到 transcript 开头位置：
    // - fork 路径：在 parent_messages 之后（让身份提示词位于对话上下文之后、
    //   prompt 之前——SubAgent 的 prompt 由下方 push 到 queue，Receive 阶段追加）
    // - 非 fork 路径：parent_messages 为空，直接 append 到 transcript 开头
    //
    // 注意：这是 session 起始身份构建（在 run_react_loop 调用前注入），不是中途纠正，
    // 用 BaseMessage::System 合法（CLAUDE.md TRAP 仅禁止中途纠正用 System）。
    if let Some(sp) = system_prompt {
        let mut tx = transcript.write();
        tx.append(BaseMessage::system(sp));
    }

    // 6c. push prompt 到 queue（fork 路径套 fork directive 模板）
    let prompt_message = match fork_directive_kind {
        Some(ForkDirectiveKind::Fork) => build_fork_directive(&prompt),
        Some(ForkDirectiveKind::Bg) => build_bg_fork_directive(&prompt),
        None => prompt.clone(),
    };
    v2_ctx.context.session.queue.push(QueuedMessage::new(
        MessageKind::Prompt,
        MessageSource::UserInput,
        BaseMessage::human(prompt_message),
    ));

    match run_mode {
        SubagentRunMode::Sync => {
            let interrupted = run_sync_subagent(
                &child_thread_id,
                &agent_name,
                &cwd,
                max_iterations,
                event_handler,
                on_subagent_start,
                on_subagent_stop,
                thread_store,
                register_runtime,
                deregister_runtime,
                langfuse_bridge,
                parent_agent_id,
                v2_ctx,
                session.clone(),
                None,
            )
            .await?;
            Ok(SubagentSpawned {
                child_thread_id,
                task_id: None,
                session,
                cancel_token,
                interrupted,
            })
        }
        SubagentRunMode::Background => {
            let task_id_clone = task_id.clone();
            spawn_background_subagent(
                task_id.clone(),
                child_thread_id.clone(),
                agent_name.clone(),
                prompt,
                cwd.clone(),
                max_iterations,
                bg_event_sender,
                task_manager,
                on_bg_complete,
                langfuse_bridge,
                thread_store,
                deregister_runtime,
                on_subagent_start,
                on_subagent_stop,
                register_runtime,
                parent_agent_id,
                cancel_token.clone(),
                v2_ctx,
            )
            .await?;
            Ok(SubagentSpawned {
                child_thread_id,
                task_id: Some(task_id_clone),
                session,
                cancel_token,
                interrupted: false,
            })
        }
    }
}
