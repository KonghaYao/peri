//! 统一 Langfuse 事件路由层。
//!
//! 定义 [`UnifiedLangfuseEvent`] 枚举（v1 ExecutorEvent + v2 RenderEvent/ObserveEvent
//! 的并集）与 [`LangfuseBridge`] 结构体，提供单一 `process_event` 入口。
//!
//! 所有 Langfuse 追踪事件只需在一处映射到 `LangfuseTracer` 方法，
//! 消除 v1 `forward_langfuse_event` 和 v2 `forward_langfuse_{render,state,observe}`
//! 双轨处理器。

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::Mutex;
use peri_agent::agent::events_v2::{ObserveEvent, RenderEvent};
use tracing;

use crate::langfuse::tracer::stages::{StageHandle, MAIN_AGENT_KEY};
use crate::langfuse::tracer::LangfuseTracer;

mod unified_event;
pub use unified_event::UnifiedLangfuseEvent;

// ── LangfuseBridge ────────────────────────────────────────────────────────────

/// 统一 Langfuse 事件桥接器。
///
/// 持有 `LangfuseTracer` 的共享引用，提供 `process_event` 单一入口。
/// `active_stage` 由桥接器内部管理（`parking_lot::Mutex<HashMap<String, StageHandle>>`，
/// key = 事件 agent_id），调用方无需关心 Stage 生命周期。
#[derive(Clone)]
pub struct LangfuseBridge {
    tracer: Arc<Mutex<LangfuseTracer>>,
    provider_display_name: String,
    /// 各 agent 活跃的 Stage Span 句柄（StageStarted→StageEnded 间持有）。
    /// 按 agent_id 隔离：并行 subagent 的 stage 事件交错到达时互不覆盖，
    /// StageEnded 精确配对到发起 agent 的 handle。
    /// 仅在 spawn_eventbus_forwarder 或 SubAgent forwarder 的 render/observe 分支中使用。
    active_stage: Arc<Mutex<HashMap<String, StageHandle>>>,
    /// 各 agent 最近一次 LlmCallStart 的 step（key = agent_id）。
    /// v1 `ExecutorEvent::LlmRetrying` 不携带 agent_id/step，而 v1 路径的 LLM
    /// 事件固定归属 MAIN_AGENT_KEY（见 from_executor_event），故 retry 查询
    /// 主 agent 自己的 step 记录；v2 ObserveEvent 路径无 LlmRetrying 变体，
    /// subagent 的 start 记录在其自身 key 下，不会覆盖主 agent 的 step。
    llm_start_steps: Arc<Mutex<HashMap<String, usize>>>,
    /// 活跃 subagent 注册表（C4 最小接入）：child_agent_id → 生命周期信息。
    /// Start 注册 / Stop 注销，仅验证事件到达与字段完整 + 计数；
    /// 归属逻辑在阶段②由 tracer registry 接管（此处不影响任何归属决策）。
    subagent_registry: Arc<Mutex<HashMap<String, SubagentLifecycle>>>,
    /// subagent 生命周期事件计数（C4 指标，供阶段②对照）：(start, stop)
    subagent_counters: Arc<Mutex<(u64, u64)>>,
}

/// C4 最小注册条目：SubagentStart 到达时记录的字段快照。
/// 阶段①只验证事件到达与字段完整（写入+注销）；字段读取在阶段②
/// （tracer registry 归属）接管，故暂允许 dead_code。
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub(crate) struct SubagentLifecycle {
    pub parent_agent_id: String,
    pub agent_name: String,
    pub is_background: bool,
}

