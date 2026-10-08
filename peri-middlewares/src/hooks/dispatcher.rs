//! Hook 分发引擎 + standalone 路径。
//!
//! 把原本分散在 `HookMiddleware::fire_event` 与 `fire_standalone_lifecycle_hooks`
//! 中重复的"hook 查找 / matcher 过滤 / async spawn / 同步执行 / once 预留"
//! 收敛到一个 [`HookDispatcher`]。Standalone 路径通过相同的分发逻辑执行
//! （差异：无 LLM factory，因此 Prompt/Agent hook 被跳过）。

use std::{collections::HashMap, sync::Arc};

use parking_lot::RwLock;
use peri_acp_types::tasks::TaskManager;
use peri_agent::agent::react::ReactLLM;

use crate::hooks::{
    executor::{
        execute_agent_hook, execute_command_hook_owned, execute_http_hook, execute_prompt_hook,
    },
    input_builder,
    matcher::{matches_if_condition, matches_matcher},
    once_tracker::OnceTracker,
    types::{HookAction, HookEvent, HookInput, HookType, PermissionDecision, RegisteredHook},
};

/// 核心分发引擎。
///
/// 持有：
/// - `hooks`: 事件 → 已注册 hook 列表
/// - `llm_factory`: 用于 Prompt / Agent hook（standalone 路径不持有）
/// - `once_tracker`: 一次性 hook 状态
///
/// [TRAP] `llm_factory` 与 `once_tracker` 均为 `Arc`，允许 dispatcher 被多处
/// 共享（如 future 的 standalone 复用同 tracker 的场景）。
pub struct HookDispatcher {
    hooks: Arc<RwLock<HashMap<HookEvent, Vec<RegisteredHook>>>>,
    /// `None` = 无 LLM 工厂（standalone / 子 agent 生命周期路径）：
    /// Prompt / Agent hook 被显式跳过（与 standalone 既有语义一致）。
    llm_factory: Option<Arc<dyn Fn() -> Box<dyn ReactLLM + Send + Sync> + Send + Sync>>,
    once_tracker: Arc<OnceTracker>,
    /// Agent hook 执行时的工作目录（对齐原 HookMiddleware.cwd）。
    cwd: String,
    task_manager: Option<Arc<dyn TaskManager>>,
}

impl HookDispatcher {
    pub fn new(
        hooks: Arc<RwLock<HashMap<HookEvent, Vec<RegisteredHook>>>>,
        llm_factory: Arc<dyn Fn() -> Box<dyn ReactLLM + Send + Sync> + Send + Sync>,
        once_tracker: Arc<OnceTracker>,
        cwd: String,
    ) -> Self {
        Self {
            hooks,
            llm_factory: Some(llm_factory),
            once_tracker,
            cwd,
            task_manager: None,
        }
    }

    /// 无 LLM 工厂的构造（standalone / 子 agent 生命周期）：Prompt / Agent hook 跳过。
    pub fn new_without_llm(
        registered_hooks: Vec<RegisteredHook>,
        once_tracker: Arc<OnceTracker>,
        cwd: String,
    ) -> Self {
        let mut map: HashMap<HookEvent, Vec<RegisteredHook>> = HashMap::new();
        for hook in registered_hooks {
            map.entry(hook.event.clone()).or_default().push(hook);
        }
        Self {
            hooks: Arc::new(RwLock::new(map)),
            llm_factory: None,
            once_tracker,
            cwd,
            task_manager: None,
        }
    }

    pub fn with_task_manager(mut self, task_manager: Arc<dyn TaskManager>) -> Self {
        self.task_manager = Some(task_manager);
        self
    }

    /// 可选注入会话 task manager（子 agent 生命周期路径按 host 是否有 manager 传入）。
    pub fn with_task_manager_opt(mut self, task_manager: Option<Arc<dyn TaskManager>>) -> Self {
        self.task_manager = task_manager;
        self
    }

