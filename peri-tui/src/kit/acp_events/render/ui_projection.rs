//! Ui projection.

use super::super::{BridgeState, SessionPhase};
use crate::kit::atoms::{
    ACP_STATE, AT_MENTION_ACTIVE, AcpStateSnapshot, INPUT_BUFFER, POPUP_KIND, SLASH_HINT_ACTIVE,
    SUBMIT_TX,
};

use crate::kit::submit_request::SubmitRequest;

/// 将 BridgeState 中的状态快照写入 ACP_STATE Atom。
///
/// 仅在快照值变化时才写入——避免不必要的全树重渲染。
/// 流式期间 variant/is_loading 不变时，仅 view_count 变化；
/// popup 状态由各自的独立 atom 追踪（SLASH_HINT_ACTIVE 等），
/// 不应写入 ACP_STATE 导致 AppShell 重渲染。
pub(crate) fn push_acp_state(state: &mut BridgeState) {
    let snapshot = AcpStateSnapshot {
        variant: state.variant,
        view_count: state.committed.len() + state.current_turn.view_model_count(),
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
/// 排队项含附件（`BufferedInput`）——文本与附件一起出队，附件不得在排队路径丢失。
///
/// 调用时机：`TurnDone` 事件与取消复位（stale / 非 stale）——agent 结束本轮或
/// 复位，从队列里取出用户在 loading 期间缓存的 agent text 继续提交。若 buffer
/// 为空则 no-op；若 SUBMIT_TX 未初始化也安全跳过。
///
/// [Slice 3 D4] §10 queued 反转：排队项在 loading 期间**不进 transcript**（只
/// 显示在 composer 上方队列），drain 时镜像非 loading 提交路径——先
/// `send_local_user_bubble(text)`（本地气泡恰出现一次，不依赖服务端回显）
/// 再 `tx.send(AgentText)`；`handle_local_user_bubble` 的 last_submitted_text /
/// turn_generation 语义与非 loading 提交完全一致。
///
/// 多条输入的顺序保证：VecDeque + 顺序 `tx.send` + submit_consumer 会话车道 →
/// 严格 FIFO。第一条立即触发 prompt，同会话后续消息等上一条完成后提交；不同
/// 会话的车道互不等待。
pub(crate) fn drain_input_buffer() {
    if crate::kit::steer_state::is_enabled() {
        return;
    }
    let tx = SUBMIT_TX.get().cloned();
    if tx.is_none() {
        return;
    }

    let drained: Vec<crate::kit::atoms::BufferedInput> =
        INPUT_BUFFER.state().write().drain(..).collect();
    if let Some(tx) = tx {
        for input in drained {
            // [Slice 3 D4] 本地气泡 + 提交（镜像非 loading 路径）。
            // 附件与插件选择随排队项一起出队——排队不得成为图片或插件意图的丢弃点。
            crate::kit::input_area::send_local_user_bubble(&input.text, &input.attachments);
            let _ = tx.send(SubmitRequest::AgentText {
                text: input.text,
                attachments: input.attachments,
                plugin_selections: input.plugin_selections,
            });
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