impl LangfuseBridge {
    /// 构造新桥接器。
    ///
    /// `main_agent_id`:主 v2 session 的事件侧 AgentId(Some 时注入 tracer registry,
    /// 用于区分"主 agent 事件"与"未知 subagent 事件")。bridge2(SubAgent forwarder)
    /// 与 workflow 路径不需要主 agent 身份,传 None(registry 按"非注册成员即主"
    /// fallback,兼容旧测试,见 tracer registry 注释)。
    pub fn new(
        tracer: Arc<Mutex<LangfuseTracer>>,
        provider_display_name: String,
        main_agent_id: Option<String>,
    ) -> Self {
        if let Some(ref id) = main_agent_id {
            tracer.lock().set_main_agent_id(id.clone());
        }
        Self {
            tracer,
            provider_display_name,
            active_stage: Arc::new(Mutex::new(HashMap::new())),
            llm_start_steps: Arc::new(Mutex::new(HashMap::new())),
            subagent_registry: Arc::new(Mutex::new(HashMap::new())),
            subagent_counters: Arc::new(Mutex::new((0, 0))),
        }
    }

    /// 当前活跃 subagent 注册数量（C4 指标，供测试/阶段②对照）
    #[cfg(test)]
    pub(crate) fn active_subagent_count(&self) -> usize {
        self.subagent_registry.lock().len()
    }

    /// 生命周期事件计数（C4 指标，供测试/阶段②对照）
    #[cfg(test)]
    pub(crate) fn subagent_event_counts(&self) -> (u64, u64) {
        *self.subagent_counters.lock()
    }