    /// 子 agent 生命周期事件（SubagentStart / SubagentStop）的唯一分发入口。
    ///
    /// 复用完整分发语义：matcher 以子 agent 名为匹配目标、once、async spawn（含超时 /
    /// 取消 / 进程树 owner）、执行前查找与事件一致。
    ///
    /// 生命周期事件没有工具输入：带 `if` 条件的 hook 按 (子 agent 名, 空输入) 求值，
    /// 工具条件不满足即跳过，不允许"条件无效但静默执行"。
    ///
    /// **默认非阻断**：返回 action 仅供诊断；调用方不得据此阻断子 agent。
    pub async fn fire_subagent_lifecycle(
        &self,
        event: HookEvent,
        input: &HookInput,
        subagent_name: &str,
    ) -> HookAction {
        debug_assert!(
            matches!(event, HookEvent::SubagentStart | HookEvent::SubagentStop),
            "fire_subagent_lifecycle 仅服务子 agent 生命周期事件"
        );
        let empty_input = serde_json::Value::Object(Default::default());
        self.fire_event(event, input, Some(subagent_name), Some(&empty_input))
            .await
    }

    /// 分发一次 hook 事件。
    ///
    /// 流程：
    /// 1. 修正 `hook_event_name`（见下方 [TRAP]）
    /// 2. 查找匹配 hooks
    /// 3. 对每个 hook：matcher check → if-condition check → once 原子预留 → 执行
    /// 4. 归约 action，Block/PreventContinuation 短路
    pub async fn fire_event(
        &self,
        event: HookEvent,
        input: &HookInput,
        tool_name: Option<&str>,
        tool_input: Option<&serde_json::Value>,
    ) -> HookAction {
        // 确保 hook_event_name 与实际触发的事件一致。
        //
        // 调用方可能在 before_tool 中复用同一个 HookInput 连续触发多个事件
        // （PreToolUse → PermissionRequest → Notification），而 HookInput::tool_call()
        // 构造函数硬编码 hook_event_name = PreToolUse。若不修正，PermissionRequest hook
        // 脚本从 stdin 读到的 hook_event_name 会是 "PreToolUse" 而非 "PermissionRequest"。
        //
        // [TRAP] 即便 input_builder 修复了硬编码，dispatcher 仍保留兜底逻辑
        // （防御性编程）：外部代码（standalone hooks、stages::compact 等）可能传入
        // 未修正的 input。
        let input = if input.hook_event_name != event {
            let mut corrected = input.clone();
            corrected.hook_event_name = event.clone();
            corrected
        } else {
            input.clone()
        };

        let hooks = {
            let map = self.hooks.read();
            match map.get(&event) {
                Some(h) => {
                    tracing::debug!(
                        event = ?event,
                        count = h.len(),
                        "HookMiddleware: found hooks for event"
                    );
                    h.clone()
                }
                None => {
                    return HookAction::Allow;
                }
            }
        };

        if hooks.is_empty() {
            return HookAction::Allow;
        }

        let mut final_action = HookAction::Allow;

        for registered in &hooks {
            // matcher check
            if let Some(name) = tool_name {
                let matcher_str = registered.matcher.as_deref().unwrap_or_else(|| {
                    registered
                        .hook
                        .get_matcher()
                        .map(|s| s.as_str())
                        .unwrap_or("*")
                });
                if !matches_matcher(matcher_str, name) {
                    continue;
                }
            }

            // if condition check
            if let Some(condition) = registered.hook.get_condition() {
                if let (Some(name), Some(inp)) = (tool_name, tool_input) {
                    if !matches_if_condition(condition, name, inp) {
                        continue;
                    }
                }
            }

            // once 原子预留（在 matcher / if 之后：不匹配的触发不得消耗预留）。
            // [TRAP] 预留与执行之间不得再插入"查-标记"两步：生命周期闭包经
            // `tokio::spawn` 分离触发，重叠触发必须在这里被单锁去重，否则 once
            // hook 会执行多次；预留即消费，执行失败/取消不重试。
            if OnceTracker::is_once_hook(&registered.hook)
                && !self.once_tracker.try_reserve(registered)
            {
                continue;
            }

            // Execute hook (async hooks are spawned in background, result ignored)
            if let Some(ref msg) = registered.hook.get_status_message() {
                tracing::info!(
                    plugin = %registered.plugin_name,
                    event = ?event,
                    "Hook status: {}",
                    msg
                );
            }
            let action = if registered.hook.is_async() {
                if let Err(error) =
                    spawn_async_hook(registered.clone(), input.clone(), self.task_manager.clone())
                {
                    tracing::warn!(%error, "Async hook rejected by session execution scope");
                }
                HookAction::Allow
            } else {
                self.execute_sync(&registered.hook, &input, registered)
                    .await
            };

            // Block / PreventContinuation 保持 fail-closed 短路
            if matches!(
                action,
                HookAction::Block { .. } | HookAction::PreventContinuation { .. }
            ) {
                return action;
            }

            // 其余结果按字段归并：deny/updatedInput/context/messages 互不吞并
            final_action = merge_hook_actions(final_action, action);
        }

        final_action
    }

