//! Existing child-thread claim, history reconstruction and pre-run rollback.

use std::sync::Arc;

use peri_acp_types::session_resources::{work::WorkQuery, FrozenState};
use peri_acp_types::store::PersistedPayload;
use sha2::{Digest, Sha256};

use super::super::background::spawn_background_subagent;
use super::super::run_sync::run_sync_subagent;
use super::super::types::{SubagentResumeConfig, SubagentRunMode, SubagentSpawned};
use super::super::v2_bridge::agent_id_from_child_thread;
use super::claim::ResumeClaim;
use super::context::{build_subagent_session_v2, derive_cancel_token};
use crate::messages::BaseMessage;
use crate::session::queue::{MessageKind, MessageSource, QueuedMessage};
use crate::session::Session;
use crate::tools::{EffectiveToolError, EffectiveToolErrorCode};

fn authorization_denied(message: &str) -> EffectiveToolError {
    EffectiveToolError::new(EffectiveToolErrorCode::PermissionDenied, message)
}

fn preparation_failed(message: impl Into<String>) -> EffectiveToolError {
    EffectiveToolError::new(EffectiveToolErrorCode::ApplicationFailed, message)
}

fn preparation_rejected(
    child_thread_id: &str,
    error: EffectiveToolError,
) -> Box<dyn std::error::Error + Send + Sync> {
    let reason = match error.code {
        EffectiveToolErrorCode::PermissionDenied => {
            "resume authorization or capability validation failed"
        }
        EffectiveToolErrorCode::InvalidInput => "resume target identity is invalid or absent",
        _ => "resume preparation evidence or state is unavailable",
    };
    tracing::warn!(
        child_thread_id,
        code = error.code.as_str(),
        reason,
        "resume preparation rejected"
    );
    Box::new(error)
}

fn rollback_unknown(child_thread_id: &str) {
    tracing::error!(
        child_thread_id,
        code = "UNKNOWN",
        reason = "resume preparation rollback is unconfirmed",
        "resume claim remains uncertain"
    );
}

// ─── 恢复（统一入口 resume_subagent） ───────────────────────────────────────

