use std::sync::Arc;

use peri_acp_types::{
    command::{
        CommandContext, CommandFeedback, CommandOutcome, CommandResult, FeedbackChannel,
        FeedbackLevel, PromptStopReason, ResolvedCommand,
    },
    compact::CompactConfig,
    event::{EventSink, ExecutorEvent},
    messages::{BaseMessage, MessageContent},
    session::PromptResult,
    store::ThreadStore,
    tasks::TaskManager,
};
use tokio_util::sync::CancellationToken;

/// 命令注册表查找闭包（ACP 协议面注册表注入）。
///
/// P1-6 定案：返回 `Option<ResolvedCommand>` 全链统一——args 词法切分由
/// 注册表 `resolve` 唯一实现（设计不变式 3），拦截层消费 `resolved.args`
/// 与 `resolved.entry.args_schema`。
pub type CommandLookupFn = Arc<dyn Fn(&str) -> Option<ResolvedCommand> + Send + Sync>;

// ── Intercept Request parameter object ─────────────────────────────────────

/// 命令拦截请求（参数对象，避免 12 个位置参数）。
///
/// L5 依赖反转：`peri_config`（ACP provider 配置）不进入本结构——compact
/// 配置经 [`InterceptRequest::compact_config_loader`] 注入闭包按
/// `load_compact_config` 语义预填（env overrides 每轮重新应用）；
/// 命令注册表查找经 [`InterceptRequest::command_lookup`] 注入（ACP 协议面
/// 会话级注册表语义，`resolve` 严格精确）。
pub struct InterceptRequest<'a> {
    // ── 消息上下文 ──
    pub content: &'a MessageContent,
    pub history: &'a [BaseMessage],
    // ── 会话上下文 ──
    pub cwd: &'a str,
    pub session_id: &'a str,
    pub cancel: &'a CancellationToken,
    pub thread_store: Option<Arc<dyn ThreadStore>>,
    pub thread_id: Option<String>,
    // ── 冻结数据投影（原 FrozenSessionData；ACP 调用点投影字符串）──
    pub frozen_claude_md: Option<String>,
    pub frozen_claude_local_md: Option<String>,
    pub frozen_skill_summary: Option<String>,
    pub frozen_system_prompt: Option<String>,
    // ── 运行时服务 ──
    pub event_sink: &'a Arc<dyn EventSink>,
    pub auxiliary_model: &'a Option<Arc<dyn peri_model::Model>>,
    // ── 异步服务 ──
    pub task_manager: &'a Arc<dyn TaskManager>,
    // ── 注入面（L5 依赖反转）──
    /// 命令注册表查找（ACP 协议面注册表；`None` = 未注册，fall-through）。
    pub command_lookup: CommandLookupFn,
    /// compact 配置装载（ACP 侧 `load_compact_config` 语义，含 env overrides）。
    pub compact_config_loader: Arc<dyn Fn() -> CompactConfig + Send + Sync>,
}

/// 编排层统一反馈出口（Phase 5 Step 1）：UiOnly/Session 均发射
/// `CommandFeedback` 事件；channel=Session 额外把 message 以系统消息追加进
/// `result.messages`（自动落 thread store + state.history 接续）。
///
/// 发射唯一归属本函数——命令只返回 `feedback` 字段，命令内零事件代码
/// （设计 §80/§89；P1-4）。调用时序：`handler.execute` 之后、`push_done`
/// 之前（R5：feedback 事件与 push_done 经同一 EventSink 顺序发送，TUI 无
/// id 配对依赖该顺序）。
///
/// # 暴露方式
/// `pub` + 经 `peri_agent::session::exec::executor_helpers` 模块路径可达
/// （模块链 pub，peri-acp 侧已有 use 先例：`host/prompt.rs` /
/// `host/executor_flow_test.rs`）；peri-acp 侧直接 `use`，无需 re-export 桥。
pub async fn emit_command_feedback(
    sink: &Arc<dyn EventSink>,
    session_id: &str,
    result: &mut CommandResult,
) {
    if let Some(fb) = result.feedback.take() {
        sink.push_event(
            session_id,
            &ExecutorEvent::CommandFeedback(CommandFeedback {
                level: fb.level,
                message: fb.message.clone(),
                channel: fb.channel,
            }),
            0,
        )
        .await;
        if fb.channel == FeedbackChannel::Session {
            result.messages.push(BaseMessage::system(fb.message));
        }
    }
}