    /// 同步执行单个 hook（按 hook 类型分发到对应 executor）。
    async fn execute_sync(
        &self,
        hook: &HookType,
        input: &HookInput,
        registered: &RegisteredHook,
    ) -> HookAction {
        match hook {
            HookType::Command { .. } => {
                execute_command_hook_owned(hook, input, registered, self.task_manager.as_deref())
                    .await
            }
            HookType::Prompt { .. } => match &self.llm_factory {
                Some(factory) => execute_prompt_hook(hook, input, factory).await,
                None => {
                    tracing::debug!(
                        event = ?input.hook_event_name,
                        "Prompt hook skipped: dispatcher has no LLM factory"
                    );
                    HookAction::Allow
                }
            },
            HookType::Http { .. } => execute_http_hook(hook, input).await,
            HookType::Agent { .. } => match &self.llm_factory {
                Some(factory) => execute_agent_hook(hook, input, factory, &self.cwd).await,
                None => {
                    tracing::debug!(
                        event = ?input.hook_event_name,
                        "Agent hook skipped: dispatcher has no LLM factory"
                    );
                    HookAction::Allow
                }
            },
        }
    }
}

/// 归并多个 hook 的结果：字段级合并，任何字段都不得被其它 hook 的结果吞掉。
///
/// - `Allow` 是"无判定、无字段"的零元：任一侧为 Allow 时取另一侧
/// - 判定取 [`PermissionDecision::merge_rank`] 更高者：
///   deny/非法 > ask > allow > passthrough（deny 优先于 updatedInput）
/// - `updated_input` / `system_message` 后者覆盖前者；
///   `additional_context` 按顺序拼接并有界截断
/// - 无判定但多类字段并存（如 updatedInput + systemMessage）→ 以 `Passthrough`
///   判定承载全部字段（与 output_parser 的 PreToolUse 组合一致），
///   消费方不会因为后一个 hook"无判定"而丢掉先出现的判定/字段
/// - Block / PreventContinuation 已在调用点短路，不会进入本函数；
///   `InitialUserMessage`（SessionStart 专用、投递未接线）保持后者覆盖
fn merge_hook_actions(acc: HookAction, next: HookAction) -> HookAction {
    match (&acc, &next) {
        // Allow 零元：判定与字段都不因对侧"无判定"而丢失。
        (HookAction::Allow, _) => next,
        (_, HookAction::Allow) => acc,
        // InitialUserMessage 无对应字段可承载（SessionStart 专用），保持后者覆盖。
        (HookAction::InitialUserMessage { .. }, _) | (_, HookAction::InitialUserMessage { .. }) => {
            next
        }
        _ => {
            let prev = MergeFields::from_action(&acc);
            let incoming = MergeFields::from_action(&next);
            merge_fields(prev, incoming)
        }
    }
}

/// 参与归并的字段集合（判定 + PreToolUse/上下文/系统消息字段）。
#[derive(Default)]
struct MergeFields {
    decision: Option<PermissionDecision>,
    reason: Option<String>,
    updated_input: Option<serde_json::Value>,
    additional_context: Option<String>,
    system_message: Option<String>,
}

impl MergeFields {
    fn from_action(action: &HookAction) -> Self {
        match action {
            HookAction::PermissionOverride {
                decision,
                reason,
                updated_input,
                additional_context,
                system_message,
            } => Self {
                decision: Some(decision.clone()),
                reason: reason.clone(),
                updated_input: updated_input.clone(),
                additional_context: additional_context.clone(),
                system_message: system_message.clone(),
            },
            HookAction::ModifyInput { new_input } => Self {
                updated_input: Some(new_input.clone()),
                ..Self::default()
            },
            HookAction::SystemMessage { message } => Self {
                system_message: Some(message.clone()),
                ..Self::default()
            },
            HookAction::AdditionalContext { context } => Self {
                additional_context: Some(context.clone()),
                ..Self::default()
            },
            // Allow / Block / PreventContinuation / InitialUserMessage 不进入字段归并。
            _ => Self::default(),
        }
    }
}