/// 恢复子 agent（统一入口实现，slice 4：重建 + 执行）。
///
/// 两层校验（不通过返回明确 Err，与 issue 验收一致）：
/// 1. 存在性：`load_meta` 失败/不存在 → `thread not found`
/// 2. status：`agent_status == Active`（可能未正常收尾）→ 拒绝恢复
///
/// bound 子会话必须与调用者属于同一根会话及工作区，兄弟子会话可互相恢复。
/// 缺少已保存授权与 frozen 证据的历史会话不可重新执行。
/// 保存的 authorization_ref 证明原委托的 ceiling；本次 trusted invocation
/// 授权新的输入投递，可以来自不同 admission，但不得改写保存的 ceiling。
///
/// 校验 → 置 active 段整体持锁（R-M1：防并发双 resume 双执行同一 thread）；
/// 锁内仅 load_meta + update_thread_status（无嵌套锁，不 await run_react_loop）。
/// 锁释放后重建：
/// - 分别装载 inherited/own payload 与 flags；**仅当 own 末条**含未配对 tool_calls 的 AI 时
///   pop（R2-MID-1：禁止从后往前找 AI；pop 后其后无消息，无孤儿 Tool 可清理）
/// - cwd 取 `meta.cwd`（thread 创建时固化，进程重启后不得改用父 cwd）
/// - frozen、工具上限与来源来自已保存的子会话事实。
/// - cancel token：Cascade 从父 token 优先、config fallback 派生；Independent
///   恒新建 token（不复用父或 config token）
/// - **不注入** parent_messages / identity System / skill_names（F4 / R-H1：
///   旧 transcript 已含首轮注入内容，重复注入会重复）
/// - prompt 入队：`Some(p)` 原样追加（不套 fork directive）；`None` 注入隐式
///   continue 常量（issue 决策 9）
/// - run mode 由本次调用决定（issue 决策 8）：Sync → `run_sync_subagent`；
///   Background → 新 task_id + `spawn_background_subagent`
///
/// 重建/装配失败（load_messages 失败）时回滚 status 至原值（R-M1），防 thread
/// 永久停留 active（R-M4 崩溃遗留的镜像问题）。执行开始后的失败走
/// `run_sync_subagent` / bg 各自的收尾路径（error / cancelled），不回滚。
/// 准备阶段调用被 drop 时，由 claim worker 在 active 写完成后异步回滚。
pub(super) async fn resume_subagent_impl(
    parent: Option<&Arc<Session>>,
    config: SubagentResumeConfig,
) -> Result<SubagentSpawned, Box<dyn std::error::Error + Send + Sync>> {
    // 解构 config（cwd 不用于恢复——cwd 取 meta.cwd，thread 创建时固化）
    let SubagentResumeConfig {
        thread_id,
        prompt,
        agent_name: agent_name_cfg,
        run_mode,
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
        parent_invocation_id,
        cancel_token: cancel_token_cfg,
        cwd: _,
        frozen_claude_md: _,
        frozen_claude_local_md: _,
        frozen_skill_summary: _,
        frozen_date: _,
    } = config;

    super::validate_thread_id_format(&thread_id).map_err(|error| {
        preparation_rejected(
            &thread_id,
            EffectiveToolError::new(EffectiveToolErrorCode::InvalidInput, error.to_string()),
        )
    })?;

    // 绑定校验用一次一致快照：child 的绑定必须与 owning parent 完全相同，且两者同根。
    // 存在性是本函数的第一个校验分支：快照读取报 `NotFound` 即「thread not found」，
    // 与后续 `ResumeClaim` 的状态校验分开报错（不能把「不存在」说成「仍处于运行态」）。
    let child_snapshot = match session_resources.load_session_snapshot(&thread_id).await {
        Ok(snapshot) => snapshot,
        Err(error)
            if matches!(
                error.kind(),
                peri_acp_types::session_resources::SessionResourceErrorKind::NotFound
            ) =>
        {
            // 先判格式再判存在：非 UUID 不是「不存在」，与 `validate_thread` 同判据。
            return Err(preparation_rejected(
                &thread_id,
                EffectiveToolError::new(
                    EffectiveToolErrorCode::InvalidInput,
                    format!("resume_subagent: thread not found: {thread_id}"),
                ),
            ));
        }
        Err(error) => return Err(error.into()),
    };
    let binding = match &child_snapshot.binding {
        peri_acp_types::session_resources::BindingState::Bound(binding) => Some(binding.clone()),
        // 无绑定（legacy / 外来登记 / 缺失）保留旧行为：不当作 bound child 处理。
        _ => None,
    };
    if binding.is_some() {
        let parent_id = super::spawn::parent_thread_id_of(parent).ok_or_else(|| {
            preparation_rejected(
                &thread_id,
                authorization_denied("bound child resume requires its owning parent session"),
            )
        })?;
        let parent_snapshot = session_resources.load_session_snapshot(&parent_id).await?;
        let parent_binding = match &parent_snapshot.binding {
            peri_acp_types::session_resources::BindingState::Bound(binding) => binding.clone(),
            _ => {
                return Err(preparation_rejected(
                    &thread_id,
                    authorization_denied("bound subagent parent has no execution binding"),
                ))
            }
        };
        if binding.as_ref() != Some(&parent_binding) {
            return Err(preparation_rejected(
                &thread_id,
                authorization_denied(
                    &peri_acp_types::workspace::WorkspaceError::ExecutionBindingMismatch
                        .to_string(),
                ),
            ));
        }
        if super::execution_root(session_resources.as_ref(), &thread_id).await?
            != super::execution_root(session_resources.as_ref(), &parent_id).await?
        {
            return Err(preparation_rejected(
                &thread_id,
                authorization_denied("bound subagent belongs to another root session"),
            ));
        }
    }
    if child_snapshot.meta.agent_status.is_active() {
        return Err(preparation_rejected(
            &thread_id,
            preparation_failed(format!(
                "resume_subagent: thread {} is still active \
            (thread 仍处于运行态: 可能仍在执行, 或上次异常退出未收尾; \
            若确认无执行中任务, 可改用 Agent(subagent_type: ...) 新建)",
                thread_id
            )),
        ));
    }
    let cluster_root = super::execution_root(session_resources.as_ref(), &thread_id).await?;
    let ownership = task_manager
        .as_ref()
        .map(|manager| {
            peri_acp_types::tasks::TaskManager::begin_external_execution(
                manager.as_ref(),
                "subagent",
            )
        })
        .transpose()
        .map_err(|message| preparation_rejected(&thread_id, preparation_failed(message)))?;
    let (meta, claim) = ResumeClaim::acquire(
        Arc::clone(&session_resources),
        thread_id.clone(),
        cluster_root.clone(),
        ownership,
    )
    .await?;

    // The claim remains owned across history I/O and session construction. A
    // dropped caller closes its decision channel, leaving ordered rollback to
    // the worker that performed the active write.
    let restored = async {
        // 一次一致快照：inherit 区与自有 payload/flags 同一次读取（不再分三次）。
        let snapshot = session_resources
            .load_session_snapshot(&thread_id)
            .await
            .map_err(|error| anyhow::anyhow!("{error}"))?;
        let work = session_resources
            .load_session_work(&WorkQuery {
                session_id: thread_id.clone(),
                limit: 1,
            })
            .await?;
        if work.control.status != peri_acp_types::session_resources::ControlStatus::Active {
            return Err(preparation_failed("Incomplete: child control is not Active; explicit Reopen required").into());
        }
        if prompt.is_none() && work.state.works.values().any(|work|
            matches!(work.stage, peri_acp_types::session_resources::work::WorkStage::ReasonInFlight | peri_acp_types::session_resources::work::WorkStage::Blocked)
                && work.reason_request.is_some() && work.response.is_none()) {
            return Err(preparation_failed("Blocked: original model request requires reconciliation before implicit continuation").into());
        }
        let raw = work
            .state
            .child_resume_metadata
            .get(&work.control.lifecycle)
            .ok_or_else(|| preparation_failed("Blocked: saved child runtime metadata unavailable"))?;
        let saved: super::cold::ChildResumeMetadata = serde_json::from_str(raw)
            .map_err(|error| preparation_failed(format!("Blocked: invalid saved child runtime metadata: {error}")))?;
        let FrozenState::Present(bytes) = &snapshot.frozen else {
            return Err(preparation_failed("Blocked: saved child frozen snapshot unavailable").into());
        };
        if saved.version != 1
            || saved.child_session_id != thread_id
            || saved.recipient_lifecycle != work.control.lifecycle
            || saved.frozen_digest != format!("{:x}", Sha256::digest(bytes.as_str().as_bytes()))
            || saved.model_name != llm.model_name()
            || saved.authorization_ref.is_empty()
            || saved.direct_initiator_session_id.is_empty()
            || saved.delegation_invocation_id.is_empty()
            || saved.delegation_task_id.is_empty()
            || saved
                .tool_origins
                .keys()
                .cloned()
                .collect::<std::collections::BTreeSet<_>>()
                != saved.tool_ceiling
        {
            return Err(preparation_failed("Blocked: saved child runtime identity or authorization unavailable").into());
        }
        let initiator = super::spawn::parent_thread_id_of(parent)
            .ok_or_else(|| authorization_denied("current delegation initiator unavailable"))?;
        if meta.parent_thread_id.as_deref().is_some_and(|parent_id|
            parent_id != saved.direct_initiator_session_id)
            || saved.direct_initiator_lifecycle == 0
        {
            return Err(authorization_denied("saved child direct initiator differs from owning parent").into());
        }
        let original_parent_work = session_resources
            .load_session_work(&WorkQuery {
                session_id: saved.direct_initiator_session_id.clone(),
                limit: 1,
            })
            .await?;
        if original_parent_work.control.lifecycle != saved.direct_initiator_lifecycle {
            return Err(authorization_denied("saved child initiator lifecycle differs from owning parent").into());
        }
        let original = original_parent_work
            .state
            .invocations
            .get(&saved.delegation_invocation_id)
            .ok_or_else(|| authorization_denied("original trusted delegation invocation unavailable"))?;
        if original.recipient_lifecycle != saved.direct_initiator_lifecycle
            || original.intent.authorization_ref != saved.authorization_ref
            || original.intent.scope_id != saved.direct_initiator_session_id
        {
            return Err(authorization_denied("original delegation does not prove saved child authorization ceiling").into());
        }
        let invocation_id = parent_invocation_id
            .as_deref()
            .ok_or_else(|| authorization_denied("current delegation invocation unavailable"))?;
        let parent_work = session_resources
            .load_session_work(&WorkQuery {
                session_id: initiator.clone(),
                limit: 1,
            })
            .await?;
        let current = parent_work
            .state
            .invocations
            .get(invocation_id)
            .ok_or_else(|| authorization_denied("current trusted invocation unavailable"))?;
        if current.recipient_lifecycle != parent_work.control.lifecycle
            || current.intent.scope_id != initiator
            || current.intent.authorization_ref.is_empty()
        {
            return Err(authorization_denied("current delegation authorization or scope unavailable").into());
        }
        if tools
            .iter()
            .filter(|tool| saved.tool_ceiling.contains(tool.name()))
            .any(|tool| {
                saved
                    .tool_origins
                    .get(tool.name())
                    .map(|origin| origin.as_deref())
                    != Some(tool.mcp_server_name())
            })
            || !saved
                .tool_ceiling
                .is_subset(&tools.iter().map(|tool| tool.name().to_owned()).collect())
        {
            return Err(authorization_denied("Blocked: saved child tool origin or ceiling unavailable").into());
        }
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
            return Err(preparation_failed("inherited context overlaps child own history").into());
        }
        inherited.flags.extend(
            snapshot
                .flags
                .into_iter()
                .filter(|(id, _)| own_ids.contains(id)),
        );
        Ok::<_, anyhow::Error>((inherited, own, saved))
    }
    .await;
    let (inherited, mut loaded, saved) = match restored {
        Ok(history) => history,
        Err(error) => {
            let rollback = claim.rollback().await;
            if rollback.is_ok() && error.is::<EffectiveToolError>() {
                return Err(preparation_rejected(
                    &thread_id,
                    error.downcast::<EffectiveToolError>()?,
                ));
            }
            if rollback.is_err() {
                rollback_unknown(&thread_id);
            }
            let suffix = rollback.err().map(|e| format!("; {e}")).unwrap_or_default();
            return Err(format!(
                "resume_subagent: failed to load messages for {}: {}{}",
                thread_id, error, suffix
            )
            .into());
        }
    };
    let max_iterations = max_iterations.min(saved.max_iterations);
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

    let frozen_claude_md = Some(saved.claude_md.clone());
    let frozen_skill_summary = Some(saved.skill_summary.clone());
    let frozen = crate::session::FrozenContext {
        system_prompt: Arc::from(saved.system_prompt.as_str()),
        claude_md: Arc::from(saved.claude_md.as_str()),
        skill_summary: Arc::from(saved.skill_summary.as_str()),
        date: Arc::from(saved.date.as_str()),
        language: saved.language.as_deref().map(Arc::from),
        meta_harness: peri_acp_types::meta_harness::MetaHarnessState {
            section_overrides: saved
                .section_overrides
                .iter()
                .map(|(key, value)| (key.clone(), Arc::from(value.as_str())))
                .collect(),
            disabled_middlewares: saved.disabled_middlewares.iter().cloned().collect(),
            built_in_subagents_enabled: saved.built_in_subagents_enabled,
        },
        // 子 resume metadata 目前不携带运行环境快照（M3 归组）；恢复不重探
        // 本地值冒充，派生新 prompt 时按 unavailable 显式标记（H3）。
        runtime_env: None,
    };
    let ceiling = saved.tool_ceiling.clone();
    let configured_filter = tool_filter;
    let tool_filter: crate::session::tool_catalog::ToolFilter =
        Arc::new(move |tool| ceiling.contains(tool.name()) && configured_filter(tool));

    // 4. cancel token：Cascade = 父 cancel 传播（parent 优先，回退 config 注入的
    //    父 token；均缺失时新建——与 spawn 的 :466-472 完全对齐，review low-2），
    //    Independent = 恒新建（忽略 config 注入的 token——复用会让父 cancel
    //    波及 Independent 子任务，与 spawn :471 对齐，review low-1）
    let cancel_token = derive_cancel_token(parent, cancel_token_cfg, meta.cancel_policy);

    // 5. agent_name：config 优先，回退 meta.title，最后兜底 "subagent"
    let agent_name = agent_name_cfg
        .or_else(|| meta.title.clone())
        .unwrap_or_else(|| "subagent".to_string());

    // 6. 重建 session（thread_id 固定 = config.thread_id；ancestor/own 显式分区，
    //    两区 flags 均恢复后绑定持久化——helper 内）
    //    不注入 parent_messages / identity System / skill_names（F4 / R-H1）
    let (session, v2_ctx) = build_subagent_session_v2(
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
            .and_then(|host| host.session_mcp_capability.clone()),
        Vec::new(), // skill_names 恒空（R-H1：恢复不重复注入 SkillPreload）
        frozen_claude_md,
        saved.claude_local_md.clone(),
        frozen_skill_summary,
        tool_invocation_resolver,
        compact_config,
        context_budget,
        compact_llm,
        Some(agent_id_from_child_thread(&thread_id)),
    )
    .await?;

    // Assembly is synchronous but can observe cancellation from another task
    // (or a callback). Sync callers still receive the established interrupted
    // result with this thread/session identity, without publishing Start.
    // Background must pass through real registration and asynchronous completion;
    // returning a task-less success here would violate its launch contract.
    if matches!(run_mode, SubagentRunMode::Sync) && cancel_token.is_cancelled() {
        claim.rollback().await?;
        return Ok(SubagentSpawned {
            child_thread_id: thread_id,
            task_id: None,
            session,
            cancel_token,
            interrupted: true,
        });
    }

    // 7. prompt 入队：Some(p) 原样追加（不套 fork directive——恢复目标仍是原
    //    任务，直接追加指令）；None 注入隐式 continue 常量（issue 决策 9）
    let input_mode = if prompt.is_some() {
        super::delegation::DelegationInputMode::ReplaceProcessing
    } else {
        super::delegation::DelegationInputMode::FollowUp
    };
    let prompt_text = prompt.unwrap_or_else(|| IMPLICIT_CONTINUE_PROMPT.to_string());
    if matches!(run_mode, SubagentRunMode::Background) && task_manager.is_none() {
        let rollback = claim.rollback().await;
        if rollback.is_ok() {
            return Err(preparation_rejected(&thread_id, preparation_failed(format!("resume_subagent: thread {thread_id}: Background tasks not available: no task manager configured"))));
        }
        rollback_unknown(&thread_id);
        let suffix = rollback
            .err()
            .map(|error| format!("; {error}"))
            .unwrap_or_default();
        return Err(format!("resume_subagent: thread {thread_id}: Background tasks not available: no task manager configured{suffix}").into());
    }
    let task_id = match run_mode {
        SubagentRunMode::Sync => format!(
            "sync-{}",
            parent_invocation_id
                .as_deref()
                .ok_or("Blocked: current delegation invocation unavailable")?
        ),
        SubagentRunMode::Background => format!("bg-{}", uuid::Uuid::now_v7()),
    };
    let mut parent_tool_call_id = None;
    let publication = async {
        let initiator = super::spawn::parent_thread_id_of(parent).ok_or_else(|| {
            authorization_denied("Blocked: current delegation initiator unavailable")
        })?;
        let invocation_id = parent_invocation_id.as_deref().ok_or_else(|| {
            authorization_denied("Blocked: current delegation invocation unavailable")
        })?;
        let current = session_resources
            .load_session_work(&WorkQuery {
                session_id: initiator.clone(),
                limit: 1,
            })
            .await?;
        let invocation = current
            .state
            .invocations
            .get(invocation_id)
            .ok_or_else(|| {
                authorization_denied("Blocked: current trusted invocation unavailable")
            })?;
        if invocation.recipient_lifecycle != current.control.lifecycle
            || invocation.intent.scope_id != initiator
            || invocation.intent.authorization_ref.is_empty()
        {
            return Err(authorization_denied(
                "current delegation authorization or scope unavailable",
            )
            .into());
        }
        let lifecycle = session_resources
            .load_session_control(&thread_id)
            .await?
            .lifecycle;
        parent_tool_call_id = super::delegation::parent_tool_call_id(
            Some(session_resources.as_ref()),
            Some(&initiator),
            Some(invocation_id),
        )
        .await?;
        super::delegation::publish_work_delegation(
            session_resources.clone(),
            &thread_id,
            lifecycle,
            &v2_ctx.context.session.queue,
            QueuedMessage::new(
                MessageKind::Prompt,
                MessageSource::UserInput,
                BaseMessage::human(prompt_text.clone()),
            ),
            &initiator,
            invocation_id,
            &task_id,
            input_mode,
        )
        .await?;
        Ok::<_, Box<dyn std::error::Error + Send + Sync>>(())
    }
    .await;
    if let Err(error) = publication {
        let rollback = claim.rollback().await;
        if rollback.is_ok() && error.is::<EffectiveToolError>() {
            return Err(preparation_rejected(
                &thread_id,
                *error.downcast::<EffectiveToolError>()?,
            ));
        }
        if rollback.is_err() {
            rollback_unknown(&thread_id);
        }
        let suffix = rollback
            .err()
            .map(|error| format!("; {error}"))
            .unwrap_or_default();
        return Err(format!("{error}{suffix}").into());
    }

    // 8. 执行（run mode 由本次调用决定，issue 决策 8）
    match run_mode {
        SubagentRunMode::Sync => {
            // Transfer the live claim into execution, not a disarmed marker.
            let interrupted = run_sync_subagent(
                &thread_id,
                &agent_name,
                &cwd,
                max_iterations,
                event_handler,
                on_subagent_start,
                on_subagent_stop,
                Some(Arc::clone(&session_resources)),
                register_runtime,
                deregister_runtime,
                langfuse_bridge,
                parent_agent_id,
                parent_tool_call_id,
                v2_ctx,
                session.clone(),
                Some(claim),
            )
            .await?;
            Ok(SubagentSpawned {
                child_thread_id: thread_id,
                task_id: None,
                session,
                cancel_token,
                interrupted,
            })
        }
        SubagentRunMode::Background => {
            // slice 5：后台恢复——生成新 task_id、TaskManager 注册（参数与 spawn
            // 调用点对齐；prompt 传实际注入文本——用户 prompt 或 continue 常量，
            // 仅用于 prompt_summary 展示，R2-LOW-3）
            match spawn_background_subagent(
                task_id.clone(),
                thread_id.clone(),
                agent_name.clone(),
                prompt_text,
                cwd,
                max_iterations,
                bg_event_sender,
                task_manager,
                super::completion_delivery(parent, on_bg_complete),
                langfuse_bridge,
                Some(Arc::clone(&session_resources)),
                deregister_runtime,
                on_subagent_start,
                on_subagent_stop,
                register_runtime,
                parent_agent_id,
                parent_tool_call_id,
                cancel_token.clone(),
                v2_ctx,
            )
            .await
            {
                Ok(()) => claim.release().await,
                Err(e) => {
                    // review MEDIUM-1 回滚：注册失败（task_manager 缺失 /
                    // register_with_kind 撞 per-kind 上限）时任务未执行——status
                    // 回滚至原值，防 thread 永久停留 active（R-M1 执行前失败回滚
                    // 契约，与 load_messages 失败回滚同款）；错误携带 thread_id
                    let rollback = claim.rollback().await;
                    if rollback.is_err() {
                        rollback_unknown(&thread_id);
                    }
                    let suffix = rollback.err().map(|e| format!("; {e}")).unwrap_or_default();
                    return Err(
                        format!("resume_subagent: thread {}: {}{}", thread_id, e, suffix).into(),
                    );
                }
            }
            Ok(SubagentSpawned {
                child_thread_id: thread_id,
                task_id: Some(task_id),
                session,
                cancel_token,
                interrupted: false,
            })
        }
    }
}

/// 隐式 continue 指令（prompt 缺省时注入，issue 决策 9）
const IMPLICIT_CONTINUE_PROMPT: &str = "Continue your previous task where you left off.";
