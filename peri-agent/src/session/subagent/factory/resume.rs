//! Existing child-thread claim, history reconstruction and pre-run rollback.

use std::sync::Arc;
use std::time::Duration;

use peri_acp_types::messages::MessageId;
use peri_acp_types::session::MqFilter;
use peri_acp_types::store::PersistedPayload;

use crate::messages::BaseMessage;
use crate::session::queue::{MessageKind, MessageQueue, MessageSource, QueuedMessage};

use crate::session::subagent::factory::claim::ResumeClaim;

use crate::session::subagent::types::{SubagentResumeConfig, SubagentSpawned};

use crate::session::Session;

// ─── 恢复（统一入口 resume_subagent） ───────────────────────────────────────

/// 恢复子 agent（统一入口实现，slice 4：重建 + 执行）。
///
/// 两层校验（不通过返回明确 Err，与 issue 验收一致）：
/// 1. 存在性：`load_meta` 失败/不存在 → `thread not found`
/// 2. status：`agent_status == Active`（可能未正常收尾）→ 拒绝恢复
///
/// bound 子会话必须与调用者属于同一根会话及工作区，兄弟子会话可互相恢复。
/// legacy 无绑定入口保留旧行为；新执行权不能仅由持有 child_thread_id 推断。
///
/// 校验 → 置 active 段整体持锁（R-M1：防并发双 resume 双执行同一 thread）；
/// 锁内仅 load_meta + update_thread_status（无嵌套锁，不 await run_react_loop）。
/// 锁释放后重建：
/// - 分别装载 inherited/own payload 与 flags；**仅当 own 末条**含未配对 tool_calls 的 AI 时
///   pop（R2-MID-1：禁止从后往前找 AI；pop 后其后无消息，无孤儿 Tool 可清理）
/// - cwd 取 `meta.cwd`（thread 创建时固化，进程重启后不得改用父 cwd）
/// - frozen 从父 session copy（ARC-FROZEN-001；parent None 用 config 回退）
/// - cancel token：Cascade 从父 token 优先、config fallback 派生；Independent
///   恒新建 token（不复用父或 config token）
/// - **不注入** parent_messages / identity System / skill_names（F4 / R-H1：
///   旧 transcript 已含首轮注入内容，重复注入会重复）
/// - prompt 入队：`Some(p)` 原样追加（不套 fork directive）；`None` 注入隐式
///   continue 常量（issue 决策 9）
/// - 执行恒为后台（Issue #450）：新 task_id + `spawn_background_subagent`
///
/// 重建/装配失败（load_messages 失败）时回滚 status 至原值（R-M1），防 thread
/// 永久停留 active（R-M4 崩溃遗留的镜像问题）。执行开始后的失败走后台收尾路径
/// （error / cancelled），不回滚。
/// 准备阶段调用被 drop 时，由 claim worker 在 active 写完成后异步回滚。
pub(super) async fn resume_subagent_impl(
    parent: Option<&Arc<Session>>,
    config: SubagentResumeConfig,
) -> Result<SubagentSpawned, Box<dyn std::error::Error + Send + Sync>> {
    if parent.is_some_and(|session| session.store().frozen.legacy_embedded_instructions) {
        return Err("V1 frozen prompt contains embedded external instructions; create a new session before resuming a subagent".into());
    }
    // 解构 config（cwd 不用于恢复——cwd 取 meta.cwd，thread 创建时固化）
    let SubagentResumeConfig {
        thread_id,
        prompt,
        agent_name: agent_name_cfg,
        max_iterations,
        llm,
        chain_assembler,
        tools,
        tool_filter,
        tool_invocation_resolver,
        compact_config,
        context_budget,
        compact_llm,
        session_resources,
        event_handler: _,
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
        cwd: _,
        frozen_claude_md: frozen_claude_md_cfg,
        frozen_claude_local_md: frozen_claude_local_md_cfg,
        frozen_skill_summary: frozen_skill_summary_cfg,
        frozen_date: frozen_date_cfg,
    } = config;

    // 绑定校验只读 binding：不再为预检物化 child/parent 的有效正文。
    // 注意：child 与 parent 是两次独立读取，不构成跨会话原子快照；这里只做
    // 分类校验（存在性/绑定/同根）。执行正文由 claim 成功后唯一一次
    // `load_active_snapshot` 读取，不能拿预检结果直接执行。
    // 存在性是本函数的第一个校验分支：binding 读取报 `NotFound` 即「thread not found」，
    // 与后续 `ResumeClaim` 的状态校验分开报错（不能把「不存在」说成「仍处于运行态」）。
    let binding = match session_resources.load_session_binding(&thread_id).await {
        Ok(peri_acp_types::session_resources::BindingState::Bound(binding)) => Some(binding),
        // 无绑定（legacy / 外来登记 / 缺失）保留旧行为：不当作 bound child 处理。
        Ok(_) => None,
        Err(error)
            if matches!(
                error.kind(),
                peri_acp_types::session_resources::SessionResourceErrorKind::NotFound
            ) =>
        {
            // 先判格式再判存在：非 UUID 不是「不存在」，与 `validate_thread` 同判据。
            crate::session::subagent::factory::validate_thread_id_format(&thread_id)?;
            return Err(format!("resume_subagent: thread not found: {thread_id}").into());
        }
        Err(error) => return Err(error.into()),
    };
    if binding.is_some() {
        let parent_id = crate::session::subagent::factory::spawn::parent_thread_id_of(parent)
            .ok_or("bound child resume requires its owning parent session")?;
        let parent_binding = match session_resources.load_session_binding(&parent_id).await? {
            peri_acp_types::session_resources::BindingState::Bound(binding) => binding,
            _ => return Err("bound subagent parent has no execution binding".into()),
        };
        if binding.as_ref() != Some(&parent_binding) {
            return Err(peri_acp_types::workspace::WorkspaceError::ExecutionBindingMismatch.into());
        }
        if crate::session::subagent::factory::execution_root(session_resources.as_ref(), &thread_id)
            .await?
            != crate::session::subagent::factory::execution_root(
                session_resources.as_ref(),
                &parent_id,
            )
            .await?
        {
            return Err("bound subagent belongs to another root session execution owner".into());
        }
    }
    let preparation = parent
        .and_then(|session| session.subagent_host())
        .and_then(|host| {
            host.task_manager
                .as_ref()
                .and_then(|manager| manager.child_owner(&thread_id))
        })
        .map(|owner| owner.prepare_resume())
        .transpose()?;
    let ownership = task_manager
        .as_ref()
        .map(|manager| {
            peri_acp_types::tasks::TaskManager::begin_external_execution(
                manager.as_ref(),
                "subagent-resume",
            )
        })
        .transpose()?;
    let cluster_root =
        crate::session::subagent::factory::execution_root(session_resources.as_ref(), &thread_id)
            .await?;
    let (meta, mut claim) = ResumeClaim::acquire(
        Arc::clone(&session_resources),
        thread_id.clone(),
        cluster_root,
        ownership,
    )
    .await?;

    let completion_target = crate::session::subagent::factory::completion_target(
        parent,
        on_bg_complete.as_ref(),
        crate::session::subagent::factory::spawn::parent_thread_id_of(parent),
    );
    // The claim remains owned across history I/O and session construction. A
    // dropped caller closes its decision channel, leaving ordered rollback to
    // the worker that performed the active write.
    let restored = async {
        crate::session::subagent::factory::persist_completion_target(
            session_resources.as_ref(),
            &thread_id,
            completion_target.as_deref(),
        )
        .await
        .map_err(|error| anyhow::anyhow!("{error}"))?;
        // 一次一致快照：inherit 区与自有 payload/flags 同一次读取（不再分三次）。
        // 恢复只重建有效视图：已排除 own 正文不属于续聊的当前上下文。
        let snapshot = session_resources
            .load_active_snapshot(&thread_id)
            .await
            .map_err(|error| anyhow::anyhow!("{error}"))?;
        let mut inherited = snapshot.inherited;
        let own = snapshot.payloads;
        let ancestor_ids = inherited
            .payloads
            .iter()
            .map(PersistedPayload::id)
            .collect::<std::collections::HashSet<_>>();
        let own_ids = own
            .iter()
            .map(PersistedPayload::id)
            .collect::<std::collections::HashSet<_>>();
        if own_ids.iter().any(|id| ancestor_ids.contains(id)) {
            anyhow::bail!("inherited context overlaps child own history");
        }
        inherited.flags.extend(
            snapshot
                .flags
                .into_iter()
                .filter(|(id, _)| own_ids.contains(id)),
        );
        Ok::<_, anyhow::Error>((inherited, own))
    }
    .await;
    let (inherited, mut loaded) = match restored {
        Ok(history) => history,
        Err(error) => {
            let rollback = claim.rollback().await;
            let suffix = rollback.err().map(|e| format!("; {e}")).unwrap_or_default();
            return Err(format!(
                "resume_subagent: failed to load messages for {}: {}{}",
                thread_id, error, suffix
            )
            .into());
        }
    };
    if loaded.last().is_some_and(|payload| {
        payload
            .as_message()
            .is_some_and(BaseMessage::has_tool_calls)
    }) {
        loaded.pop();
    }

    // 2. cwd 取 meta.cwd（thread 创建时固化的；进程重启后不得改用父 cwd）。
    //    绑定身份已在上方与 owning parent 比对相等，工作区一致性随该绑定成立。
    let cwd = meta.cwd.clone();

    // 3. frozen 从父 session copy（ARC-FROZEN-001：不重读磁盘；parent None 用
    //    config 回退，与 spawn 的父侧解析一致）
    let frozen_claude_md = parent
        .map(|p| p.store().frozen.claude_md.to_string())
        .or(frozen_claude_md_cfg);
    let frozen_skill_summary = parent
        .map(|p| p.store().frozen.skill_summary.to_string())
        .or(frozen_skill_summary_cfg);
    let frozen_date = parent
        .map(|p| p.store().frozen.date.to_string())
        .or(frozen_date_cfg);
    // Spawn puts the child's identity first in its own persisted history. Only
    // hidden child threads use that convention; legacy/non-child history has no
    // typed identity marker and its first System must remain ordinary history.
    // For older hidden children, the first own System is the compatible identity
    // slot. A future typed payload can remove this historical convention.
    let identity = (meta.hidden && meta.parent_thread_id.is_some())
        .then(|| loaded.first().and_then(PersistedPayload::as_message))
        .flatten()
        .and_then(|message| {
            matches!(message, BaseMessage::System { .. }).then(|| message.content().to_owned())
        });
    let frozen = crate::session::subagent::factory::context::inherited_frozen_context(
        parent,
        identity.as_deref(),
        frozen_claude_md.as_deref(),
        frozen_skill_summary.as_deref(),
        frozen_date.as_deref(),
    );

    // 4. cancel token：Cascade = 父 cancel 传播（parent 优先，回退 config 注入的
    //    父 token；均缺失时新建——与 spawn 的 :466-472 完全对齐，review low-2），
    //    Independent = 恒新建（忽略 config 注入的 token——复用会让父 cancel
    //    波及 Independent 子任务，与 spawn :471 对齐，review low-1）
    let cancel_token = crate::session::subagent::factory::context::derive_cancel_token(
        parent,
        cancel_token_cfg,
        meta.cancel_policy,
    );

    // 5. agent_name：config 优先，回退 meta.title，最后兜底 "subagent"
    let agent_name = agent_name_cfg
        .or_else(|| meta.title.clone())
        .unwrap_or_else(|| "subagent".to_string());

    // 6. 重建 session（thread_id 固定 = config.thread_id；ancestor/own 显式分区，
    //    两区 flags 均恢复后绑定持久化——helper 内）
    //    不注入 parent_messages / identity System / skill_names（F4 / R-H1）
    let (session, mut v2_ctx) =
        crate::session::subagent::factory::context::build_subagent_session_v2(
            cwd.clone(),
            frozen,
            cancel_token.clone(),
            thread_id.clone(),
            parent.and_then(|session| session.subagent_host()),
            Some(Arc::clone(&session_resources)),
            inherited,
            loaded,
            llm,
            chain_assembler,
            tools,
            tool_filter,
            parent
                .and_then(|session| session.subagent_host())
                .and_then(|host| host.session_mcp_catalog.clone()),
            Vec::new(), // skill_names 恒空（R-H1：恢复不重复注入 SkillPreload）
            frozen_claude_md,
            frozen_claude_local_md_cfg,
            frozen_skill_summary,
            tool_invocation_resolver,
            compact_config,
            context_budget,
            compact_llm,
            Some(crate::session::subagent::v2_bridge::agent_id_from_child_thread(&thread_id)),
            identity.is_some(),
        )
        .await?;
    let parent_thread = crate::session::subagent::factory::spawn::parent_thread_id_of(parent);
    v2_ctx.context.session.model_parent =
        crate::agent::model_call::capture_tool_parent(parent_tool_call_id.as_deref())
            .filter(|parent| Some(parent.owner.as_str()) == parent_thread.as_deref());

    // 7. prompt 入队：Some(p) 原样追加（不套 fork directive——恢复目标仍是原
    //    任务，直接追加指令）；None 注入隐式 continue 常量（issue 决策 9）
    let prompt_text = prompt.unwrap_or_else(|| IMPLICIT_CONTINUE_PROMPT.to_string());
    let prompt_message = QueuedMessage::new(
        MessageKind::Prompt,
        MessageSource::UserInput,
        BaseMessage::human(prompt_text.clone()),
    );
    // 注册失败时要撤回的就是这一行；稳定 ID 必须在入队移动前取出。
    let prompt_message_id = prompt_message.id();
    let prompt_queue = v2_ctx.context.session.queue.clone();
    if let Err(error) = v2_ctx
        .context
        .session
        .queue
        .enqueue_sent(prompt_message)
        .await
    {
        // 没有接纳到初始指令的恢复不得继续跑：先撤销本次认领，再把失败交给调用方。
        let detail = format!("resume prompt enqueue failed: {error}");
        if let Err(rollback) = claim.rollback().await {
            return Err(format!("{detail}; resume claim rollback failed: {rollback}").into());
        }
        return Err(detail.into());
    }

    // 8. 执行（恒后台，Issue #450）
    // 认领已写入 Active；准备门在进入执行前释放，避免与本次 run 的
    // 执行 permit 互斥（并发的第二次 resume 由 claim/status 拒绝）。
    drop(preparation);
    // 终态结果投递到父会话的 MQ（无父 thread 时不做跨会话投递）。
    let terminal_route = completion_target.map(|target| {
        crate::session::subagent::terminal_settlement::TerminalMqRoute {
            store: Arc::clone(&session_resources),
            target: peri_acp_types::thread::ThreadId::from(target.as_str()),
        }
    });
    // slice 5：后台恢复——生成新 task_id、TaskManager 注册（参数与 spawn
    // 调用点对齐；prompt 传实际注入文本——用户 prompt 或 continue 常量，
    // 仅用于 prompt_summary 展示，R2-LOW-3）
    let task_id = format!("bg-{}", uuid::Uuid::now_v7());
    match crate::session::subagent::background::spawn_background_subagent(
        task_id.clone(),
        thread_id.clone(),
        agent_name.clone(),
        prompt_text,
        cwd,
        max_iterations,
        bg_event_sender,
        task_manager,
        crate::session::subagent::factory::completion_delivery(parent, on_bg_complete),
        langfuse_bridge,
        Some(Arc::clone(&session_resources)),
        terminal_route,
        deregister_runtime,
        on_subagent_start,
        on_subagent_stop,
        register_runtime,
        parent_agent_id,
        parent_tool_call_id,
        cancel_token.clone(),
        v2_ctx,
        Some(&mut claim),
    )
    .await
    {
        Ok(()) => {}
        Err(e) => {
            // 失败补偿必须是不可拆分的整体：owned task 先确认撤回本次已接纳的
            // prompt，再回滚认领。调用方取消只丢弃 JoinHandle，不会留下
            // 「状态已回滚、prompt 仍在队列」的重试重复投递窗口。
            let compensation = tokio::spawn(compensate_failed_resume(
                prompt_queue,
                prompt_message_id,
                claim,
            ));
            let suffix = match tokio::time::timeout(RESUME_COMPENSATION_WAIT, compensation).await {
                Ok(Ok(Ok(()))) => String::new(),
                Ok(Ok(Err(rollback))) => format!("; {rollback}"),
                Ok(Err(join)) => format!("; resume compensation task failed: {join}"),
                // 补偿仍由 owned task 持有认领并在后台重试；调用方只报告未结清。
                Err(_) => "; resume compensation is still running".to_string(),
            };
            return Err(format!("resume_subagent: thread {}: {}{}", thread_id, e, suffix).into());
        }
    }
    Ok(SubagentSpawned {
        child_thread_id: thread_id,
        task_id,
        session,
        cancel_token,
    })
}