/// 对称归并两个字段集合：判定按 rank 取高者（同 rank 保持先出现者），字段互相吸收。
fn merge_fields(prev: MergeFields, incoming: MergeFields) -> HookAction {
    let (decision, reason) = match (prev.decision, incoming.decision) {
        (Some(prev_decision), Some(decision)) => {
            if decision.merge_rank() > prev_decision.merge_rank() {
                (Some(decision), incoming.reason)
            } else {
                (Some(prev_decision), prev.reason)
            }
        }
        (Some(decision), None) => (Some(decision), prev.reason),
        (None, decision) => (decision, incoming.reason),
    };

    // 后者覆盖前者（既有语义）；不同字段之间互不吞并。
    let updated_input = incoming.updated_input.or(prev.updated_input);
    let system_message = incoming.system_message.or(prev.system_message);
    let additional_context =
        merge_additional_context(prev.additional_context, incoming.additional_context);

    if let Some(decision) = decision {
        return HookAction::PermissionOverride {
            decision,
            reason,
            updated_input,
            additional_context,
            system_message,
        };
    }

    // 无判定：单一字段保持既有简单载体，多类字段以 Passthrough 承载，全部保留。
    let field_kinds = [
        updated_input.is_some(),
        additional_context.is_some(),
        system_message.is_some(),
    ]
    .iter()
    .filter(|present| **present)
    .count();
    match field_kinds {
        0 => HookAction::Allow,
        1 if updated_input.is_some() => HookAction::ModifyInput {
            new_input: updated_input.expect("checked above"),
        },
        1 if system_message.is_some() => HookAction::SystemMessage {
            message: system_message.expect("checked above"),
        },
        1 => HookAction::AdditionalContext {
            context: additional_context.expect("checked above"),
        },
        _ => HookAction::PermissionOverride {
            decision: PermissionDecision::Passthrough,
            reason: None,
            updated_input,
            additional_context,
            system_message,
        },
    }
}

/// additionalContext 归并上限：多个 hook 叠加也不产生无界上下文。
const MAX_ADDITIONAL_CONTEXT_BYTES: usize = 16 * 1024;

fn merge_additional_context(existing: Option<String>, incoming: Option<String>) -> Option<String> {
    match (existing, incoming) {
        (None, None) => None,
        (Some(existing), None) => Some(existing),
        (None, Some(incoming)) => Some(incoming),
        (Some(mut existing), Some(incoming)) => {
            if !existing.is_empty() {
                existing.push('\n');
            }
            existing.push_str(&incoming);
            if existing.len() > MAX_ADDITIONAL_CONTEXT_BYTES {
                existing.truncate(existing.floor_char_boundary(MAX_ADDITIONAL_CONTEXT_BYTES));
            }
            Some(existing)
        }
    }
}

/// Fire standalone lifecycle hooks outside of the middleware lifecycle.
///
/// Used by ACP lifecycle paths outside the agent ReAct loop:
/// - `SessionEnd`: on session close, including `/clear` and host shutdown
/// - `PreCompact` / `PostCompact`: before/after context compaction
/// - `Notification`: when agent needs user attention (e.g. AskUserQuestion)
///
/// The HookMiddleware instance is owned by the agent task and not accessible
/// from these code paths, so we dispatch hooks directly.
///
/// [TRAP] async spawn 分支一致性：standalone 路径无 LLM factory，Prompt/Agent
/// hook 被跳过（与 fire_event 的 async 分支语义一致——async 仅适用于 Command）。
#[allow(clippy::too_many_arguments)]
pub async fn fire_standalone_lifecycle_hooks(
    registered_hooks: &[RegisteredHook],
    event: HookEvent,
    cwd: &str,
    session_id: &str,
    transcript_path: &str,
    current_model: &str,
    message_count: Option<usize>,
    reason: Option<&str>,
) {
    fire_standalone_lifecycle_hooks_owned(
        registered_hooks,
        event,
        cwd,
        session_id,
        transcript_path,
        current_model,
        message_count,
        reason,
        None,
    )
    .await;
}

