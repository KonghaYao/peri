//! Session executor.

use super::{
    collect_result, exec_oneshot, input_recalls, intercept_immediate_command, spawn_event_pump,
    AgentInput, Arc, AsyncRouter, BackgroundTaskResult, BgTaskKind, CollectRequest, ContentBlock,
    ExecutionFailure, ExecutorEvent, FrozenSessionData, InterceptOutcome, InterceptRequest,
    LangfuseTurnEndHook, MessageContent, PromptResult, PromptStopReason, SessionContext,
    SpawnPumpRequest, TurnInput,
};

/// bg 完成 → ACP server continuation scheduler 的通知请求。
///
/// 由 executor 的 `on_bg_complete` 闭包在 [`AsyncRouter::route_bg_result`] 之后发送：
/// 先确保 deferred callback 已写入 SessionInbox，再通知 scheduler。
/// scheduler 按 session 原子 take `session/cancel` 置位的标记后运行一次内部
/// AsyncContinuation（见 peri-tui/src/acp_server/continuation.rs）。
#[derive(Debug, Clone)]
pub struct ContinuationRequest {
    pub session_id: String,
    pub kind: BgTaskKind,
    /// loop 自然结束后队列仍有 steering/bg Defer 待消费（不依赖 cancel armed）。
    pub mq_steering: bool,
    /// 已规范提交但尚未被主模型响应覆盖的后台结果构成的工作来源。
    pub pending_result: bool,
    /// 由恢复启动门发出的 continuation：只有这类请求的早失败会反馈给恢复协调器。
    pub recovery: bool,
}

/// keepgoing 判定：内容按 block 判空（`MessageContent::is_empty`）。
///
/// 这是 TUI keepgoing 按钮 ↔ ACP ↔ agent stages 的**跨层共享判定**：
/// - 此处：空 prompt → 跳过 recall 注入（`run_session_loop`）
/// - `peri-agent` stages：空 Prompt → 不写入 transcript（`append_messages_to_transcript`）
///
/// 必须与 stages 层保持同一语义。用 `is_empty()`（按 content block 判空）而非
/// `text_content().trim()`——后者会把 `Blocks([Image])` 这类纯附件消息误判为
/// keepgoing（图片接入后即触发），且畸形请求经 `extract_prompt_params` 默认值
/// （空文本）落入 keepgoing 路径时行为一致。
///
/// 协议约定（见 docs/standards/architecture-contracts.md ARC-KEEPGOING-001）：
/// 空白 user prompt = "继续跑 loop"，唯一生产者为 TUI keepgoing 按钮。
#[must_use]
pub fn is_keepgoing(content: &peri_acp_types::messages::MessageContent) -> bool {
    content.is_empty()
}

/// 把本轮 `bg_results` 注入 MQ；任一失败都必须回到调用方（一次性投递）。
pub(super) async fn inject_bg_results(
    router: &AsyncRouter,
    results: &[BackgroundTaskResult],
) -> Result<(), String> {
    let mut failures = Vec::new();
    for result in results {
        if let Err(error) = router.route_bg_result(result, BgTaskKind::Agent).await {
            failures.push(format!("task {}: {error}", result.task_id));
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("; "))
    }
}

/// Per-turn computed configuration derived from [`SessionContext`].
///
/// Built once at the top of [`run_session_loop`], passed by reference to
/// [`build_and_execute_agent`] to avoid recomputing and to keep the agent
/// builder function signature manageable.
pub(super) struct TurnConfig<'turn> {
    pub(super) cwd: &'turn str,
    pub(super) frozen: Option<&'turn FrozenSessionData>,
    pub(super) language: Option<String>,
    pub(super) auxiliary_model: Option<Arc<dyn peri_model::Model>>,
}

