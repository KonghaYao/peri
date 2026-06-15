//! Compact 中间件 — 在 ReAct 循环内原地压缩上下文
//!
//! `before_model` 钩子: 每轮 LLM 调用前检查 token 阈值，超过时执行
//! micro/full compact。compact 后不改变控制流，ReAct 循环自然继续。

use parking_lot::Mutex;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use async_trait::async_trait;
use peri_agent::{
    agent::{
        compact::{
            config::CompactConfig, extract_file_info, extract_skill_names, full_compact,
            micro_compact_enhanced, re_inject,
        },
        events::AgentEvent as ExecutorEvent,
        state::State,
        token::ContextBudget,
        AgentCancellationToken,
    },
    error::AgentResult,
    llm::BaseModel,
    messages::BaseMessage,
    middleware::r#trait::Middleware,
};
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::hooks::{self, RegisteredHook};

/// Compact 中间件
///
/// 在 `before_model` 钩子中检查 token 使用量，触发 micro/full compact。
/// full compact 使用 LLM 生成对话摘要，re_inject 恢复关键文件/skills。
pub struct CompactMiddleware {
    /// LLM 模型（full compact 摘要生成用），None 则跳过 full compact
    model: Option<Arc<dyn BaseModel>>,
    /// Compact 配置
    config: CompactConfig,
    /// 上下文窗口预算
    budget: ContextBudget,
    /// 工作目录（re_inject 需要）
    cwd: String,
    /// 事件通道
    event_tx: Arc<Mutex<Option<mpsc::UnboundedSender<ExecutorEvent>>>>,
    /// 取消令牌
    cancel: AgentCancellationToken,
    /// Hooks（PreCompact/PostCompact）
    hooks: Vec<RegisteredHook>,
    /// Session ID（hook 上下文）
    session_id: String,
    /// Provider 名称（hook 上下文）
    provider_name: String,
    /// micro compact 在当前 prompt 执行中是否已触发过。
    /// 每次 execute_prompt 创建新的 CompactMiddleware 实例，天然 per-prompt 作用域。
    /// 防止 micro compact 反复触发（压缩量 < 新增量时会在每轮重复触发）。
    micro_compact_done: AtomicBool,
}