/// Run lifecycle hooks using their session's task manager.
/// SessionEnd awaits even asynchronous hooks in the environment's cleanup scope;
/// all other events preserve normal asynchronous dispatch.
#[allow(clippy::too_many_arguments)]
pub async fn fire_standalone_lifecycle_hooks_owned(
    registered_hooks: &[RegisteredHook],
    event: HookEvent,
    cwd: &str,
    session_id: &str,
    transcript_path: &str,
    current_model: &str,
    message_count: Option<usize>,
    reason: Option<&str>,
    task_manager: Option<Arc<dyn TaskManager>>,
) {
    // Filter hooks matching the event
    let matching: Vec<&RegisteredHook> = registered_hooks
        .iter()
        .filter(|h| h.event == event)
        .collect();

    if matching.is_empty() {
        return;
    }

    let input = match &event {
        HookEvent::SessionEnd => input_builder::session_end_standalone(
            session_id,
            transcript_path,
            cwd,
            current_model,
            reason,
        ),
        HookEvent::PreCompact | HookEvent::PostCompact => HookInput::compact(
            session_id,
            transcript_path,
            cwd,
            event.clone(),
            message_count.unwrap_or(0),
        ),
        // === P1-5 新增 standalone 事件分支 ===
        HookEvent::Setup => {
            input_builder::setup_standalone(session_id, transcript_path, cwd, current_model)
        }
        HookEvent::InstructionsLoaded => input_builder::instructions_loaded_standalone(
            session_id,
            transcript_path,
            cwd,
            current_model,
        ),
        HookEvent::ConfigChange => input_builder::config_change_standalone(
            session_id,
            transcript_path,
            cwd,
            current_model,
            "unknown",
        ),
        HookEvent::WorktreeCreate => input_builder::worktree_create_standalone(
            session_id,
            transcript_path,
            cwd,
            current_model,
            "unknown",
        ),
        HookEvent::WorktreeRemove => input_builder::worktree_remove_standalone(
            session_id,
            transcript_path,
            cwd,
            current_model,
            "unknown",
        ),
        HookEvent::CwdChanged => input_builder::cwd_changed_standalone(
            session_id,
            transcript_path,
            cwd,
            current_model,
            "unknown",
        ),
        HookEvent::Notification => {
            input_builder::notification_standalone(session_id, transcript_path, cwd, current_model)
        }
        _ => return,
    };

    for registered in matching {
        if let Some(ref msg) = registered.hook.get_status_message() {
            tracing::info!(
                plugin = %registered.plugin_name,
                event = ?event,
                "Hook status: {}",
                msg
            );
        }

        if registered.hook.is_async() && event != HookEvent::SessionEnd {
            if let Err(error) =
                spawn_async_hook(registered.clone(), input.clone(), task_manager.clone())
            {
                tracing::warn!(%error, "Async lifecycle hook rejected by session execution scope");
            }
            continue;
        }

        let _action = match &registered.hook {
            HookType::Command { .. } => {
                execute_command_hook_owned(
                    &registered.hook,
                    &input,
                    registered,
                    task_manager.as_deref(),
                )
                .await
            }
            HookType::Prompt { .. } => {
                // No LLM factory available in standalone context; skip
                HookAction::Allow
            }
            HookType::Http { .. } => execute_http_hook(&registered.hook, &input).await,
            HookType::Agent { .. } => {
                // No LLM factory available in standalone context; skip
                HookAction::Allow
            }
        };
    }
}

/// 异步 hook 输出的字段摘要（**非决策**：不生效、不投递、不追溯授权）。
///
/// 只保留字段名与长度，不携带 hook 正文。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct AsyncHookOutputSummary {
    /// 决策类输出（异步路径不会应用）：block / prevent_continuation / modify_input /
    /// permission_override。
    pub decision: Option<&'static str>,
    /// 需要投递但异步路径没有定义的字段名（systemMessage / additionalContext /
    /// initialUserMessage）。
    pub undelivered: Vec<&'static str>,
    /// 未投递字段的字节数合计（诊断用，不含正文）。
    pub undelivered_bytes: usize,
}