/// Shared agent execution pipeline with auto-compact support.
///
/// # 调用方职责（L5 依赖反转）
///
/// - Session management (storing/retrieving cwd, history, cancel_token)
/// - Choosing the broker (HITL/AskUser handler)
/// - Providing the correct `EventSink` implementation
/// - 投影 ACP 特有构造（provider / peri_config / AgentPool / SessionManager /
///   Controller）为 [`SessionContext`] 的端口字段与注入闭包
/// - 经 [`TurnInput::stage_build`] / [`TurnInput::forwarder_launcher`] 注入
///   stage 装配桥与 EventBus forwarder 启动器（ACP 宿主侧）
pub async fn run_session_loop(ctx: SessionContext, turn: TurnInput) -> PromptResult {
    let TurnInput {
        event_sink,
        content,
        continuation,
        frozen,
        history_payloads,
        incoming_recalls,
        bg_results,
        langfuse,
        stage_build,
        forwarder_launcher,
    } = turn;

    // keepgoing：空白 user prompt 是 TUI keepgoing 按钮发起的"继续跑 loop"指令。
    // 语义：不插入 user prompt（stages/append_messages_to_transcript 跳过空 Prompt），
    // 仅让 Receive 消费计数 >0 从而驱动 ReAct loop 继续。此时不注入 recall——
    // 否则 recall 会拼进 user 消息使其非空，破坏"不插入"语义。
    // 判定与 stages 层共用同一语义：按 content block 判空（见 is_keepgoing 注释）。
    //
    // [AsyncContinuation] 内部续跑（continuation=true）不是 keepgoing：
    // 空 user prompt 不落入 keepgoing 语义，也不走空历史 keepgoing short-circuit。
    // 与 keepgoing 相同的是：**不注入 recall**——上一轮留给用户 prompt 的 recall
    // 由 run_prompt 保留在 SessionState（clone 而非 take），续跑只消费已 route 的
    // Defer/Info 消息；recall 留给后续用户 prompt 注入。
    let is_keepgoing = !continuation && is_keepgoing(&content);
    let mailbox_input = ctx
        .user_input_mailbox
        .as_ref()
        .is_some_and(|mailbox| mailbox.is_managed_attempt());
    let mut incoming_recalls = if is_keepgoing || (continuation && !mailbox_input) {
        tracing::debug!(
            skip = if continuation {
                "continuation"
            } else {
                "keepgoing"
            },
            "empty user prompt, skipping recall injection"
        );
        Vec::new()
    } else {
        incoming_recalls
    };

    // 空历史 + 空 prompt：无内容可继续——直接短路返回，避免跑一轮无意义 LLM 调用。
    // （TUI 侧 handle_keepgoing_submit 已有 has_session 防御；此处防御 stdio 等
    // 其他 transport 对全新 session 发空 prompt 的场景。）
    //
    // 判空只看消息投影：canonical reminder 没有 `BaseMessage` 形态，与旧
    // `TurnInput.history.is_empty()` 口径一致；投影仅在确实短路时构造一次。
    let has_projected_history = history_payloads
        .iter()
        .any(|payload| payload.as_message().is_some());
    if is_keepgoing
        && !has_projected_history
        && !ctx
            .user_input_mailbox
            .as_ref()
            .is_some_and(|mailbox| mailbox.has_handed_off_inputs())
    {
        tracing::debug!("keepgoing: empty history, short-circuiting (nothing to continue)");
        // [TRAP] 短路路径绕过 agent event pump（spawn_event_pump 的 push_done
        // 不会执行），必须手动发送终止通知（ARC-EVENT-001），否则 TUI 依赖
        // AgentDone→TurnDone 退出 loading 的机制失效，界面永久卡在 loading。
        // stop_reason 与正常路径保持一致（executor_helpers push_done "end_turn"）。
        event_sink
            .push_done(&ctx.session_id, "end_turn", ctx.request_id.as_deref())
            .await;
        let messages = history_payloads
            .iter()
            .filter_map(|payload| payload.as_message().cloned())
            .collect();
        return PromptResult {
            execution_binding: None,
            persisted_payloads: history_payloads,
            messages,
            ok: true,
            stop_reason: PromptStopReason::EndTurn,
            history_replaced_by_compaction: false,
            persistence_inconsistent: false,
            recall_items: Vec::new(),
            pending_tasks: 0,
            failure: None,
            context_usage_tokens: None,
        };
    }

    // The host has already applied environment overrides to this turn's config.
    let disable_compact = !ctx.compact_config.auto_compact_enabled;

    // 解析会话级共享的 v2 MessageQueue（经 SessionAccessPort）。
    // 缺失时（无 session_access / session 不存在）退化为独立 MessageQueue，
    // 保持行为可运行——但跨 turn 消息将不可见（仅降级场景）。
    //
    // 在 run_session_loop 开头解析而非 build_and_execute_agent 内部，
    // 是为了让 bg_results / workflow Path B 等会话级注入能在此处统一 push。
    let v2_message_queue = ctx
        .session_access
        .as_ref()
        .and_then(|sa| sa.v2_message_queue(&ctx.session_id))
        .unwrap_or_default();
    if mailbox_input && !incoming_recalls.is_empty() {
        input_recalls::push_input_recalls(&v2_message_queue, &incoming_recalls).await;
        incoming_recalls.clear();
    }

    let async_router = AsyncRouter::for_queue(&v2_message_queue);
    if !bg_results.is_empty() {
        tracing::info!(
            count = bg_results.len(),
            "[bg-diag] ctx.bg_results is non-empty, will inject each via AsyncRouter"
        );
        // bg_results 来自一次性 ACP prompt 参数，没有第二次投递机会：失败必须
        // 以本轮失败暴露，不能只留日志。
        if let Err(error) = inject_bg_results(&async_router, &bg_results).await {
            event_sink
                .push_done(&ctx.session_id, "error", ctx.request_id.as_deref())
                .await;
            let messages = history_payloads
                .iter()
                .filter_map(|payload| payload.as_message().cloned())
                .collect();
            return PromptResult {
                execution_binding: None,
                persisted_payloads: history_payloads,
                messages,
                ok: false,
                stop_reason: PromptStopReason::EndTurn,
                history_replaced_by_compaction: false,
                persistence_inconsistent: false,
                recall_items: Vec::new(),
                pending_tasks: 0,
                failure: Some(ExecutionFailure::internal(format!(
                    "background result injection failed: {error}"
                ))),
                context_usage_tokens: None,
            };
        }
    }

    // Auxiliary model — reuse AgentPool cache if available, otherwise create fresh.
    // 共享于 v2 stages/compact.rs（摘要）与 Goal 工具（完成度验证）。
    // （L5：缓存读取 / fresh 构造经注入闭包，AgentPool 留在 ACP）
    let cached_llm = ctx.get_cached_llm.as_ref().and_then(|f| f());
    let auxiliary_model: Option<Arc<dyn peri_model::Model>> = if disable_compact {
        None
    } else {
        cached_llm
            .as_ref()
            .map(|c| c.auxiliary_model.clone())
            .or_else(|| {
                // 转发器从 session 级 AgentPool 取：fresh 模型烘焙的 observer 会随
                // CachedLlmInstances 跨 turn 复用，必须指向 session 级转发器。
                ctx.fresh_auxiliary_model.as_ref().map(|f| f())
            })
    };

    // Context window（宿主构造点已按 context_1m 计算 effective 值）
    let effective_context_window = ctx.effective_context_window;

    // session 级 TaskManager（跨 prompt 存活，由 executor 从 session 获取）
    // 有界等待摘要需要 loop 结束后的未结算计数；TaskManager 随后被 move 进
    // 执行路径，这里先保留一个 Arc 句柄。
    let pending_task_probe = ctx
        .session_access
        .as_ref()
        .and_then(|sa| sa.task_manager(&ctx.session_id));
    let task_manager_for_cmd = pending_task_probe
        .clone()
        .unwrap_or_else(|| Arc::new(peri_acp_types::tasks::NoopTaskManager));
    let goal_controller_for_cmd = ctx
        .session_access
        .as_ref()
        .and_then(|sa| sa.goal_controller(&ctx.session_id));
    task_manager_for_cmd.retry_pending_deliveries();

    // ── L5 命令拦截注入面（注册表 / compact 配置）──
    let command_lookup = Arc::clone(&ctx.command_lookup);
    let compact_config_loader = Arc::clone(&ctx.compact_config_loader);

    // Command interception — check if content is a slash command before building agent.
    // 三态分发（Phase 5 Step 6）：
    //   Handled(r)   → 命令已完成（push_done 已由拦截层调用），直接返回；
    //   Inject(text) → AgentInput::blocks(text) 进 agent 管线（不 push_done，
    //                  pump 负责）；
    //   PassThrough  → 现状 AgentInput::blocks(content)。
    let injected_content = match intercept_immediate_command(InterceptRequest {
        content: &content,
        history_payloads: std::borrow::Cow::Borrowed(&history_payloads),
        cwd: &ctx.cwd,
        session_id: &ctx.session_id,
        cancel: &ctx.cancel,
        session_resources: ctx.session_resources.clone(),
        thread_id: ctx.thread_id.clone(),
        // L5：冻结数据由调用点投影为字符串字段（原 FrozenSessionData 引用）
        frozen_claude_md: frozen
            .as_ref()
            .and_then(|f| f.claude_md().map(String::from)),
        frozen_claude_local_md: frozen
            .as_ref()
            .and_then(|f| f.claude_local_md().map(String::from)),
        frozen_skill_summary: frozen
            .as_ref()
            .and_then(|f| f.skill_summary().map(String::from)),
        // fork/bg-fork 复用的冻结 prompt（16_workflow 已删除（C2），子面向
        // 与主 prompt 字节相同，直接复用主 prompt）
        frozen_system_prompt: frozen.as_ref().map(|f| f.system_prompt().to_string()),
        event_sink: &event_sink,
        auxiliary_model: &auxiliary_model,
        task_manager: &task_manager_for_cmd,
        goal_controller: goal_controller_for_cmd,
        mcp_pool: ctx.mcp_pool.clone(),
        async_result_mailbox: ctx.async_result_mailbox.clone(),
        command_lookup,
        compact_config_loader,
    })
    .await
    {
        InterceptOutcome::Handled(result) => return *result,
        InterceptOutcome::Inject(text) => MessageContent::text(text),
        InterceptOutcome::PassThrough => content,
    };

    let trace_input = injected_content.text_content();
    // 演进 1（meta-harness 波 4）：permission_mode 运行时通知注入已整体删除。
    // 模型对权限模式的感知仅经 10_hitl 段落（PermissionMiddleware 持有）
    // 的机制说明——mode 会话内切换不再注入 `<system-reminder>` 通知。
    // 此处仅保留 incoming_recalls 的受控容器注入语义。
    let agent_input = if incoming_recalls.is_empty() {
        AgentInput::blocks(injected_content)
    } else {
        let mut blocks = injected_content.content_blocks();
        blocks.push(ContentBlock::text(incoming_recalls.join("\n")));
        AgentInput::blocks(MessageContent::blocks(blocks))
    };

    // Event channel (lives for entire run_session_loop lifetime)
    let (event_tx, event_rx) = tokio::sync::mpsc::unbounded_channel::<ExecutorEvent>();
    let event_tx = Arc::new(parking_lot::Mutex::new(Some(event_tx)));

    let turn = TurnConfig {
        cwd: &ctx.cwd,
        frozen: frozen.as_ref(),
        language: frozen
            .as_ref()
            .and_then(|f| f.language().map(|s| s.to_string()))
            .or_else(|| ctx.language.clone()),
        auxiliary_model: auxiliary_model.clone(),
    };

    // Langfuse 遥测经注入闭包（宿主构造的 LangfuseHooks）接入。
    let langfuse_on_turn_start: Option<Arc<dyn Fn() + Send + Sync>> = langfuse.as_ref().map(|h| {
        let on_start = Arc::clone(&h.on_turn_start);
        let trace_input = trace_input.to_string();
        Arc::new(move || {
            on_start(&trace_input);
        }) as Arc<dyn Fn() + Send + Sync>
    });
    let langfuse_on_turn_end: Option<LangfuseTurnEndHook> =
        langfuse.as_ref().map(|h| Arc::clone(&h.on_turn_end));

    // Main event pump（事件三层化：发射点 → EventPublisher →
    // 本泵订阅消费；event_rx 仅作发射点集合的关闭信号）
    let (stop_reason_tx, stop_reason_rx) = exec_oneshot::channel::<(
        PromptStopReason,
        peri_acp_types::session::TurnTelemetryOutcome,
    )>();
    let pump_handle = spawn_event_pump(SpawnPumpRequest {
        // L5：订阅经端口适配（契约层 SubscriptionError 镜像，Controller 零改动）
        subscription: (ctx.subscribe)(),
        event_rx,
        stop_reason_rx,
        sink: Arc::clone(&event_sink),
        session_id: ctx.session_id.clone(),
        effective_context_window,
        // L5：Langfuse tracer 留在 ACP——泵经闭包在任务开头触发
        // on_turn_start、pump_done 之后触发 on_turn_end（JoinHandle drop =
        // fire-and-forget，不得阻塞管线）。
        langfuse_on_turn_start,
        langfuse_on_turn_end,
        request_id: ctx.request_id.clone(),
        delivery_journal: ctx.user_input_mailbox.clone(),
    });

    // 把会 move/借用 的资源直接传入 build_and_execute_agent。
    // 由于 prompt builder 需要的所有资源都已提供，调用方后续不再访问这些已 move 字段
    // （session_id 在 collect_result 借用，此时 build_and_execute_agent 已完成）。
    let exec_outcome = crate::session::exec::executor::agent_build::build_and_execute_agent(
        &ctx,
        &turn,
        agent_input,
        history_payloads,
        &ctx.session_id,
        cached_llm.as_ref(),
        continuation,
        stage_build,
        forwarder_launcher,
    )
    .await;

    // Send canonical terminal outcome to the event pump before it pushes done.
    let telemetry_outcome = peri_acp_types::session::TurnTelemetryOutcome::from_result(
        exec_outcome.stop_reason,
        exec_outcome.failure.clone(),
    );
    let _ = stop_reason_tx.send((exec_outcome.stop_reason, telemetry_outcome));

    let pending_tasks =
        crate::session::exec::executor_helpers::pending_task_count(pending_task_probe.as_ref());
    let result = collect_result(CollectRequest {
        event_tx: &event_tx,
        pump_handle,
        session_id: &ctx.session_id,
        exec_outcome,
        pending_tasks,
    })
    .await;

    // turn 收尾：转发器是 session 级（挂 AgentPool），turn 间不清理、不重建，
    // 靠下一 turn `build_agent` 覆盖式 `set` 当前 handler。残留 handler（v1
    // 直发）经 `EventPublisher` 发射，事件泵随本轮 `event_tx` 关闭
    // 退出后到达的事件自然丢弃（与迁移前 close 后检查 None 丢弃语义一致）。

    result
}
