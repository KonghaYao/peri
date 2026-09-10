//! Existing child-thread claim, history reconstruction and pre-run rollback.

use std::sync::Arc;

use super::super::background::spawn_background_subagent;
use super::super::run_sync::run_sync_subagent;
use super::super::types::{SubagentResumeConfig, SubagentRunMode, SubagentSpawned};
use super::super::v2_bridge::agent_id_from_child_thread;
use super::claim::ResumeClaim;
use super::context::{build_subagent_session_v2, derive_cancel_token, inherited_frozen_context};
use crate::messages::BaseMessage;
use crate::session::queue::{MessageKind, MessageSource, QueuedMessage};
use crate::session::Session;

// ─── 恢复（统一入口 resume_subagent） ───────────────────────────────────────

/// 恢复子 agent（统一入口实现，slice 4：重建 + 执行）。
///
/// 两层校验（不通过返回明确 Err，与 issue 验收一致）：
/// 1. 存在性：`load_meta` 失败/不存在 → `thread not found`
/// 2. status：`agent_status == Active`（可能未正常收尾）→ 拒绝恢复
///
/// parent 链校验已移除：parent_thread_id 解析链路（store().thread_id 优先、
/// 回退 host 注入值）在生产路径经常与写盘值不一致，误判导致兄弟 subagent
/// 无法恢复；thread_id 本身就是不透明凭证，持有 child_thread_id 即可恢复。
///
/// 校验 → 置 active 段整体持锁（R-M1：防并发双 resume 双执行同一 thread）；
/// 锁内仅 load_meta + update_thread_status（无嵌套锁，不 await run_react_loop）。
/// 锁释放后重建：
/// - `load_messages` 重放 transcript；**仅当末条**含未配对 tool_calls 的 AI 时
///   pop（R2-MID-1：禁止从后往前找 AI；pop 后其后无消息，无孤儿 Tool 可清理）
/// - cwd 取 `meta.cwd`（thread 创建时固化，进程重启后不得改用父 cwd）
/// - frozen 从父 session copy（ARC-FROZEN-001；parent None 用 config 回退）
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
        error_suggest_registry,
        tool_registry_snapshot,
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
        cwd: _,
        frozen_claude_md: frozen_claude_md_cfg,
        frozen_claude_local_md: frozen_claude_local_md_cfg,
        frozen_skill_summary: frozen_skill_summary_cfg,
        frozen_date: frozen_date_cfg,
    } = config;

    let (meta, claim) = ResumeClaim::acquire(Arc::clone(&thread_store), thread_id.clone()).await?;

    // The claim remains owned across history I/O and session construction. A
    // dropped caller closes its decision channel, leaving ordered rollback to
    // the worker that performed the active write.
    let mut loaded = match thread_store.load_payloads(&thread_id).await {
        Ok(msgs) => msgs,
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

    // 2. cwd 取 meta.cwd（thread 创建时固化的；进程重启后不得改用父 cwd）
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
    let frozen = inherited_frozen_context(
        parent,
        &frozen_claude_md,
        &frozen_skill_summary,
        &frozen_date,
    );

    // 4. cancel token：Cascade = 父 cancel 传播（parent 优先，回退 config 注入的
    //    父 token；均缺失时新建——与 spawn 的 :466-472 完全对齐，review low-2），
    //    Independent = 恒新建（忽略 config 注入的 token——复用会让父 cancel
    //    波及 Independent 子任务，与 spawn :471 对齐，review low-1）
    let cancel_token = derive_cancel_token(parent, cancel_token_cfg, meta.cancel_policy);

    // 5. agent_name：config 优先，回退 meta.title，最后兜底 "subagent"
    let agent_name = agent_name_cfg
        .or_else(|| meta.title.clone())
        .unwrap_or_else(|| "subagent".to_string());

    // 6. 重建 session（thread_id 固定 = config.thread_id；with_ancestor 装载
    //    旧 transcript 重放 + with_persistence 绑定，顺序不可反——helper 内）
    //    不注入 parent_messages / identity System / skill_names（F4 / R-H1）
    let (session, v2_ctx) = build_subagent_session_v2(
        cwd.clone(),
        frozen,
        cancel_token.clone(),
        thread_id.clone(),
        Some(Arc::clone(&thread_store)),
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
        frozen_claude_local_md_cfg,
        frozen_skill_summary,
        tool_invocation_resolver,
        error_suggest_registry,
        tool_registry_snapshot,
        compact_config,
        context_budget,
        compact_llm,
        Some(agent_id_from_child_thread(&thread_id)),
    );

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
    let prompt_text = prompt.unwrap_or_else(|| IMPLICIT_CONTINUE_PROMPT.to_string());
    v2_ctx.context.session.queue.push(QueuedMessage::new(
        MessageKind::Prompt,
        MessageSource::UserInput,
        BaseMessage::human(prompt_text.clone()),
    ));

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
                Some(Arc::clone(&thread_store)),
                register_runtime,
                deregister_runtime,
                langfuse_bridge,
                parent_agent_id,
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
            let task_id = format!("bg-{}", uuid::Uuid::now_v7());
            match spawn_background_subagent(
                task_id.clone(),
                thread_id.clone(),
                agent_name.clone(),
                prompt_text,
                cwd,
                max_iterations,
                bg_event_sender,
                task_manager,
                on_bg_complete,
                langfuse_bridge,
                Some(Arc::clone(&thread_store)),
                deregister_runtime,
                on_subagent_start,
                on_subagent_stop,
                register_runtime,
                parent_agent_id,
                cancel_token.clone(),
                v2_ctx,
            )
            .await
            {
                Ok(()) => claim.release(),
                Err(e) => {
                    // review MEDIUM-1 回滚：注册失败（task_manager 缺失 /
                    // register_with_kind 撞 per-kind 上限）时任务未执行——status
                    // 回滚至原值，防 thread 永久停留 active（R-M1 执行前失败回滚
                    // 契约，与 load_messages 失败回滚同款）；错误携带 thread_id
                    let rollback = claim.rollback().await;
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
