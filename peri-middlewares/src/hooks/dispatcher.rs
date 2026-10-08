//! Hook 分发引擎 + standalone 路径。
//!
//! 把原本分散在 `HookMiddleware::fire_event` 与 `fire_standalone_lifecycle_hooks`
//! 中重复的"hook 查找 / matcher 过滤 / async spawn / 同步执行 / once 标记"
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
    types::{HookAction, HookEvent, HookInput, HookType, RegisteredHook},
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
    llm_factory: Arc<dyn Fn() -> Box<dyn ReactLLM + Send + Sync> + Send + Sync>,
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
            llm_factory,
            once_tracker,
            cwd,
            task_manager: None,
        }
    }

    pub fn with_task_manager(mut self, task_manager: Arc<dyn TaskManager>) -> Self {
        self.task_manager = Some(task_manager);
        self
    }

    /// 分发一次 hook 事件。
    ///
    /// 流程：
    /// 1. 修正 `hook_event_name`（见下方 [TRAP]）
    /// 2. 查找匹配 hooks
    /// 3. 对每个 hook：once check → matcher check → if-condition check → 执行
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
            // once check
            if OnceTracker::is_once_hook(&registered.hook)
                && self.once_tracker.was_fired(registered)
            {
                continue;
            }

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

            // once mark
            if OnceTracker::is_once_hook(&registered.hook) {
                self.once_tracker.mark_fired(registered);
            }

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
            HookType::Prompt { .. } => execute_prompt_hook(hook, input, &self.llm_factory).await,
            HookType::Http { .. } => execute_http_hook(hook, input).await,
            HookType::Agent { .. } => {
                execute_agent_hook(hook, input, &self.llm_factory, &self.cwd).await
            }
        }
    }
}

/// 归并多个 hook 的结果：字段级合并，任何字段都不得被其它 hook 的结果吞掉。
///
/// - 判定取 [`HookAction::PermissionOverride`] 中 `merge_rank` 更高者：
///   deny/非法 > ask > allow > passthrough（deny 优先于 updatedInput）
/// - `updated_input` / `system_message` 后者覆盖前者；
///   `additional_context` 按顺序拼接并有界截断
/// - Block / PreventContinuation 已在调用点短路，不会进入本函数
fn merge_hook_actions(acc: HookAction, next: HookAction) -> HookAction {
    match (acc, next) {
        (HookAction::Allow, other) => other,
        (
            HookAction::ModifyInput { new_input },
            HookAction::PermissionOverride {
                decision,
                reason,
                additional_context,
                system_message,
                ..
            },
        ) => HookAction::PermissionOverride {
            decision,
            reason,
            updated_input: Some(new_input),
            additional_context,
            system_message,
        },
        (
            HookAction::PermissionOverride {
                decision,
                reason,
                additional_context,
                system_message,
                ..
            },
            HookAction::ModifyInput { new_input },
        ) => HookAction::PermissionOverride {
            decision,
            reason,
            updated_input: Some(new_input),
            additional_context,
            system_message,
        },
        (
            HookAction::PermissionOverride {
                decision: prev_decision,
                reason: prev_reason,
                updated_input: prev_input,
                additional_context: prev_context,
                system_message: prev_message,
            },
            HookAction::PermissionOverride {
                decision,
                reason,
                updated_input,
                additional_context,
                system_message,
            },
        ) => {
            let (decision, reason) = if decision.merge_rank() > prev_decision.merge_rank() {
                (decision, reason)
            } else {
                (prev_decision, prev_reason)
            };
            HookAction::PermissionOverride {
                decision,
                reason,
                updated_input: updated_input.or(prev_input),
                additional_context: merge_additional_context(prev_context, additional_context),
                system_message: system_message.or(prev_message),
            }
        }
        // 其余单值 action 沿用「后者覆盖」语义
        (_, other) => other,
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
        if let Some(token) = cancellation {
            tokio::select! {
                biased;
                _ = token.cancelled() => {}
                _ = execute => {}
            }
        } else {
            let _ = execute.await;
        }
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