/// 失败补偿的调用方等待上限；超时后 owned task 继续持有认领并重试撤回。
const RESUME_COMPENSATION_WAIT: Duration = Duration::from_secs(10);
/// 撤回重试的初始/上限间隔；只控制频率，不改变「未确认不继续」的次序。
const WITHDRAW_RETRY_INITIAL: Duration = Duration::from_millis(50);
const WITHDRAW_RETRY_MAX: Duration = Duration::from_secs(1);

/// 失败派发的 owned 补偿：先撤回本次 prompt，再回滚认领。
///
/// 两步由同一个任务顺序执行，调用方取消不会把它们拆开；撤回未确认提交时
/// `rollback` 不会运行，认领保持 Active，避免重试重复投递。
async fn compensate_failed_resume(
    queue: MessageQueue,
    prompt_id: MessageId,
    claim: ResumeClaim,
) -> Result<(), String> {
    withdraw_admitted_prompt(&queue, prompt_id).await;
    claim.rollback().await
}

/// 撤回已接纳的 prompt，直到结果可判定。
///
/// `mq_cancel` 在各后端都以单次事务删除：内存后端同步删除；SQLite 与远端在提交
/// 失败时可能只报告「未知」。因此一次 `Err` 既不能当作「行还在」，也不能当作
/// 「行已删」；重查 MQ 后行已消失才算撤回完成，仍在则按退避重试。
async fn withdraw_admitted_prompt(queue: &MessageQueue, prompt_id: MessageId) {
    let mut delay = WITHDRAW_RETRY_INITIAL;
    loop {
        match queue.cancel_user_input(&prompt_id).await {
            Ok(_) => return,
            Err(error) => {
                match queue.query(MqFilter::default()).await {
                    Ok(rows) if !rows.iter().any(|row| row.id == prompt_id) => return,
                    Ok(_) => {}
                    Err(query_error) => {
                        tracing::warn!(
                            %query_error,
                            "resume compensation could not verify prompt withdrawal"
                        );
                    }
                }
                tracing::warn!(%error, "resume prompt withdrawal failed; retrying");
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(WITHDRAW_RETRY_MAX);
            }
        }
    }
}

/// 隐式 continue 指令（prompt 缺省时注入，issue 决策 9）
const IMPLICIT_CONTINUE_PROMPT: &str = "Continue your previous task where you left off.";