impl CompactMiddleware {
    /// 创建新的 CompactMiddleware
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        model: Option<Arc<dyn BaseModel>>,
        config: CompactConfig,
        budget: ContextBudget,
        cwd: String,
        event_tx: Arc<Mutex<Option<mpsc::UnboundedSender<ExecutorEvent>>>>,
        cancel: AgentCancellationToken,
        hooks: Vec<RegisteredHook>,
        session_id: String,
        provider_name: String,
    ) -> Self {
        Self {
            model,
            config,
            budget,
            cwd,
            event_tx,
            cancel,
            hooks,
            session_id,
            provider_name,
            micro_compact_done: AtomicBool::new(false),
        }
    }

    fn is_disabled(&self) -> bool {
        std::env::var("DISABLE_COMPACT").is_ok()
            || std::env::var("DISABLE_AUTO_COMPACT").is_ok()
            || !self.config.auto_compact_enabled
    }

    /// Reset per-turn state for reuse across prompts.
    pub fn reset(&self) {
        self.micro_compact_done
            .store(false, std::sync::atomic::Ordering::SeqCst);
    }

    fn send_event(&self, event: ExecutorEvent) {
        if let Some(tx) = self.event_tx.lock().as_ref() {
            let _ = tx.send(event);
        }
    }

    async fn fire_hooks(&self, event: hooks::types::HookEvent, msg_count: usize) {
        hooks::middleware::fire_standalone_lifecycle_hooks(
            &self.hooks,
            event,
            &self.cwd,
            &self.session_id,
            "",
            &self.provider_name,
            Some(msg_count),
        )
        .await
    }

    /// 执行 full compact：full_compact + re_inject + hooks + 事件
    /// 只对 own messages（ancestor_len..）执行 compact，ancestor 快照保持不变。
    async fn do_full_compact(&self, state: &mut impl State) -> AgentResult<()> {
        let model = match &self.model {
            Some(m) => m,
            None => {
                warn!("CompactMiddleware: model is None, skipping full compact");
                self.send_event(ExecutorEvent::CompactError {
                    message: "模型不可用，跳过压缩".to_string(),
                });
                return Ok(());
            }
        };

        let ancestor_len = state.ancestor_len();

        // 只 compact own messages，不动 ancestor 快照
        let own_messages: Vec<BaseMessage> = state.messages_mut().drain(ancestor_len..).collect();
        let msg_count = own_messages.len();

        info!(
            msg_count,
            ancestor_len, "CompactMiddleware: 触发 full compact"
        );

        // PreCompact hooks
        self.fire_hooks(hooks::types::HookEvent::PreCompact, msg_count)
            .await;

        self.send_event(ExecutorEvent::CompactStarted);

        // full_compact with cancellation
        let compact_result = tokio::select! {
            biased;
            _ = self.cancel.cancelled() => {
                self.send_event(ExecutorEvent::CompactError {
                    message: "已取消".to_string(),
                });
                self.fire_hooks(hooks::types::HookEvent::PostCompact, msg_count).await;
                // RESTORE: compact 被取消，own_messages 丢弃前放回 state
                state.messages_mut().extend(own_messages);
                return Ok(());
            }
            result = full_compact(&own_messages, model.as_ref(), &self.config, "", &self.cwd) => {
                match result {
                    Ok(r) => r,
                    Err(e) => {
                        warn!(error = %e, "CompactMiddleware: full_compact 失败");
                        self.send_event(ExecutorEvent::CompactError {
                            message: e.to_string(),
                        });
                        self.fire_hooks(hooks::types::HookEvent::PostCompact, msg_count).await;
                        // RESTORE: full_compact 失败，own_messages 丢弃前放回 state
                        state.messages_mut().extend(own_messages);
                        return Ok(());
                    }
                }
            }
        };

        // Cancel check before re_inject
        if self.cancel.is_cancelled() {
            self.send_event(ExecutorEvent::CompactError {
                message: "已取消".to_string(),
            });
            self.fire_hooks(hooks::types::HookEvent::PostCompact, msg_count)
                .await;
            // RESTORE: re_inject 前取消，own_messages 丢弃前放回 state
            state.messages_mut().extend(own_messages);
            return Ok(());
        }

        info!(
            summary_len = compact_result.summary.len(),
            "CompactMiddleware: full_compact 完成"
        );

        // re_inject
        let re_inject_result = tokio::select! {
            biased;
            _ = self.cancel.cancelled() => {
                self.send_event(ExecutorEvent::CompactError {
                    message: "已取消".to_string(),
                });
                self.fire_hooks(hooks::types::HookEvent::PostCompact, msg_count).await;
                // RESTORE: re_inject 被取消，own_messages 丢弃前放回 state
                state.messages_mut().extend(own_messages);
                return Ok(());
            }
            result = re_inject(&own_messages, &self.config, &self.cwd) => result,
        };

        info!(
            files_injected = re_inject_result.files_injected,
            skills_injected = re_inject_result.skills_injected,
            "CompactMiddleware: re_inject 完成"
        );

        let files = extract_file_info(&re_inject_result.messages);
        let skills = extract_skill_names(&re_inject_result.messages);

        // 摘要作为 Human 消息（与 Claude Code 实现对齐）。
        // 原因：LLM 适配器将 System 消息提取到 system 字段，不进入 messages 数组。
        // 若摘要为 System 类型，compact 后 messages 数组可能只有 system 角色消息，
        // DeepSeek/OpenAI 兼容 API 要求至少一条 user/assistant 消息，否则返回 400。
        let summary_content = format!(
            "<system-reminder>\n{}\n\n[上下文已压缩，请根据摘要继续工作]\n</system-reminder>",
            compact_result.summary
        );
        let mut new_messages = vec![BaseMessage::human(summary_content)];
        new_messages.extend(re_inject_result.messages.clone());

        self.send_event(ExecutorEvent::CompactCompleted {
            summary: compact_result.summary.clone(),
            files: files.clone(),
            skills: skills.clone(),
            micro_cleared: 0,
            messages: new_messages.clone(),
        });

        // PostCompact hooks
        self.fire_hooks(hooks::types::HookEvent::PostCompact, msg_count)
            .await;

        // Rebuild: ancestor (unchanged, still in state) + compacted own messages
        state.messages_mut().extend(new_messages);
        state.token_tracker_mut().reset();

        // Invalidate materialized context cache after compact
        if let (Some(store), Some(thread_id)) = (state.store(), state.own_thread_id()) {
            if let Err(e) = store.invalidate_context_cache(thread_id).await {
                tracing::warn!("failed to invalidate context cache after compact: {e}");
            }
        }

        info!("CompactMiddleware: full compact 完成，状态已更新");
        Ok(())
    }

    /// 执行 micro compact：原地压缩旧工具结果（仅 own messages）
    fn do_micro_compact(&self, state: &mut impl State) {
        let ancestor_len = state.ancestor_len();
        let messages = state.messages_mut();
        let cleared = micro_compact_enhanced(&self.config, messages, ancestor_len);
        if cleared > 0 {
            info!(cleared, "CompactMiddleware: micro-compact 完成");
            self.send_event(ExecutorEvent::CompactCompleted {
                summary: String::new(),
                files: vec![],
                skills: vec![],
                micro_cleared: cleared,
                messages: messages.to_vec(),
            });
        }
    }
}

#[async_trait]
impl<S: State> Middleware<S> for CompactMiddleware {
    fn name(&self) -> &str {
        "CompactMiddleware"
    }

    /// LLM 调用前：检查 token 阈值，必要时执行 compact
    async fn before_model(&self, state: &mut S) -> AgentResult<()> {
        if self.is_disabled() {
            return Ok(());
        }

        // Step 1: 不可变借用读取阈值（在块作用域内）
        let (should_full, should_micro) = {
            let tracker = state.token_tracker();
            let full = self.budget.should_auto_compact(tracker);
            // micro compact 每轮只触发一次，防止压缩量 < 新增量时的反复触发振荡
            let micro = !full
                && self.budget.should_warn(tracker)
                && !self.micro_compact_done.load(Ordering::Relaxed);
            (full, micro)
        };

        // Step 2: emit compact trigger metric before mutating state
        if should_full || should_micro {
            let tracker = state.token_tracker();
            let percentage = tracker
                .context_usage_percent(self.budget.context_window)
                .unwrap_or(0.0);
            peri_agent::metrics::emit(
                "trap.compact_trigger",
                serde_json::json!({
                    "trigger": if should_full { "full" } else { "micro" },
                    "tokens_used": tracker.estimated_context_tokens().unwrap_or(0),
                    "tokens_total": self.budget.context_window as u64,
                    "percentage": percentage,
                }),
                state.get_context("session_id"),
                state.get_context("run_id"),
            );
        }

        // Step 3: 可变借用（tracker 引用已 drop）
        if should_full {
            self.do_full_compact(state).await?;
        } else if should_micro {
            self.micro_compact_done.store(true, Ordering::Relaxed);
            self.do_micro_compact(state);
        }

        Ok(())
    }
}

#[cfg(test)]
#[path = "compact_middleware_test.rs"]
mod tests;