/// 分类异步 hook 的归并结果：完成（无输出）/ 决策被忽略 / 未定义投递。
pub(crate) fn async_hook_output_summary(action: &HookAction) -> AsyncHookOutputSummary {
    let mut summary = AsyncHookOutputSummary::default();
    match action {
        HookAction::Allow => {}
        HookAction::Block { .. } => summary.decision = Some("block"),
        HookAction::PreventContinuation { .. } => {
            summary.decision = Some("prevent_continuation");
        }
        HookAction::ModifyInput { .. } => summary.decision = Some("modify_input"),
        HookAction::PermissionOverride {
            additional_context,
            system_message,
            ..
        } => {
            summary.decision = Some("permission_override");
            if let Some(context) = additional_context {
                summary.undelivered.push("additionalContext");
                summary.undelivered_bytes += context.len();
            }
            if let Some(message) = system_message {
                summary.undelivered.push("systemMessage");
                summary.undelivered_bytes += message.len();
            }
        }
        HookAction::SystemMessage { message } => {
            summary.undelivered.push("systemMessage");
            summary.undelivered_bytes += message.len();
        }
        HookAction::AdditionalContext { context } => {
            summary.undelivered.push("additionalContext");
            summary.undelivered_bytes += context.len();
        }
        HookAction::InitialUserMessage { message } => {
            summary.undelivered.push("initialUserMessage");
            summary.undelivered_bytes += message.len();
        }
    }
    summary
}

fn async_hook_kind(hook: &HookType) -> &'static str {
    match hook {
        HookType::Command { .. } => "command",
        HookType::Prompt { .. } => "prompt",
        HookType::Http { .. } => "http",
        HookType::Agent { .. } => "agent",
    }
}

/// 记录异步 hook 终态：完成走 debug，决策忽略 / 未定义投递 / 取消走 warn。
///
/// 诊断只含字段名与长度，不含 hook 正文。
fn record_async_hook_outcome(
    event: &HookEvent,
    plugin: &str,
    hook_kind: &'static str,
    outcome: Result<HookAction, ()>,
) {
    let action = match outcome {
        Ok(action) => action,
        Err(()) => {
            tracing::warn!(
                event = ?event,
                plugin,
                hook_kind,
                outcome = "cancelled",
                "Async hook cancelled by session scope; result is not applied (non-decision hook)"
            );
            return;
        }
    };
    let summary = async_hook_output_summary(&action);
    if summary.decision.is_none() && summary.undelivered.is_empty() {
        tracing::debug!(
            event = ?event,
            plugin,
            hook_kind,
            outcome = "completed",
            "Async hook completed without output"
        );
        return;
    }
    let outcome = if summary.decision.is_some() {
        "decision_ignored"
    } else {
        "undelivered_output"
    };
    tracing::warn!(
        event = ?event,
        plugin,
        hook_kind,
        outcome,
        decision = summary.decision,
        undelivered_fields = ?summary.undelivered,
        undelivered_bytes = summary.undelivered_bytes,
        "Async hook output is not applied on the async path (non-decision hook; no delivery defined)"
    );
}

/// Async completion stays in the session scope while its command owner drains
/// the process tree independently of the ignored HookAction result.
fn spawn_async_hook(
    registered: RegisteredHook,
    input: HookInput,
    task_manager: Option<Arc<dyn TaskManager>>,
) -> Result<(), String> {
    let manager = task_manager.clone();
    let cancellation = manager
        .as_ref()
        .and_then(|manager| manager.execution_cancel_token());
    let event = input.hook_event_name.clone();
    let plugin = registered.plugin_name.clone();
    let hook_kind = async_hook_kind(&registered.hook);
    let task = async move {
        let execute = async {
            match &registered.hook {
                HookType::Command { .. } => {
                    execute_command_hook_owned(
                        &registered.hook,
                        &input,
                        &registered,
                        manager.as_deref(),
                    )
                    .await
                }
                HookType::Http { .. } => execute_http_hook(&registered.hook, &input).await,
                _ => HookAction::Allow,
            }
        };
        let outcome = if let Some(token) = cancellation {
            tokio::select! {
                biased;
                _ = token.cancelled() => Err(()),
                action = execute => Ok(action),
            }
        } else {
            Ok(execute.await)
        };
        record_async_hook_outcome(&event, &plugin, hook_kind, outcome);
    };
    match task_manager {
        Some(manager) => {
            manager.spawn_owned(Box::pin(task))?;
        }
        None => {
            tokio::spawn(task);
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "hook_action_merge_test.rs"]
mod hook_action_merge_tests;
