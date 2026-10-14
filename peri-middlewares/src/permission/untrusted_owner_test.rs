//! 审批 fail-closed 的回归测试：**缺少可信归属**（无真实 session / turn 事实）时，
//! 审批必须显式拒绝，不得静默放行、不得伪造归属、不得把不需要审批的调用连带拒绝。
//!
//! 归属由 `crate::interaction_owner` 单一权威铸造；本文件锁定两个消费点：
//! 单条审批（`before_tool` → `broker_approve`）与批量审批
//!（`before_tools_batch` → `batch_broker_approve`）。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use peri_agent::agent::state::AgentState;

use super::*;

/// 记录交互次数、被调用即失败的 broker：无归属的路径**不得**产生任何 broker 交互。
struct PanickingBroker {
    calls: AtomicUsize,
}

impl PanickingBroker {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            calls: AtomicUsize::new(0),
        })
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl UserInteractionBroker for PanickingBroker {
    async fn request(
        &self,
        _owner: TrustedInteractionOwner,
        _ctx: InteractionContext,
    ) -> InteractionResponse {
        self.calls.fetch_add(1, Ordering::SeqCst);
        panic!("无归属的审批不得发出任何 broker 交互");
    }
}

/// 无归属的审批态：既不声明 session 身份，也不声明 turn 身份（子 agent 自建
/// session 的真实形状）。
fn state_without_owner() -> AgentState {
    AgentState::new("/tmp")
}

/// 有归属的审批态（正例对照）：真实 session + turn/attempt 事实。
fn state_with_owner() -> AgentState {
    AgentState::new("/tmp").with_turn_identity(
        "session-test",
        peri_acp_types::session::ExecutionBinding {
            turn_id: peri_acp_types::session::TurnId::new(),
            attempt_id: peri_acp_types::identity::AttemptId::new(),
        },
    )
}

fn call(id: &str, name: &str) -> ToolCall {
    ToolCall::new(id, name, serde_json::json!({}))
}

fn assert_untrusted_rejection(result: &AgentResult<ToolCall>) {
    match result {
        Err(AgentError::ToolRejected { reason, .. }) => {
            assert_eq!(
                reason,
                crate::interaction_owner::UNTRUSTED_OWNER_REASON,
                "拒绝原因必须是单一权威的无归属说明"
            );
        }
        other => panic!("无归属必须显式拒绝（fail-closed），实际: {other:?}"),
    }
}

/// [回归] 单条审批：无归属 ⇒ 显式拒绝，且零 broker 交互（不弹窗、不静默放行）。
#[tokio::test]
async fn single_approval_without_trusted_owner_is_rejected_without_broker_interaction() {
    let broker = PanickingBroker::new();
    let mw = PermissionMiddleware::new(broker.clone(), default_requires_approval);
    let mut state = state_without_owner();

    let result = mw.before_tool(&mut state, &call("c1", "Write")).await;

    assert_untrusted_rejection(&result);
    assert_eq!(broker.calls(), 0, "无归属不得产生 broker 交互");
}

/// [回归] 批量审批：无归属 ⇒ 待审批条目逐条显式拒绝，**不需要审批的调用不受影响**
/// （结果长度与输入一致，粒度与正常路径相同）。
#[tokio::test]
async fn batch_approval_without_trusted_owner_rejects_only_calls_needing_approval() {
    let broker = PanickingBroker::new();
    let mw = PermissionMiddleware::new(broker.clone(), default_requires_approval);
    let mut state = state_without_owner();

    // `Write` 需要审批、`Read` 不需要：顺序让「过度拒绝」可判别。
    let calls = vec![call("c1", "Write"), call("c2", "Read")];
    let results = mw.before_tools_batch(&mut state, &calls).await;

    assert_eq!(results.len(), calls.len(), "结果必须与输入等长");
    assert_untrusted_rejection(&results[0]);
    assert!(
        results[1].is_ok(),
        "不需要审批的调用不得被无归属连带拒绝，实际: {:?}",
        results[1]
    );
    assert_eq!(broker.calls(), 0, "无归属不得产生 broker 交互");
}

/// 正例对照：归属齐备时批量审批照常发出一次交互（证明上面的拒绝来自归属缺失，
/// 而不是整条路径被短路）。
#[tokio::test]
async fn batch_approval_with_trusted_owner_still_reaches_the_broker() {
    let calls = Arc::new(AtomicUsize::new(0));
    struct ApprovingBroker(Arc<AtomicUsize>);
    #[async_trait]
    impl UserInteractionBroker for ApprovingBroker {
        async fn request(
            &self,
            _owner: TrustedInteractionOwner,
            ctx: InteractionContext,
        ) -> InteractionResponse {
            let InteractionContext::Approval { items } = ctx else {
                return InteractionResponse::Rejected;
            };
            self.0.fetch_add(1, Ordering::SeqCst);
            InteractionResponse::Decisions(vec![
                ApprovalDecision::Approve { source: None };
                items.len()
            ])
        }
    }

    let mw = PermissionMiddleware::new(
        Arc::new(ApprovingBroker(Arc::clone(&calls))),
        default_requires_approval,
    );
    let mut state = state_with_owner();
    let results = mw
        .before_tools_batch(&mut state, &[call("c1", "Write")])
        .await;

    assert!(results[0].is_ok(), "批准后必须放行");
    assert_eq!(calls.load(Ordering::SeqCst), 1, "有归属时恰好一次交互");
}
