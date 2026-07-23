//! Hook 分发引擎 + standalone 路径。
//!
//! 把原本分散在 `HookMiddleware::fire_event` 与 `fire_standalone_lifecycle_hooks`
//! 中重复的"hook 查找 / matcher 过滤 / async spawn / 同步执行 / once 标记"
//! 收敛到一个 [`HookDispatcher`]。Standalone 路径通过相同的分发逻辑执行
//! （差异：无 LLM factory，因此 Prompt/Agent hook 被跳过）。

use std::{collections::HashMap, sync::Arc};

use parking_lot::RwLock;
use peri_agent::agent::react::ReactLLM;

use crate::hooks::{
    executor::{execute_agent_hook, execute_command_hook, execute_http_hook, execute_prompt_hook},
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
        }
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
                // Fire-and-forget: spawn in background, return Allow immediately
                let hook = registered.hook.clone();
                let owned_input = input.clone();
                let registered = registered.clone();
                tokio::spawn(async move {
                    let _ = match &hook {
                        HookType::Command { .. } => {
                            execute_command_hook(&hook, &owned_input, &registered).await
                        }
                        HookType::Http { .. } => execute_http_hook(&hook, &owned_input).await,
                        // Prompt/Agent hooks need LLM factory which can't be cloned into spawn;
                        // async only applies to Command per schema definition.
                        _ => HookAction::Allow,
                    };
                });
                HookAction::Allow
            } else {
                self.execute_sync(&registered.hook, &input, registered)
                    .await
            };

            // once mark
            if OnceTracker::is_once_hook(&registered.hook) {
                self.once_tracker.mark_fired(registered);
            }

            // Short-circuit on Block / PreventContinuation
            match &action {
                HookAction::Block { .. } | HookAction::PreventContinuation { .. } => return action,
                HookAction::ModifyInput { new_input } => {
                    final_action = HookAction::ModifyInput {
                        new_input: new_input.clone(),
                    };
                }
                HookAction::PermissionOverride { decision, reason } => {
                    // Phase 2: 权限覆盖决策暂不改变实际权限行为，仅记录
                    tracing::debug!(
                        "PermissionOverride from hook: {:?} (reason: {:?})",
                        decision,
                        reason
                    );
                    final_action = HookAction::PermissionOverride {
                        decision: decision.clone(),
                        reason: reason.clone(),
                    };
                }
                _ => {}
            }
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
            HookType::Command { .. } => execute_command_hook(hook, input, registered).await,
            HookType::Prompt { .. } => execute_prompt_hook(hook, input, &self.llm_factory).await,
            HookType::Http { .. } => execute_http_hook(hook, input).await,
            HookType::Agent { .. } => {
                execute_agent_hook(hook, input, &self.llm_factory, &self.cwd).await
            }
        }
    }
}

/// Fire standalone lifecycle hooks outside of the middleware lifecycle.
///
/// Used by the TUI layer for events that occur outside the agent ReAct loop:
/// - `SessionEnd`: when `/clear` resets the session
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

        if registered.hook.is_async() {
            // Fire-and-forget async hook
            let hook = registered.hook.clone();
            let input = input.clone();
            let registered = registered.clone();
            tokio::spawn(async move {
                let _ = match &hook {
                    HookType::Command { .. } => {
                        execute_command_hook(&hook, &input, &registered).await
                    }
                    HookType::Http { .. } => execute_http_hook(&hook, &input).await,
                    _ => HookAction::Allow,
                };
            });
            continue;
        }

        let _action = match &registered.hook {
            HookType::Command { .. } => {
                execute_command_hook(&registered.hook, &input, registered).await
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
