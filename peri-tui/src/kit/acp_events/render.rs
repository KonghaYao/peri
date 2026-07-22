//! Render helper functions — push_view_models, push_acp_state, etc.

use super::*;
use crate::kit::submit_request::SubmitRequest;
use crate::kit::tui_render_unit::TuiRenderUnit;

/// 将 BridgeState 中的 ViewModels 写入 VIEW_MODELS Atom。
///
/// 从 `state.committed`（im::Vector）clone（O(1)引用计数）后逐条 push_back
/// `current_turn.view_models()`，构成扁平单层列表。generation 每次调用递增+1。
pub(crate) fn push_view_models(state: &mut BridgeState) {
    // [Diagnostic] 追踪 VIEW_MODELS 写入时机——配合 scroll diag 分析 submit/history 滚动问题
    let is_loading = state.phase == SessionPhase::PromptRunning;
    tracing::info!(
        target: "msg_scroll_diag",
        committed = state.committed.len(),
        current_turn = state.current_turn.view_models().len(),
        generation = state.generation,
        phase = ?state.phase,
        is_loading,
        "push_view_models: writing VIEW_MODELS atom",
    );
    let mut items = state.committed.clone();
    for vm in state.current_turn.view_models() {
        items.push_back(vm.clone());
    }

    // 只展开最后一个含 reasoning 的 assistant bubble，其余折叠。
    // [Bug 2 修复] 仅在 collapsed 实际改变时同步重算 content_hash——
    // reasoning.collapsed 已纳入 hash 公式（见 TuiAssistantBubble::compute_hash），
    // 不重算会导致按 hash 分片的渲染缓存命中旧值、折叠/展开后 UI 不刷新。
    // 仅在变化时重算避免每次 token 到达都遍历 hash。
    let mut found_last = false;
    for i in (0..items.len()).rev() {
        if let TuiRenderUnit::TuiAssistantBubble(bubble) = &mut items[i]
            && let Some(ref mut reasoning) = bubble.reasoning
        {
            let target_collapsed = found_last;
            if reasoning.collapsed != target_collapsed {
                reasoning.collapsed = target_collapsed;
                bubble.recompute_hash();
            }
            if !found_last {
                found_last = true;
            }
        }
    }

    state.generation = state.generation.wrapping_add(1);
    let snapshot = ViewModelsSnapshot {
        items,
        generation: state.generation,
    };
    *VIEW_MODELS.state().write() = snapshot;
}

/// 由 acp_bridge 在 BRIDGE_RESET_COUNTER 复位时调用——
/// 立即将空快照写入 VIEW_MODELS atom，防止其他 reader 读到旧 session 数据。
pub fn push_view_models_for_reset() {
    let snapshot = ViewModelsSnapshot {
        items: im::Vector::new(),
        generation: 0,
    };
    *VIEW_MODELS.state().write() = snapshot;
}

/// 将 BridgeState 中的状态快照写入 ACP_STATE Atom。
///
/// 仅在快照值变化时才写入——避免不必要的全树重渲染。
/// 流式期间 variant/is_loading 不变时，仅 view_count 变化；
/// popup 状态由各自的独立 atom 追踪（SLASH_HINT_ACTIVE 等），
/// 不应写入 ACP_STATE 导致 AppShell 重渲染。
pub(crate) fn push_acp_state(state: &mut BridgeState) {
    let snapshot = AcpStateSnapshot {
        variant: state.variant,
        view_count: state.committed.len() + state.current_turn.view_models().len(),
        is_loading: state.phase == SessionPhase::PromptRunning,
        wizard_active: false,
        at_mention_active: *AT_MENTION_ACTIVE.state().read(),
        slash_hint_active: *SLASH_HINT_ACTIVE.state().read(),
    };
    let state_ref = ACP_STATE.state();
    let mut acp = state_ref.write();
    if *acp != snapshot {
        *acp = snapshot;
    }
}

/// 将 BridgeState.popup_kind 写入 POPUP_KIND Atom（S7）。
pub(crate) fn push_popup_kind(state: &BridgeState) {
    *POPUP_KIND.state().write() = state.popup_kind;
}

/// 将 `INPUT_BUFFER` atom 中所有排队输入按入队顺序 drain，逐条发送到 SUBMIT_TX。
///
/// 调用时机：`TurnDone` 事件——agent 完成本轮，从队列里取出用户在 loading 期间
/// 缓存的 agent text 继续提交。若 buffer 为空则 no-op；若 SUBMIT_TX 未初始化也安全跳过。
///
/// 多条输入的顺序保证：VecDeque + 顺序 `tx.send` + submit_consumer 单消费者 →
/// 严格 FIFO。第一条立即触发 prompt，后续在 submit_consumer 内部顺序处理
/// （每条都等上一条的 RPC 完成）。
pub(crate) fn drain_input_buffer() {
    let tx = SUBMIT_TX.get().cloned();
    if tx.is_none() {
        return;
    }

    let drained: Vec<String> = INPUT_BUFFER.state().write().drain(..).collect();
    if let Some(tx) = tx {
        for text in drained {
            let _ = tx.send(SubmitRequest::AgentText(text));
        }
    }
}

/// 从 ACP SessionUpdate::Plan JSON 中提取 TodoItem 列表并写入 TODO_ITEMS atom。
///
/// 使用类型安全 serde 反序列化将 Plan JSON 映射为 TodoItem 列表。
/// Plan JSON 格式:
///   {"sessionUpdate":"plan","entries":[{"content":"Fix bug","status":"in_progress","priority":"medium"}]}
pub fn handle_plan_update(update: &serde_json::Value) {
    use crate::kit::message_area::{TodoItem, TodoStatus};
    use agent_client_protocol::schema::v1::{Plan, PlanEntryStatus};

    let plan: Plan = match serde_json::from_value(update.clone()) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(error = %e, "handle_plan_update: failed to deserialize Plan");
            return;
        }
    };

    tracing::debug!(
        entries_count = plan.entries.len(),
        "handle_plan_update: received Plan entries"
    );

    let items: Vec<TodoItem> = plan
        .entries
        .into_iter()
        .map(|e| {
            let status = match e.status {
                PlanEntryStatus::Pending => TodoStatus::Pending,
                PlanEntryStatus::InProgress => TodoStatus::InProgress,
                PlanEntryStatus::Completed => TodoStatus::Completed,
                _ => {
                    tracing::warn!(status = ?e.status, "handle_plan_update: unknown PlanEntryStatus, fallback to Pending");
                    TodoStatus::Pending
                }
            };
            TodoItem {
                content: e.content,
                status,
            }
        })
        .collect();

    tracing::debug!(
        "handle_plan_update: writing {} items to TODO_ITEMS",
        items.len()
    );
    *crate::kit::atoms::TODO_ITEMS.state().write() = items;
}
