//! 归属铸造规则的回归测试（单一权威 `interaction_owner`）。
//!
//! 覆盖两件事：
//! 1. 会话身份**不做** agent_id 之类的回退——缺失即「无归属」；
//! 2. 无归属 = fail-closed（[`untrusted_owner_error`] 显式拒绝，措辞可诊断）。

use peri_acp_types::identity::AttemptId;
use peri_acp_types::interaction::{InteractionAttribution, TrustedInteractionOwner};
use peri_acp_types::session::{ExecutionBinding, TurnId};
use peri_agent::error::AgentError;
use peri_agent::messages::BaseMessage;
use peri_agent::middleware::capabilities::{BeforeToolState, HookOutputState, StateView};

use super::*;

/// 审批 hook 状态替身：归属事实按用例显式声明（生产由 `BoundApprovalState` /
/// `AgentContext` 从真实 `InvocationContext` 投影）。
struct HookStateFixture {
    session_id: Option<&'static str>,
    attribution: Option<InteractionAttribution>,
}

impl StateView for HookStateFixture {
    fn execution_binding(&self) -> Option<ExecutionBinding> {
        self.attribution
            .as_ref()
            .and_then(|attribution| match attribution {
                InteractionAttribution::ModelTurn { binding, .. } => Some(binding.clone()),
                _ => None,
            })
    }
    fn session_id(&self) -> Option<&str> {
        self.session_id
    }
    fn interaction_attribution(&self) -> Option<InteractionAttribution> {
        self.attribution.clone()
    }
    fn cwd(&self) -> &str {
        "/tmp"
    }
    fn messages(&self) -> &[BaseMessage] {
        &[]
    }
    fn current_step(&self) -> Option<usize> {
        self.attribution.is_some().then_some(0)
    }
}

/// 本 fixture 只驱动归属铸造，没有 hook 投递路径：被调用即显式失败。
impl HookOutputState for HookStateFixture {
    fn enqueue_hook_model_reminder(
        &self,
        _reminder: peri_acp_types::system_reminder::TrustedSystemReminder,
    ) {
        panic!("HookStateFixture has no hook delivery path");
    }

    fn enqueue_hook_client_notice(
        &self,
        _reminder: peri_acp_types::system_reminder::TrustedSystemReminder,
    ) {
        panic!("HookStateFixture has no hook delivery path");
    }

    fn enqueue_session_start_message(
        &self,
        _reminder: peri_acp_types::system_reminder::TrustedSystemReminder,
    ) {
        panic!("HookStateFixture has no hook delivery path");
    }
}

impl BeforeToolState for HookStateFixture {}

fn binding() -> ExecutionBinding {
    ExecutionBinding {
        turn_id: TurnId::new(),
        attempt_id: AttemptId::new(),
    }
}

/// [回归] 归属事实缺失时**不铸造**归属（审批入口据此 fail-closed）。
///
/// session 身份在场也不足以构成可信归属：归属是独立的事实（谁在问），不能从
/// session / turn 字段二次拼装，也不能回退成 agent_id 之类的非会话身份。
#[test]
fn hook_state_without_attribution_mints_no_owner() {
    let state = HookStateFixture {
        session_id: Some("session-a"),
        attribution: None,
    };
    assert_eq!(
        owner_from_hook_state(&state),
        None,
        "无归属事实必须返回 None，不得用 session / turn 字段拼装"
    );
}

/// 正例：真实 turn 事实齐备 ⇒ 铸造模型归属（会话身份来自归属本身）。
#[test]
fn hook_state_with_model_attribution_mints_model_owner() {
    let state = HookStateFixture {
        session_id: Some("session-a"),
        attribution: Some(InteractionAttribution::model_turn("session-a", binding())),
    };
    match owner_from_hook_state(&state).expect("真实事实必须铸造归属") {
        TrustedInteractionOwner::Model { session_id, .. } => assert_eq!(session_id, "session-a"),
        other => panic!("模型归属事实只铸造模型归属，实际: {other:?}"),
    }
}

/// 正例（idle 入口）：App 归属事实 ⇒ 铸造 App 归属（路由到该附件）。
#[test]
fn hook_state_with_app_attribution_mints_app_owner() {
    use peri_acp_types::interaction::{AttachmentId, ConnectionEpoch};

    let state = HookStateFixture {
        session_id: Some("session-a"),
        attribution: Some(InteractionAttribution::app_attachment(
            "app-1",
            "att-1",
            ConnectionEpoch::initial(),
        )),
    };
    match owner_from_hook_state(&state).expect("App 归属事实必须铸造归属") {
        TrustedInteractionOwner::App {
            app_id,
            attachment_id,
            connection_epoch,
        } => {
            assert_eq!(app_id, "app-1");
            assert_eq!(attachment_id, AttachmentId::new("att-1"));
            assert_eq!(connection_epoch, ConnectionEpoch::initial());
        }
        other => panic!("App 归属事实必须铸造 App 归属，实际: {other:?}"),
    }
}

/// [回归] 工具侧同理：canonical dispatch 未绑定归属事实时不铸造归属。
#[test]
fn tool_context_without_attribution_mints_no_owner() {
    let ctx = peri_agent::tools::ToolContext::new(&[], "/tmp")
        .with_session_identity("session-a", "turn-generation-a");
    assert_eq!(
        owner_from_tool_context(&ctx),
        None,
        "工具不得用 session / turn 字段拼装归属"
    );
}

/// 正例：canonical dispatch 绑定的真实事实 ⇒ 铸造模型归属。
#[test]
fn tool_context_with_real_facts_mints_model_owner() {
    let ctx = peri_agent::tools::ToolContext::new(&[], "/tmp")
        .with_session_identity("session-a", "turn-generation-a")
        .with_attribution(InteractionAttribution::model_turn("session-a", binding()));
    match owner_from_tool_context(&ctx).expect("真实事实必须铸造归属") {
        TrustedInteractionOwner::Model { session_id, .. } => assert_eq!(session_id, "session-a"),
        other => panic!("工具上下文只铸造模型归属，实际: {other:?}"),
    }
}

/// 无归属的拒绝必须点名工具、说明「未执行」，且与「用户拒绝」措辞不同。
#[test]
fn untrusted_owner_error_is_explicit_and_diagnosable() {
    let error = untrusted_owner_error("Bash");
    let AgentError::ToolRejected { tool, reason } = &error else {
        panic!("无归属必须显式拒绝（ToolRejected），实际: {error:?}");
    };
    assert_eq!(tool, "Bash");
    assert_eq!(reason, UNTRUSTED_OWNER_REASON);
    assert!(reason.contains("缺少可信归属"), "原因必须可诊断: {reason}");
    assert!(reason.contains("未执行"), "必须说明操作未执行: {reason}");
    assert!(
        !reason.contains("用户拒绝"),
        "无归属不是用户拒绝，不得伪装: {reason}"
    );
}
