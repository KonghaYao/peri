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
pub(super) fn parent_thread_id_of(parent: Option<&Arc<Session>>) -> Option<String> {
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
/// 并发限制：Agent 类后台任务不设上限（shell/workflow 的 kind 上限无关本路径），
/// 故不做入口预检，由注册阶段的 `register_with_kind` 如实返回注册失败——失败仅
/// 剩 session execution scope 关闭一类路径，错误语义与迁移前一致（"Failed to
/// register"）。预检（若有）位于调用方（llm_factory 之前），保证「预检 → 装配 →
/// 注册」的确定性窗口不被重复预检破坏（S3.1 幽灵任务回归测试依赖此结构）。
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
        tool_invocation_resolver,
        compact_config,
        context_budget,
        compact_llm,
        session_resources,
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
        parent_tool_call_id,
        cancel_token: cancel_token_cfg,
        cwd: cwd_cfg,
        parent_thread_id: parent_thread_id_cfg,
        frozen_claude_md: frozen_claude_md_cfg,
        frozen_claude_local_md: frozen_claude_local_md_cfg,
        frozen_skill_summary: frozen_skill_summary_cfg,
        frozen_date: frozen_date_cfg,
    } = config;

    // 注册失败由注册阶段兜底（register_with_kind 如实返回错误——Agent 类无并发
    // 上限，失败仅剩 scope 关闭一类），不在入口预检：保证并发竞态窗口内错误语义
    // 与迁移前一致（"Failed to register"，S3.1）。

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
        if let (Some(store), Some(parent_id)) = (&session_resources, &parent_thread_id) {
            // 继承来源是一次**一致快照**：canonical payload（含 reminder）与 flags 同一次
            // 读取，不再分 load_inherited_context / load_payloads / load_message_flags 三次。
            let snapshot = store.load_session_snapshot(parent_id).await?;
            let mut canonical = snapshot
                .inherited
                .payloads
                .into_iter()
                .map(|payload| (payload.id(), payload))
                .collect::<std::collections::HashMap<_, _>>();
            canonical.extend(
                snapshot
                    .payloads
                    .into_iter()
                    .map(|payload| (payload.id(), payload)),
            );
            for payload in &mut inherited.payloads {
                if let Some(original) = canonical.get(&payload.id()) {
                    *payload = original.clone();
                }
            }
            let inherited_ids = inherited
                .payloads
                .iter()
                .map(PersistedPayload::id)
                .collect::<std::collections::HashSet<_>>();
            inherited.flags = snapshot.inherited.flags;
            inherited.flags.extend(
                snapshot
                    .flags
                    .into_iter()
                    .filter(|(id, _)| inherited_ids.contains(id)),
            );
        } else if let Some(parent) = parent {
            // 无持久化路径（测试/遗留）：只能用父会话内存副本对齐 canonical 与 flags。
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
        }
    }

    // 4. 保存 child：一次 `save_child` 落父子关系、绑定继承、frozen 原字节与继承区。
    //    失败即整体失败——不再有 create + store_inherited + delete_thread 的手工补偿链
    //    （那正是「部分成功被当成已保存」的来源）。
    if let Some(ref store) = session_resources {
        let parent_id = parent_thread_id
            .clone()
            .ok_or("spawn_subagent: 持久化 child 需要 parent thread id（无父会话则无继承来源）")?;
        let snapshot = store.load_session_snapshot(&parent_id).await?;
        let binding = match &snapshot.binding {
            peri_acp_types::session_resources::BindingState::Bound(binding) => binding.clone(),
            other => {
                return Err(format!(
                    "spawn_subagent: parent session {parent_id} has no execution binding ({other:?}); child cannot inherit one"
                )
                .into())
            }
        };
        // child frozen 取不可变 parent/root 的**已持久化**字节，不重扫目录、不按当前日期重冻；
        // 门面会再校验它与 root 已保存快照逐字节相同。
        let frozen = match snapshot.frozen {
            peri_acp_types::session_resources::FrozenState::Present(bytes) => bytes,
            other => {
                return Err(format!(
                    "spawn_subagent: parent session {parent_id} has no persisted frozen snapshot ({other:?})"
                )
                .into())
            }
        };
        if snapshot.meta.cwd != cwd {
            return Err(peri_acp_types::workspace::WorkspaceError::ExecutionBindingMismatch.into());
        }
        let root_id = super::execution_root(store.as_ref(), &parent_id).await?;
        let snapshot_id = parent_messages.last().map(|m| m.id());
        let child = peri_acp_types::session_resources::ChildSnapshot {
            target: peri_acp_types::session_resources::NewSession {
                thread_id: child_thread_id.clone(),
                created_at: peri_time::now_utc_rfc3339(),
                meta: peri_acp_types::session_resources::NewSessionMeta {
                    title: Some(agent_name.clone()),
                    cwd: cwd.clone(),
                    parent_thread_id: Some(parent_id.clone()),
                    hidden: true,
                    cancel_policy,
                    snapshot_at_message_id: snapshot_id,
                },
                binding,
                frozen,
            },
            parent_id,
            root_id,
            inherited: inherited.clone(),
        };
        store.save_child(&child).await?;
    }

    // 5. 构造子 session + 链装配 + v2_ctx（共享 helper [build_subagent_session_v2]：
    //    frozen 从父 copy 不重读磁盘，transcript 恢复只读 inherited snapshot 后绑定存储）
    //    身份（H1/M3）：子 `FrozenContext.system_prompt` = system_builder 的子能力
    //    投影字节（定义型带 overrides / fork 无 overrides）——不复制父字节；
    //    注入 prompt 留在本函数，不进 helper（D1）
    let frozen = inherited_frozen_context(
        parent,
        system_prompt.as_deref(),
        &frozen_claude_md,
        &frozen_skill_summary,
        &frozen_date,
    );
    let (session, v2_ctx) = build_subagent_session_v2(
        cwd.clone(),
        frozen,
        cancel_token.clone(),
        child_thread_id.clone(),
        parent.and_then(|session| session.subagent_host()),
        session_resources.clone(),
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
        compact_config,
        context_budget,
        compact_llm,
        Some(agent_id_from_child_thread(&child_thread_id)),
        // 身份随 transcript 持久化（见 6b），故开启定向吸收：请求投影按内容相等
        // 丢弃该条，身份在请求面只由 bridge base system 出现一次。
        true,
    )
    .await?;

    // 父上下文已作为只读 ancestor 装载；不可用原 ID append 到 child messages。
    //
    // 6b（H1/M3 + 身份持久化）：子身份同时落在两处，各司其职——
    // - `FrozenContext.system_prompt`：bridge base system，负责**每次请求**的注入；
    // - transcript 起始处的 System 消息：负责**持久化**。执行恢复 metadata 通道
    //   已随 recovery 移除，transcript 是本分支上身份唯一的持久事实；恢复路径
    //   （见 `resume.rs`）不再注入身份，而是读回这条历史，避免重注入。
    //   归一化吸收（上方 `normalize_persisted_identity`）保证请求面仍恰一次，
    //   transcript 本体不被改写、历史保持可读。
    // - fork 路径：位于 parent_messages/ancestor 之后（身份在对话上下文之后）。
    if let Some(identity) = system_prompt.as_deref() {
        let transcript = session.transcript();
        let mut guard = transcript.write();
        guard.append(BaseMessage::system(identity));
    }

    // 6c. push prompt 到 queue（fork 路径套 fork directive 模板）
    let prompt_message = match fork_directive_kind {
        Some(ForkDirectiveKind::Fork) => build_fork_directive(&prompt),
        Some(ForkDirectiveKind::Bg) => build_bg_fork_directive(&prompt),
        None => prompt.clone(),
    };
    let queued = QueuedMessage::new(
        MessageKind::Prompt,
        MessageSource::UserInput,
        BaseMessage::human(prompt_message),
    );
    v2_ctx.context.session.queue.push(queued);

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
                session_resources,
                register_runtime,
                deregister_runtime,
                langfuse_bridge,
                parent_agent_id,
                parent_tool_call_id,
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
                super::completion_delivery(parent, on_bg_complete),
                langfuse_bridge,
                session_resources,
                deregister_runtime,
                on_subagent_start,
                on_subagent_stop,
                register_runtime,
                parent_agent_id,
                parent_tool_call_id,
                cancel_token.clone(),
                v2_ctx,
                None,
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