    /// 处理统一 Langfuse 事件，转发到 `LangfuseTracer`。
    ///
    /// `active_stage` 用于 StageStarted/StageEnded 间的 `StageHandle` 传递。
    /// 仅 `spawn_eventbus_forwarder` 传入真实可变引用；其他调用方传入 `&mut None`。
    pub fn process_event(
        &self,
        event: &UnifiedLangfuseEvent,
        active_stage: &mut HashMap<String, StageHandle>,
    ) {
        let mut t = self.tracer.lock();
        match event {
            UnifiedLangfuseEvent::LlmCallStart {
                agent_id,
                step,
                messages,
                tools,
            } => {
                self.llm_start_steps.lock().insert(agent_id.clone(), *step);
                t.on_llm_start(agent_id, *step, messages, tools);
            }
            UnifiedLangfuseEvent::LlmRequestPayload {
                agent_id,
                step,
                body,
                ..
            } => {
                t.on_llm_request_payload(agent_id, *step, Arc::clone(body));
            }
            UnifiedLangfuseEvent::LlmCallEnd {
                agent_id,
                step,
                model,
                output,
                usage,
                request_id,
            } => {
                t.on_llm_end(
                    agent_id,
                    *step,
                    model,
                    &self.provider_display_name,
                    output,
                    usage.as_ref(),
                    request_id.as_deref(),
                );
            }
            UnifiedLangfuseEvent::LlmRetrying {
                attempt,
                max_attempts,
                delay_ms,
                error,
            } => {
                // v1 retry 事件无 agent_id/step：LLM 事件在 v1 路径固定归
                // MAIN_AGENT_KEY，step 取该 agent 最近一次 LlmCallStart 的记录。
                let step = self
                    .llm_start_steps
                    .lock()
                    .get(MAIN_AGENT_KEY)
                    .copied()
                    .unwrap_or(0);
                t.on_llm_retrying(
                    MAIN_AGENT_KEY,
                    step,
                    *attempt,
                    *max_attempts,
                    *delay_ms,
                    error,
                );
            }
            UnifiedLangfuseEvent::TextChunk { chunk } => {
                t.on_text_chunk(chunk);
            }
            UnifiedLangfuseEvent::ToolStart {
                agent_id,
                tool_call_id,
                name,
                input,
            } => {
                t.on_tool_start(agent_id, tool_call_id, name, input);
            }
            UnifiedLangfuseEvent::ToolEnd {
                agent_id,
                tool_call_id,
                output,
                is_error,
            } => {
                t.on_tool_end(agent_id, tool_call_id, output, *is_error);
            }
            UnifiedLangfuseEvent::CompactStarted { strategy, trigger } => {
                t.on_compact_start(*strategy, *trigger);
            }
            UnifiedLangfuseEvent::CompactEnded {
                summary,
                files_count,
                skills_count,
                micro_cleared,
                is_error,
                error_message,
                estimated_tokens_saved,
                estimated_tokens_before,
                estimated_tokens_after,
                cache_hit_rate_before,
                full_escalation_reason,
                outcome,
            } => {
                // [P2-4] 本分支来自 ExecutorEvent::CompactCompleted 收敛映射时，
                // files_count/estimated_tokens_* 等数字字段为占位 0（未知/不可用，
                // 见上方映射注释），勿将本日志中的 0 读作真实观测。
                tracing::info!(
                    estimated_tokens_saved,
                    estimated_tokens_before,
                    estimated_tokens_after,
                    cache_hit_rate_before,
                    full_escalation_reason = ?full_escalation_reason,
                    outcome = ?outcome,
                    files_count,
                    skills_count,
                    "CompactCompleted"
                );
                t.on_compact_end(crate::langfuse::tracer::compact::CompactEndInfo {
                    summary: summary.clone(),
                    files_count: *files_count,
                    skills_count: *skills_count,
                    micro_cleared: *micro_cleared,
                    is_error: *is_error,
                    error_message: error_message.clone(),
                    estimated_tokens_saved: *estimated_tokens_saved,
                    estimated_tokens_before: *estimated_tokens_before,
                    estimated_tokens_after: *estimated_tokens_after,
                    cache_hit_rate_before: *cache_hit_rate_before,
                    full_escalation_reason: full_escalation_reason.clone(),
                    outcome: outcome.clone(),
                });
            }
            UnifiedLangfuseEvent::BudgetWarning {
                percentage,
                used_tokens,
                total_tokens,
                threshold_label,
            } => {
                t.on_budget_threshold_hit(
                    threshold_label,
                    *percentage,
                    *used_tokens,
                    *total_tokens,
                );
            }
            UnifiedLangfuseEvent::StageStarted {
                agent_id,
                stage,
                turn_id,
            } => {
                // 先释放 MutexGuard 再获取 trace_id/agent_observation_id。
                // 归属按事件 agent_id 查 tracer registry(替代旧栈顶近似):
                // ① 命中 by_agent_id → parent = 该 child 的 AGENT obs id;
                // ② main_agent_id 匹配(或未注入时非 registry 成员)→ 主 agent obs;
                // ③ 其余 → 注册闸门缓存(等 Start join 重放)或跳过(incomplete),
                //    绝不 fallback 主 agent。
                drop(t);
                let handle = {
                    let mut t2 = self.tracer.lock();
                    t2.on_stage_start_gated(agent_id, *stage, &turn_id.to_string())
                };
                if let Some(handle) = handle {
                    active_stage.insert(agent_id.clone(), handle);
                }
            }
            UnifiedLangfuseEvent::StageEnded { agent_id, status } => {
                // 按 agent_id 精确配对：只结束该 agent 自己的 handle，
                // 其他并行 subagent 的活跃 stage 不受影响。
                if let Some(handle) = active_stage.remove(agent_id) {
                    t.on_stage_end(agent_id, &handle, *status);
                } else {
                    // 乱序场景:StageStarted 被注册闸门缓存后重放,handle 在 tracer 侧
                    if let Some(handle) = t.take_replayed_stage_handle(agent_id) {
                        t.on_stage_end(agent_id, &handle, *status);
                    } else {
                        tracing::warn!(
                            target: "langfuse::forward",
                            %agent_id,
                            "StageEnded 无匹配的活跃 stage handle（可能事件乱序或已结束），跳过"
                        );
                    }
                }
            }
            UnifiedLangfuseEvent::MessageQueueDrained {
                agent_id,
                prompt,
                defer,
                info,
            } => {
                t.on_mq_drained(agent_id, *prompt, *defer, *info);
            }
            UnifiedLangfuseEvent::AiReasoningChunk { text } => {
                t.on_ai_reasoning_chunk(text);
            }
            UnifiedLangfuseEvent::TurnError { reason } => {
                t.on_turn_error(*reason);
            }
            UnifiedLangfuseEvent::SessionStarted { frozen_summary } => {
                t.on_session_start(frozen_summary);
            }
            UnifiedLangfuseEvent::MiddlewareStarted { mw_name, hook } => {
                t.on_middleware_start(mw_name, *hook);
            }
            UnifiedLangfuseEvent::MiddlewareEnded {
                mw_name,
                hook,
                status,
                error,
            } => {
                // 先释放 MutexGuard，查询活跃 middleware span
                drop(t);
                let span_id = {
                    let t2 = self.tracer.lock();
                    t2.middleware.find_active(mw_name, *hook)
                };
                if let Some(span_id) = span_id {
                    let handle = crate::langfuse::tracer::middleware::MiddlewareSpanHandle {
                        span_id,
                        name: mw_name.clone(),
                        hook: *hook,
                    };
                    self.tracer
                        .lock()
                        .on_middleware_end(&handle, *status, error.clone());
                } else {
                    tracing::warn!(
                        target: "langfuse::forward",
                        %mw_name,
                        ?hook,
                        "MiddlewareEnded without active middleware span, skipping"
                    );
                }
            }
            UnifiedLangfuseEvent::WorkflowStarted {
                workflow_id,
                plan_summary,
            } => {
                t.on_workflow_start(workflow_id, plan_summary);
            }
            UnifiedLangfuseEvent::WorkflowEnded {
                workflow_id,
                agents_spawned,
                tool_calls,
            } => {
                t.on_workflow_end(workflow_id, *agents_spawned, *tool_calls);
            }
            // SubagentStart/Stop:bridge 层保留 C4 注册/注销 + 日志 + 计数(指标),
            // 归属/生命周期由 tracer registry 接管(AGENT obs 创建/关闭)。
            UnifiedLangfuseEvent::SubagentStart {
                parent_agent_id,
                child_agent_id,
                agent_name,
                is_background,
            } => {
                let mut reg = self.subagent_registry.lock();
                if reg.contains_key(child_agent_id) {
                    tracing::warn!(
                        target: "langfuse::subagent",
                        %child_agent_id,
                        "SubagentStart 重复（child_agent_id 已有活跃记录），覆盖注册"
                    );
                }
                reg.insert(
                    child_agent_id.clone(),
                    SubagentLifecycle {
                        parent_agent_id: parent_agent_id.clone(),
                        agent_name: agent_name.clone(),
                        is_background: *is_background,
                    },
                );
                self.subagent_counters.lock().0 += 1;
                tracing::info!(
                    target: "langfuse::subagent",
                    event = "subagent_start",
                    %parent_agent_id,
                    %child_agent_id,
                    %agent_name,
                    is_background,
                    active = reg.len(),
                    "SubagentStart 注册"
                );
                drop(reg);
                // tracer registry:AGENT obs 创建(join 成功后) + gate 重放
                t.on_subagent_start(parent_agent_id, child_agent_id, agent_name, *is_background);
            }
            UnifiedLangfuseEvent::SubagentStop {
                parent_agent_id,
                child_agent_id,
                agent_name,
                result,
                is_error,
            } => {
                let mut reg = self.subagent_registry.lock();
                let was_registered = reg.remove(child_agent_id).is_some();
                self.subagent_counters.lock().1 += 1;
                tracing::info!(
                    target: "langfuse::subagent",
                    event = "subagent_stop",
                    %parent_agent_id,
                    %child_agent_id,
                    %agent_name,
                    is_error,
                    was_registered,
                    active = reg.len(),
                    result_len = result.len(),
                    "SubagentStop 注销"
                );
                if !was_registered {
                    tracing::warn!(
                        target: "langfuse::subagent",
                        %child_agent_id,
                        "SubagentStop 无对应 Start（丢失/乱序），阶段②走 incomplete 分支"
                    );
                }
                drop(reg);
                // tracer registry:AGENT obs 关闭(两信号齐备时)
                t.on_subagent_stop(parent_agent_id, child_agent_id, result, *is_error);
            }
        }
    }
}