/// 命令拦截结果三态（Phase 5 Step 6 定案，计划代码形态）：
///
/// - [`InterceptOutcome::Handled`]：命令已完成（`Done` 映射——ok:true /
///   `push_done` 已调用，agent 不构建）
/// - [`InterceptOutcome::Inject`]：透传指令进 agent 管线（本 Phase 无命令
///   返回，预留；不 `push_done`——agent pump 负责）
/// - [`InterceptOutcome::PassThrough`]：未命中 / 词法非法 / 非 Immediate：
///   fall through 进 agent 管线（不报错，设计 §78）
pub enum InterceptOutcome {
    Handled(PromptResult),
    Inject(String),
    PassThrough,
}

/// 命令拦截：检查 content 是否为已注册 slash 命令。
///
/// 返回 [`InterceptOutcome`] 三态（旧 `Option<PromptResult>` 退役）：
/// `Handled` = 已处理（agent 不构建）；`Inject` = 注入 agent 管线；
/// `PassThrough` = 继续走 agent 管线。
///
/// 执行方式由 [`CommandOutcome`] 承载（旧 `kind() != Immediate` 判断已删除）：
/// `Done` → `Handled`（emit_command_feedback → push_done）；`Inject` → 回传
/// `Inject`（不 push_done）；`Delegate` 本 Phase 无实现（恒 `Done`），
/// `unreachable!`（Phase 6 ui 域上送注册后接入）。
///
/// [TRAP] Immediate 命令路径绕过 agent event pump，必须手动调用 `sink.push_done()`。
/// 否则 TUI 界面永久卡在 loading 状态（issue_2026-05-29-immediate-command-missing-push-done）。
pub async fn intercept_immediate_command(req: InterceptRequest<'_>) -> InterceptOutcome {
    let text = req.content.text_content();
    let Some(stripped) = text.strip_prefix('/') else {
        return InterceptOutcome::PassThrough;
    };
    if stripped.is_empty() {
        return InterceptOutcome::PassThrough;
    }

    // 命令注册表查找经注入闭包（ACP 协议面注册表；接收已 strip `/` 前缀的
    // 命令文本，命令名 + 参数词法切分由注册表 resolve 统一完成，不变式 3）。
    let Some(resolved) = (req.command_lookup)(stripped) else {
        return InterceptOutcome::PassThrough;
    };

    tracing::debug!(
        command = %resolved.entry.fullname,
        history_len = req.history.len(),
        "command intercepted"
    );

    // args 解析（Phase 5 Step 6）：构造 CommandContext 前消费 `resolved.args`
    // （词法切分由注册表 resolve 统一完成，不变式 3），调用
    // `resolved.entry.args_schema` 声明的解析器（Phase 1 ArgsSchema）。
    // 失败 → 不进入 handler，立即返回 `Done` + `feedback(Error)`（rewind
    // 现状语义泛化，设计 §81：错误不进会话、走 UI 通道）；
    // 成功 → `ParsedArgs` 经 `ctx.parsed_args` 传入 handler，handler 消费
    // 统一解析结果，不再自研解析（P1-1，验收标准第 2 条）。
    let parsed_args = match &resolved.entry.args_schema {
        Some(schema) => match schema.parse(&resolved.args) {
            Ok(parsed) => Some(parsed),
            Err(err) => {
                let name = resolved
                    .entry
                    .fullname
                    .rsplit(':')
                    .next()
                    .unwrap_or(&resolved.entry.fullname);
                let mut result = CommandResult {
                    messages: req.history.to_vec(),
                    stop_reason: PromptStopReason::EndTurn,
                    feedback: Some(CommandFeedback {
                        level: FeedbackLevel::Error,
                        message: format!("{name} 参数解析失败: {err}"),
                        channel: FeedbackChannel::UiOnly,
                    }),
                };
                emit_command_feedback(req.event_sink, req.session_id, &mut result).await;
                req.event_sink
                    .push_done(req.session_id, "end_turn", None)
                    .await;
                return InterceptOutcome::Handled(PromptResult {
                    messages: result.messages,
                    ok: true,
                    stop_reason: result.stop_reason,
                    history_replaced_by_compaction: false,
                    recall_items: Vec::new(),
                    failure: None,
                });
            }
        },
        None => None,
    };

    let deps = peri_acp_types::command::DependencyBag::new();
    let mut ctx = CommandContext::new(
        req.session_id.to_string(),
        req.history.to_vec(),
        req.cwd.to_string(),
        req.event_sink.clone(),
        req.cancel.clone(),
        deps,
    );
    // L5：compact 配置由装配点预填（env overrides 每轮重新应用，
    // 语义与原 compact_pipeline::load_compact_config 一致）。
    ctx.compact_config = (req.compact_config_loader)();
    ctx.auxiliary_model = req.auxiliary_model.clone();
    ctx.args = resolved.args;
    // 用户消息原文随上下文透传（AgentPassthrough 等 handler 交还 agent 管线
    // 用；skill 命中时原文含 `/skill-name` token，SkillPreloadMiddleware
    // 自动检测分支依赖原文，命令不被吞）。
    ctx.raw_text = req.content.text_content();
    // 拦截层存在 agent 管线：`CommandOutcome::Inject` 原文会替换用户消息进
    // 管线（McpSkillReleaser 依此放行，决策 A2；RPC 路径恒 false）。
    ctx.supports_inject = true;
    ctx.parsed_args = parsed_args;
    ctx.thread_store = req.thread_store;
    ctx.thread_id = req.thread_id;
    ctx.task_manager = Some(req.task_manager.clone());
    ctx.frozen_claude_md = req.frozen_claude_md.clone().map(Arc::new);
    ctx.frozen_claude_local_md = req.frozen_claude_local_md.clone().map(Arc::new);
    ctx.frozen_skill_summary = req.frozen_skill_summary.clone().map(Arc::new);
    // fork/bg-fork 复用的冻结 prompt（16_workflow 已删除（C2），与主
    // prompt 字节相同）
    ctx.frozen_system_prompt = req.frozen_system_prompt.clone().map(Arc::new);
    // 扁平 RouteEntry：handler 为 pub 字段（对齐现状 select! 分支顺序——
    // execute 优先于 cancel.cancelled()；二者均会触发 push_done）。
    let outcome = tokio::select! {
        r = resolved.entry.handler.execute(ctx) => r,
        _ = req.cancel.cancelled() => {
            tracing::info!(session_id = %req.session_id, "command cancelled");
            CommandOutcome::Done(CommandResult {
                messages: req.history.to_vec(),
                stop_reason: PromptStopReason::Cancelled,
                feedback: None,
            })
        }
    };
    // Outcome 三态分发（Phase 5 Step 6；计划代码形态）：
    //   Done(r)   → emit_command_feedback（Step 1）→ push_done → Handled(PromptResult)
    //   Inject(s) → 不 push_done（agent pump 负责），回传 Inject
    //   Delegate(_) → 本 Phase 无实现（恒 Done），unreachable!（Phase 6 使用）
    match outcome {
        CommandOutcome::Done(mut result) => {
            // 反馈统一出口：handler.execute 之后、push_done 之前发射
            // CommandFeedback 事件（channel=Session 额外追加系统消息）。
            // [P2-1] 占位日志退役——事件通道已接入（Phase 5 Step 1）。
            emit_command_feedback(req.event_sink, req.session_id, &mut result).await;
            // Immediate 命令跳过 agent event pump，必须手动发送 push_done
            // 通知 TUI agent 执行完成，否则界面永久卡在 loading 状态。
            // 命令 turn 无 request_id（None）——TUI 侧跳过 id 配对、回退代际兜底。
            req.event_sink
                .push_done(req.session_id, "end_turn", None)
                .await;
            InterceptOutcome::Handled(PromptResult {
                messages: result.messages,
                ok: true,
                stop_reason: result.stop_reason,
                history_replaced_by_compaction: false,
                recall_items: Vec::new(),
                failure: None,
            })
        }
        CommandOutcome::Inject(payload) => InterceptOutcome::Inject(payload),
        CommandOutcome::Delegate(_) => {
            unreachable!("Delegate 本 Phase 无实现（恒 Done）；Phase 6 ui 域上送注册后接入")
        }
    }
}