// ── LangfuseBridge impl LangfuseBridgeLike ───────────────────────────────────

impl peri_agent::agent::LangfuseBridgeLike for LangfuseBridge {
    fn process_render_event(&self, ev: &RenderEvent) {
        if let Some(u) = UnifiedLangfuseEvent::from_render_event(ev.clone()) {
            let mut guard = self.active_stage.lock();
            self.process_event(&u, &mut guard);
        }
    }

    fn process_observe_event(&self, ev: &ObserveEvent) {
        if let Some(u) = UnifiedLangfuseEvent::from_observe_event(ev.clone()) {
            let mut guard = self.active_stage.lock();
            self.process_event(&u, &mut guard);
        }
    }
}

// ── C4 最小接入测试 ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use peri_agent::agent::LangfuseBridgeLike;

    fn make_bridge() -> (
        LangfuseBridge,
        std::sync::Arc<crate::langfuse::fake_session::FakeLangfuseSession>,
    ) {
        // FakeLangfuseSession::new() 已返回 Arc<Self>
        let session = crate::langfuse::fake_session::FakeLangfuseSession::new("sess_c4");
        let config = crate::langfuse::config::LangfuseConfig {
            public_key: None,
            secret_key: None,
            host: "https://cloud.langfuse.com".to_string(),
            trace_sampling: 0.0,
            error_span_always: true,
            batch_max_events: 50,
            batch_flush_interval_secs: 10,
            user_id: None,
        };
        let tracer = crate::langfuse::tracer::LangfuseTracer::new(
            session.clone(),
            "sess_c4".to_string(),
            config,
        );
        let bridge = LangfuseBridge::new(
            Arc::new(parking_lot::Mutex::new(tracer)),
            "test-provider".to_string(),
            None,
        );
        (bridge, session)
    }

    /// C4: v2 SubagentStart/Stop → Unified 映射字段完整（child/parent/name/bg/result/error）
    #[test]
    fn test_from_observe_event_subagent_start_stop_mapping() {
        use peri_acp_types::identity::AgentId;
        use peri_agent::session::turn::TurnId;

        let turn_id = TurnId::new();
        let parent = AgentId::new();
        let child = AgentId::new();

        let start = ObserveEvent::SubagentStart {
            turn_id,
            agent_id: parent,
            child_agent_id: child,
            agent_name: "code-reviewer".to_string(),
            is_background: true,
        };
        match UnifiedLangfuseEvent::from_observe_event(start) {
            Some(UnifiedLangfuseEvent::SubagentStart {
                parent_agent_id,
                child_agent_id,
                agent_name,
                is_background,
            }) => {
                assert_eq!(parent_agent_id, parent.to_string());
                assert_eq!(child_agent_id, child.to_string());
                assert_eq!(agent_name, "code-reviewer");
                assert!(is_background);
            }
            other => panic!("应为 SubagentStart，实际 {:?}", other),
        }

        let stop = ObserveEvent::SubagentStop {
            turn_id,
            agent_id: parent,
            child_agent_id: child,
            agent_name: "code-reviewer".to_string(),
            result: "done".to_string(),
            is_error: false,
        };
        match UnifiedLangfuseEvent::from_observe_event(stop) {
            Some(UnifiedLangfuseEvent::SubagentStop {
                parent_agent_id,
                child_agent_id,
                agent_name,
                result,
                is_error,
            }) => {
                assert_eq!(parent_agent_id, parent.to_string());
                assert_eq!(child_agent_id, child.to_string());
                assert_eq!(agent_name, "code-reviewer");
                assert_eq!(result, "done");
                assert!(!is_error);
            }
            other => panic!("应为 SubagentStop，实际 {:?}", other),
        }
    }

    /// C4: process_event 的 Start 注册 / Stop 注销 + 计数（归属逻辑未动）
    #[test]
    fn test_process_event_registers_and_deregisters() {
        use peri_acp_types::identity::AgentId;

        let (bridge, _session) = make_bridge();
        let mut active_stage = HashMap::new();
        let parent = AgentId::new();
        let child = AgentId::new();

        // Start → 注册 + 计数
        bridge.process_event(
            &UnifiedLangfuseEvent::SubagentStart {
                parent_agent_id: parent.to_string(),
                child_agent_id: child.to_string(),
                agent_name: "explorer".to_string(),
                is_background: false,
            },
            &mut active_stage,
        );
        assert_eq!(
            bridge.active_subagent_count(),
            1,
            "Start 后应有 1 个活跃注册"
        );
        assert_eq!(
            bridge.subagent_event_counts(),
            (1, 0),
            "Start 计数应为 (1, 0)"
        );

        // 重复 Start → 覆盖注册（不增加条目），计数仍递增
        bridge.process_event(
            &UnifiedLangfuseEvent::SubagentStart {
                parent_agent_id: parent.to_string(),
                child_agent_id: child.to_string(),
                agent_name: "explorer".to_string(),
                is_background: false,
            },
            &mut active_stage,
        );
        assert_eq!(
            bridge.active_subagent_count(),
            1,
            "重复 Start 不增加注册条目"
        );

        // Stop → 注销 + 计数
        bridge.process_event(
            &UnifiedLangfuseEvent::SubagentStop {
                parent_agent_id: parent.to_string(),
                child_agent_id: child.to_string(),
                agent_name: "explorer".to_string(),
                result: "found".to_string(),
                is_error: false,
            },
            &mut active_stage,
        );
        assert_eq!(bridge.active_subagent_count(), 0, "Stop 后注册应清空");
        assert_eq!(bridge.subagent_event_counts(), (2, 1));

        // 无对应 Start 的 Stop → 不 panic，计数仍递增（阶段② incomplete 分支）
        bridge.process_event(
            &UnifiedLangfuseEvent::SubagentStop {
                parent_agent_id: parent.to_string(),
                child_agent_id: AgentId::new().to_string(),
                agent_name: "ghost".to_string(),
                result: "lost".to_string(),
                is_error: true,
            },
            &mut active_stage,
        );
        assert_eq!(bridge.active_subagent_count(), 0);
        assert_eq!(bridge.subagent_event_counts(), (2, 2));
    }

    /// C4: 经 LangfuseBridgeLike 完整链路（forwarder 同入口）Start/Stop 可达
    #[test]
    fn test_bridge_like_process_observe_start_stop() {
        use peri_acp_types::identity::AgentId;
        use peri_agent::session::turn::TurnId;

        let (bridge, _session) = make_bridge();
        let parent = AgentId::new();
        let child = AgentId::new();
        let turn_id = TurnId::new();

        bridge.process_observe_event(&ObserveEvent::SubagentStart {
            turn_id,
            agent_id: parent,
            child_agent_id: child,
            agent_name: "plan".to_string(),
            is_background: false,
        });
        assert_eq!(bridge.active_subagent_count(), 1);

        bridge.process_observe_event(&ObserveEvent::SubagentStop {
            turn_id,
            agent_id: parent,
            child_agent_id: child,
            agent_name: "plan".to_string(),
            result: "done".to_string(),
            is_error: false,
        });
        assert_eq!(bridge.active_subagent_count(), 0);
        assert_eq!(bridge.subagent_event_counts(), (1, 1));
    }
}

#[cfg(test)]
#[path = "bridge_test.rs"]
mod bridge_test;
