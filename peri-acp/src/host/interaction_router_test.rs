//! `SessionInteractionRouter` 单测：按归属选 endpoint / 投递失败原因的诚实性
//!（`NoOnlineEndpoint` / `OwnerUnmatched` / `AttachmentDetached`）/ View 附件纪元
//!（重挂接递增、旧纪元丢弃、detach 隔离与窗口到期）/ 连接关闭 detach 与 retire 语义。
//!
//! transport 是 stub（只记录 ACP 帧）；路由与门是真实对象。

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use peri_acp_types::identity::AttemptId;
use peri_acp_types::interaction::{
    ApprovalDecision, ApprovalItem, AttachmentId, ConnectionEpoch, InteractionContext,
    InteractionResponse, QuestionItem, TrustedInteractionOwner, UnansweredCause,
    UserInteractionBroker,
};
use peri_acp_types::mcp_apps_facade::{
    AttachmentDetachReason, ClientViewId, DETACH_REATTACH_WINDOW_MS, MAX_ATTACHMENTS_PER_SESSION,
};
use peri_acp_types::session::TurnId;
use serde_json::Value;

use super::{
    scope_host_request_connection, AttachRejection, AttachmentState, DetachVerdict,
    SessionInteractionRouter, ViewAttachRequest, MAX_ONLINE_ENDPOINTS,
};
use crate::transport::types::{AcpError, IncomingMessage, RequestId};
use crate::transport::AcpTransport;

/// 只记录 ACP 帧的 stub transport（审批应答由测试脚本给出）。
#[derive(Default)]
struct FrameTransport {
    frames: Mutex<Vec<(String, Value)>>,
    /// `Some(option_id)` ⇒ 对 `session/request_permission` 回该选项。
    permission_choice: Mutex<Option<String>>,
}

impl FrameTransport {
    fn with_choice(option: &str) -> Arc<Self> {
        let transport = Arc::new(Self::default());
        *transport.permission_choice.lock().unwrap() = Some(option.to_string());
        transport
    }

    fn frames(&self) -> Vec<(String, Value)> {
        self.frames.lock().unwrap().clone()
    }
}

/// `Arc<FrameTransport>` → `Arc<dyn AcpTransport>`（endpoint 持有 trait object）。
fn erased(transport: &Arc<FrameTransport>) -> Arc<dyn AcpTransport> {
    Arc::clone(transport) as Arc<dyn AcpTransport>
}

#[async_trait]
impl AcpTransport for FrameTransport {
    async fn send_request(&self, method: &str, params: Value) -> Result<Value, AcpError> {
        self.frames
            .lock()
            .unwrap()
            .push((method.to_string(), params.clone()));
        match method {
            "session/request_permission" => {
                let choice = self
                    .permission_choice
                    .lock()
                    .unwrap()
                    .clone()
                    .unwrap_or_else(|| "reject_once".to_string());
                Ok(serde_json::json!({
                    "outcome": { "outcome": "selected", "optionId": choice }
                }))
            }
            "elicitation/create" => Ok(serde_json::json!({
                "action": { "action": "cancel" }
            })),
            _ => Ok(Value::Null),
        }
    }

    async fn send_notification(&self, _method: &str, _params: Value) -> Result<(), AcpError> {
        Ok(())
    }

    async fn recv(&self) -> Option<IncomingMessage> {
        None
    }

    async fn send_response(
        &self,
        _id: RequestId,
        _result: Result<Value, AcpError>,
    ) -> Result<(), AcpError> {
        Ok(())
    }
}

fn router() -> SessionInteractionRouter {
    SessionInteractionRouter::new("session-1", Arc::new(tokio::sync::Mutex::new(())))
}

fn model_owner() -> TrustedInteractionOwner {
    TrustedInteractionOwner::model(
        "session-1",
        peri_acp_types::session::ExecutionBinding {
            turn_id: TurnId::new(),
            attempt_id: AttemptId::new(),
        },
    )
}

fn approval() -> InteractionContext {
    InteractionContext::Approval {
        items: vec![ApprovalItem {
            tool_call_id: "call-1".to_string(),
            tool_name: "Bash".to_string(),
            tool_input: serde_json::json!({"command": "ls"}),
        }],
    }
}

fn is_approve(response: &InteractionResponse) -> bool {
    matches!(
        response,
        InteractionResponse::Decisions(decisions)
            if matches!(decisions.as_slice(), [ApprovalDecision::Approve { .. }])
    )
}

/// 应答必须是 `Unanswered` 且原因**逐字**相符（原因即事实，不得互相挪用）。
fn is_unanswered(response: &InteractionResponse, expected: UnansweredCause) -> bool {
    matches!(
        response,
        InteractionResponse::Unanswered { cause } if *cause == expected
    )
}

fn is_no_online_endpoint(response: &InteractionResponse) -> bool {
    is_unanswered(response, UnansweredCause::NoOnlineEndpoint)
}

/// 附着一条 View 路由（真实分配面：namespace / 纪元由 router 给出）。
fn attach_view(
    router: &SessionInteractionRouter,
    attachment_id: &str,
    app_id: &str,
    connection_id: &str,
) -> super::SessionAttachment {
    router
        .attach_view(ViewAttachRequest {
            attachment_id: AttachmentId::new(attachment_id),
            app_id: app_id.to_string(),
            view_key: ClientViewId::new(attachment_id),
            connection_id: connection_id.to_string(),
        })
        .expect("在线连接的 View 必须可附着")
}

// ─── 无在线 endpoint：唯一生产者 ─────────────────────────────────────────────

#[tokio::test]
async fn no_online_endpoint_is_reported_only_when_none_is_online() {
    let router = router();
    let transport = FrameTransport::with_choice("allow_once");

    // 未注册任何连接：本端确实没有任何在线 endpoint ⇒ NoOnlineEndpoint，零帧。
    let response = router.request(model_owner(), approval()).await;
    assert!(
        is_no_online_endpoint(&response),
        "一个在线 endpoint 都没有 ⇒ Unanswered(NoOnlineEndpoint)，实际: {response:?}"
    );
    assert!(transport.frames().is_empty());

    // 注册连接并发布模型轮：可投递。
    router
        .register_connection("conn-1", transport.clone())
        .expect("注册首个连接");
    let token = router
        .publish_model_turn_for_transport(&erased(&transport))
        .expect("已注册连接必须能发布模型轮");
    let response = router.request(model_owner(), approval()).await;
    assert!(is_approve(&response), "实际: {response:?}");
    assert_eq!(transport.frames().len(), 1);
    assert_eq!(transport.frames()[0].0, "session/request_permission");

    // 清除模型轮：endpoint 仍在线，但没有一个符合本次归属 ⇒ OwnerUnmatched
    //（不得谎称「没有任何在线 endpoint」，中 2）。
    router.clear_model_turn(token);
    let response = router.request(model_owner(), approval()).await;
    assert!(
        is_unanswered(&response, UnansweredCause::OwnerUnmatched),
        "在线 endpoint 存在但本轮未发布 ⇒ OwnerUnmatched，实际: {response:?}"
    );
    assert_eq!(transport.frames().len(), 1, "未投递 ⇒ 零新增帧");

    // 连接 retire：本端重新回到「没有任何在线 endpoint」。
    router.retire_connection("conn-1");
    assert_eq!(router.online_endpoint_count(), 0);
    assert!(router
        .publish_model_turn_for_transport(&erased(&transport))
        .is_none());
    let response = router.request(model_owner(), approval()).await;
    assert!(is_no_online_endpoint(&response), "实际: {response:?}");
    assert_eq!(transport.frames().len(), 1);
}

#[tokio::test]
async fn model_turn_publish_requires_the_very_transport_of_the_registered_connection() {
    let router = router();
    let registered = FrameTransport::with_choice("allow_once");
    let other = FrameTransport::with_choice("allow_once");
    router
        .register_connection("conn-1", registered.clone())
        .expect("注册");

    // 未被注册的 transport 不能成为模型轮 endpoint（不猜、不串线）：本端**有**
    // 在线 endpoint（conn-1），只是还没有发布本轮 ⇒ 原因是归属不匹配。
    assert!(router
        .publish_model_turn_for_transport(&erased(&other))
        .is_none());
    let response = router.request(model_owner(), approval()).await;
    assert!(
        is_unanswered(&response, UnansweredCause::OwnerUnmatched),
        "未发布本轮 ⇒ OwnerUnmatched（不谎称没有在线 endpoint），实际: {response:?}"
    );
    assert!(other.frames().is_empty());

    let token = router
        .publish_model_turn_for_transport(&erased(&registered))
        .expect("注册过的 transport 可发布");
    assert_eq!(
        router.model_turn_connection().as_deref(),
        Some("conn-1"),
        "发布必须落在注册过的连接上"
    );
    router.clear_model_turn(token);
    assert!(router.model_turn_connection().is_none());
}

#[tokio::test]
async fn model_attribution_from_another_session_is_not_delivered() {
    let router = router();
    let transport = FrameTransport::with_choice("allow_once");
    router
        .register_connection("conn-1", transport.clone())
        .expect("注册");
    router
        .publish_model_turn_for_transport(&erased(&transport))
        .expect("发布");

    let foreign = TrustedInteractionOwner::model(
        "session-other",
        peri_acp_types::session::ExecutionBinding {
            turn_id: TurnId::new(),
            attempt_id: AttemptId::new(),
        },
    );
    let response = router.request(foreign, approval()).await;
    assert!(
        is_unanswered(&response, UnansweredCause::OwnerUnmatched),
        "归属会话与本端不符 ⇒ OwnerUnmatched（本端有在线 endpoint），实际: {response:?}"
    );
    assert!(transport.frames().is_empty());
}

#[tokio::test]
async fn clear_model_turn_only_clears_its_own_publish() {
    let router = router();
    let transport = FrameTransport::with_choice("allow_once");
    router
        .register_connection("conn-1", transport.clone())
        .expect("注册");

    let first = router
        .publish_model_turn_for_transport(&erased(&transport))
        .expect("第一次发布");
    let second = router
        .publish_model_turn_for_transport(&erased(&transport))
        .expect("第二次发布");
    // 先发布者清除不生效：当前 endpoint 归后来的发布者。
    router.clear_model_turn(first);
    assert!(router.model_turn_connection().is_some());
    router.clear_model_turn(second);
    assert!(router.model_turn_connection().is_none());
}

// ─── App 归属：attachment 路由与纪元 ────────────────────────────────────────

#[tokio::test]
async fn app_attachment_requires_matching_epoch_and_online_connection() {
    let router = router();
    let transport = FrameTransport::with_choice("allow_once");
    router
        .register_connection("conn-1", transport.clone())
        .expect("注册");
    let attachment = AttachmentId::new("att-1");
    let attached = attach_view(&router, "att-1", "app-1", "conn-1");
    let epoch = attached.epoch();
    assert_eq!(epoch, ConnectionEpoch::initial());
    assert_eq!(attached.attachment_id(), &attachment);
    assert_eq!(attached.app_id(), "app-1");
    assert_eq!(attached.connection_id(), Some("conn-1"));
    assert!(attached.is_deliverable());
    assert_eq!(router.lookup(&attachment).as_ref(), Some(&attached));

    // 当前纪元：投递。
    let current = TrustedInteractionOwner::app("app-1", attachment.clone(), epoch);
    let response = router.request(current, approval()).await;
    assert!(is_approve(&response), "实际: {response:?}");

    // 旧纪元迟到批准：丢弃（零新帧）。本端**有**在线 endpoint ⇒ 原因必须是
    // OwnerUnmatched，而不是「没有任何在线 endpoint」（中 2）。
    let stale = TrustedInteractionOwner::app("app-1", attachment.clone(), epoch.next());
    let response = router.request(stale, approval()).await;
    assert!(
        is_unanswered(&response, UnansweredCause::OwnerUnmatched),
        "纪元不符必须报「有 endpoint 但不匹配归属」，实际: {response:?}"
    );
    assert_eq!(transport.frames().len(), 1);

    // 未附着的 attachment：同样不投递，同样不谎称「没有在线 endpoint」。
    let unbound = TrustedInteractionOwner::app("app-2", "att-unknown", epoch);
    let response = router.request(unbound, approval()).await;
    assert!(
        is_unanswered(&response, UnansweredCause::OwnerUnmatched),
        "未知附件必须报归属不匹配，实际: {response:?}"
    );
    assert_eq!(transport.frames().len(), 1);
}

/// [A11] 重挂接递增纪元：旧纪元的批准失效，新纪元同 key 可继续；detach 只影响
/// 本 View —— 同 record 的**其他附件**照常投递（裁决 D6）。
#[tokio::test]
async fn reattach_bumps_epoch_and_detach_isolates_one_view() {
    let router = router();
    let first = FrameTransport::with_choice("allow_once");
    let second = FrameTransport::with_choice("allow_once");
    router
        .register_connection("conn-1", first.clone())
        .expect("注册 conn-1");
    router
        .register_connection("conn-2", second.clone())
        .expect("注册 conn-2");

    // 同一 App 的两个 View 各附着一条连接。
    let view_a = attach_view(&router, "att-a", "app-1", "conn-1");
    let view_b = attach_view(&router, "att-b", "app-1", "conn-2");
    let epoch_a = view_a.epoch();
    let owner_a = |epoch| TrustedInteractionOwner::app("app-1", "att-a", epoch);
    let owner_b = TrustedInteractionOwner::app("app-1", "att-b", view_b.epoch());

    // 两个 View 都能投递。
    assert!(is_approve(
        &router.request(owner_a(epoch_a), approval()).await
    ));
    assert!(is_approve(
        &router.request(owner_b.clone(), approval()).await
    ));
    assert_eq!((first.frames().len(), second.frames().len()), (1, 1));

    // detach att-a：本 View 不再可投递（且原因是 detach 事实，不是「没有 endpoint」，
    // 因为 conn-2 仍在线）；att-b 不受影响。
    assert_eq!(
        router.detach(
            &AttachmentId::new("att-a"),
            AttachmentDetachReason::ViewDetached
        ),
        DetachVerdict::Detached
    );
    let detached = router
        .lookup(&AttachmentId::new("att-a"))
        .expect("窗口内仍可查");
    assert_eq!(
        detached.epoch(),
        epoch_a,
        "detach 不改纪元（只有重挂接递增）"
    );
    let response = router.request(owner_a(epoch_a), approval()).await;
    assert!(
        is_unanswered(&response, UnansweredCause::AttachmentDetached),
        "detach 的 View 必须报 AttachmentDetached，实际: {response:?}"
    );
    assert!(is_approve(
        &router.request(owner_b.clone(), approval()).await
    ));
    assert_eq!((first.frames().len(), second.frames().len()), (1, 2));

    // 幂等：再次 detach 不产生状态转移。
    assert_eq!(
        router.detach(
            &AttachmentId::new("att-a"),
            AttachmentDetachReason::ViewDetached
        ),
        DetachVerdict::AlreadyDetached
    );

    // 重挂接：同一附件身份换到 conn-2，纪元递增 ⇒ 旧纪元继续被拒，新纪元可投递。
    let reattached = router
        .reattach_view(&AttachmentId::new("att-a"), "conn-2")
        .expect("窗口内必须可重挂接");
    assert_eq!(reattached.epoch(), epoch_a.next());
    assert_eq!(reattached.connection_id(), Some("conn-2"));
    assert_eq!(
        router
            .lookup(&AttachmentId::new("att-a"))
            .map(|a| a.epoch()),
        Some(epoch_a.next()),
        "表内纪元与返回一致（single authority）"
    );
    let stale = router.request(owner_a(epoch_a), approval()).await;
    assert!(
        is_unanswered(&stale, UnansweredCause::OwnerUnmatched),
        "旧纪元必须随重挂接失效，实际: {stale:?}"
    );
    assert!(
        is_approve(&router.request(owner_a(epoch_a.next()), approval()).await),
        "新纪元同 key 必须可继续"
    );
    assert_eq!(first.frames().len(), 1, "旧连接不得再收数据");
    assert_eq!(second.frames().len(), 3, "重挂接后的批准落在新连接");
}

/// 未注册 / 已离线的连接不得附着；同 ID 不得重建；已到期的 detach 条目不再可查。
#[tokio::test]
async fn attach_view_is_fail_closed_and_table_is_bounded() {
    let router = router();
    let transport = FrameTransport::with_choice("allow_once");

    // 未注册连接：显式拒绝（零条目）。
    assert_eq!(
        router.attach_view(ViewAttachRequest {
            attachment_id: AttachmentId::new("att-1"),
            app_id: "app-1".into(),
            view_key: ClientViewId::new("view-1"),
            connection_id: "conn-1".into(),
        }),
        Err(AttachRejection::EndpointUnavailable)
    );
    assert!(router.lookup(&AttachmentId::new("att-1")).is_none());

    router
        .register_connection("conn-1", transport.clone())
        .expect("注册");
    let first = attach_view(&router, "att-1", "app-1", "conn-1");
    // 同 ID 重建 = 把两个请求域混成一个：显式拒绝（重挂接走 reattach_view）。
    assert_eq!(
        router.attach_view(ViewAttachRequest {
            attachment_id: AttachmentId::new("att-1"),
            app_id: "app-1".into(),
            view_key: ClientViewId::new("view-1"),
            connection_id: "conn-1".into(),
        }),
        Err(AttachRejection::AttachmentExists)
    );
    assert_eq!(router.lookup(&AttachmentId::new("att-1")), Some(first));

    // 表上限（`MAX_ATTACHMENTS_PER_SESSION`）：满表显式拒绝，不淘汰在线附件。
    for index in 2..=MAX_ATTACHMENTS_PER_SESSION {
        attach_view(&router, &format!("att-{index}"), "app-1", "conn-1");
    }
    assert_eq!(
        router.attach_view(ViewAttachRequest {
            attachment_id: AttachmentId::new("att-overflow"),
            app_id: "app-1".into(),
            view_key: ClientViewId::new("view-overflow"),
            connection_id: "conn-1".into(),
        }),
        Err(AttachRejection::TableFull)
    );
    assert!(
        router.lookup(&AttachmentId::new("att-1")).is_some(),
        "满表不得淘汰已有 View"
    );

    // 连接 retire：附件 detach（不删除），但重新注册后可重挂接；已 detach 的
    // 旧连接不再是有效 endpoint。
    let detached = router.retire_connection("conn-1");
    assert_eq!(
        detached.len() as u64,
        MAX_ATTACHMENTS_PER_SESSION,
        "该连接承载的全部附件都被 detach"
    );
    assert_eq!(
        router.reattach_view(&AttachmentId::new("att-1"), "conn-1"),
        Err(AttachRejection::EndpointUnavailable),
        "离线连接不得作为重挂接目标（连接是重挂接的前置条件）"
    );

    // 在线连接 + 未知附件：显式 UnknownAttachment，且不创建条目。
    let fresh = FrameTransport::with_choice("allow_once");
    router
        .register_connection("conn-2", fresh)
        .expect("注册 conn-2");
    assert_eq!(
        router.reattach_view(&AttachmentId::new("att-unknown"), "conn-2"),
        Err(AttachRejection::UnknownAttachment)
    );
    assert!(router.lookup(&AttachmentId::new("att-unknown")).is_none());
    // 窗口内重挂接到新连接：纪元递增、namespace 不变（请求域身份稳定）。
    let before = router
        .lookup(&AttachmentId::new("att-1"))
        .expect("窗口内可查");
    let reattached = router
        .reattach_view(&AttachmentId::new("att-1"), "conn-2")
        .expect("在线连接必须可重挂接");
    assert_eq!(reattached.connection_id(), Some("conn-2"));
    assert_eq!(reattached.epoch(), before.epoch().next());
    assert_eq!(
        reattached.namespace(),
        before.namespace(),
        "重挂接不换请求域（同 ID 不得重建 namespace）"
    );
}

/// detach 窗口到期：条目被惰性清理，同 ID 不再是可重挂接的附件。
///
/// 探针用真实时钟（`peri_time::monotonic_now` = tokio 时钟，可被 `pause`/`advance`
/// 推进），不构造假窗口值——窗口常量仍是 §4.3 的单一权威。
#[tokio::test(start_paused = true)]
async fn detached_attachment_expires_after_the_bounded_window() {
    let router = router();
    let transport = FrameTransport::with_choice("allow_once");
    router
        .register_connection("conn-1", transport.clone())
        .expect("注册");
    let attachment = AttachmentId::new("att-1");
    let attached = attach_view(&router, "att-1", "app-1", "conn-1");
    assert_eq!(
        router.detach(&attachment, AttachmentDetachReason::ConnectionRetired),
        DetachVerdict::Detached
    );
    assert_eq!(
        router.lookup(&attachment).map(|a| a.epoch()),
        Some(attached.epoch()),
        "窗口内仍可重挂接（身份与纪元保留）"
    );

    tokio::time::advance(std::time::Duration::from_millis(
        DETACH_REATTACH_WINDOW_MS + 1,
    ))
    .await;
    assert_eq!(router.expire_detached(), 1, "到期条目必须被清理");
    assert!(
        router.lookup(&attachment).is_none(),
        "到期后同 ID 不再是可重挂接附件"
    );
    assert_eq!(
        router.reattach_view(&attachment, "conn-1"),
        Err(AttachRejection::UnknownAttachment),
        "到期后重挂接必须显式拒绝（不得以同 ID 复活旧请求域）"
    );
    assert_eq!(
        router.detach(&attachment, AttachmentDetachReason::ViewDetached),
        DetachVerdict::UnknownAttachment
    );
    // 清理出的槽位可再次附着（同 ID 允许，因为旧条目已销毁）。
    let reattached = attach_view(&router, "att-1", "app-1", "conn-1");
    assert_eq!(reattached.epoch(), ConnectionEpoch::initial());
    assert_eq!(router.expire_detached(), 0, "无到期条目时清理是 no-op");
}

/// `apps/close`：结束该 App 的全部 View 附件（终态，不留窗口），其他 App 不受影响。
#[tokio::test]
async fn close_app_attachments_is_terminal_and_scoped_to_one_app() {
    let router = router();
    let transport = FrameTransport::with_choice("allow_once");
    router
        .register_connection("conn-1", transport.clone())
        .expect("注册");
    attach_view(&router, "att-1", "app-1", "conn-1");
    attach_view(&router, "att-2", "app-1", "conn-1");
    attach_view(&router, "att-3", "app-2", "conn-1");

    let closed = router.close_app_attachments("app-1");
    assert_eq!(closed.len(), 2, "该 App 的全部 View 都被结束");
    assert!(router.lookup(&AttachmentId::new("att-1")).is_none());
    assert!(router.lookup(&AttachmentId::new("att-2")).is_none());
    assert!(
        router.lookup(&AttachmentId::new("att-3")).is_some(),
        "其他 App 的 View 不受影响"
    );
    let response = router
        .request(
            TrustedInteractionOwner::app("app-1", "att-1", ConnectionEpoch::initial()),
            approval(),
        )
        .await;
    assert!(
        is_unanswered(&response, UnansweredCause::OwnerUnmatched),
        "已关闭的 App 归属不再可投递，实际: {response:?}"
    );
    // 幂等：再次关闭不再产生转移。
    assert!(router.close_app_attachments("app-1").is_empty());
}

#[tokio::test]
async fn connection_close_detaches_only_its_own_attachments_and_endpoint() {
    let router = router();
    let first = FrameTransport::with_choice("allow_once");
    let second = FrameTransport::with_choice("allow_once");
    router
        .register_connection("conn-1", first.clone())
        .expect("注册 conn-1");
    router
        .register_connection("conn-2", second.clone())
        .expect("注册 conn-2");
    let att_one = AttachmentId::new("att-1");
    let att_two = AttachmentId::new("att-2");
    let epoch_one = attach_view(&router, "att-1", "app-1", "conn-1").epoch();
    let epoch_two = attach_view(&router, "att-2", "app-2", "conn-2").epoch();
    router
        .publish_model_turn_for_transport(&erased(&first))
        .expect("模型轮 = conn-1");

    let detached = router.retire_connection("conn-1");
    assert_eq!(detached, vec![att_one.clone()]);
    assert_eq!(router.online_endpoint_count(), 1);
    // detach 而非销毁：身份与纪元保留在有界窗口内（裁决 D6）。
    let detached_view = router.lookup(&att_one).expect("窗口内仍可查");
    assert!(matches!(
        detached_view.state(),
        AttachmentState::Detached { .. }
    ));
    assert_eq!(detached_view.epoch(), epoch_one, "detach 不改纪元");
    assert!(
        router.model_turn_connection().is_none(),
        "模型轮随连接关闭清除"
    );

    // 本连接已离线：att-1 的审批不再送达（也不改投 conn-2）。本端仍有在线
    // endpoint（conn-2）⇒ 原因是 detach 事实，不是「没有在线 endpoint」。
    let stale_owner = TrustedInteractionOwner::app("app-1", att_one, epoch_one);
    let response = router.request(stale_owner, approval()).await;
    assert!(
        is_unanswered(&response, UnansweredCause::AttachmentDetached),
        "实际: {response:?}"
    );
    assert!(first.frames().is_empty());

    // 其他连接与该 App 的其他调用不受影响。
    let live_owner = TrustedInteractionOwner::app("app-2", att_two, epoch_two);
    let response = router.request(live_owner, approval()).await;
    assert!(is_approve(&response), "实际: {response:?}");
    assert_eq!(second.frames().len(), 1);
}

#[tokio::test]
async fn endpoint_table_is_bounded_and_rejects_instead_of_evicting_online_peers() {
    let router = router();
    for index in 0..MAX_ONLINE_ENDPOINTS {
        let transport = FrameTransport::with_choice("allow_once");
        router
            .register_connection(&format!("conn-{index}"), transport)
            .expect("表未满时注册必须成功");
    }
    let overflow = FrameTransport::with_choice("allow_once");
    assert!(
        router
            .register_connection("conn-overflow", overflow)
            .is_err(),
        "表满必须显式拒绝（不得淘汰在线 endpoint）"
    );
    assert_eq!(router.online_endpoint_count(), MAX_ONLINE_ENDPOINTS);

    // 释放一个槽位后可再注册（retire 后的连接不再占位）。
    router.retire_connection("conn-0");
    let retry = FrameTransport::with_choice("allow_once");
    assert!(router.register_connection("conn-new", retry).is_ok());
}

// ─── Host 归属：请求作用域注入与唯一 endpoint 兜底 ──────────────────────────

#[tokio::test]
async fn host_request_routes_to_its_request_scope_connection() {
    let router = Arc::new(router());
    let first = FrameTransport::with_choice("allow_once");
    let second = FrameTransport::with_choice("allow_once");
    router
        .register_connection("conn-1", first.clone())
        .expect("注册 conn-1");
    router
        .register_connection("conn-2", second.clone())
        .expect("注册 conn-2");

    let owner = TrustedInteractionOwner::host("peri/mcp/invoke", 1);
    let router_for_scope = Arc::clone(&router);
    let response = scope_host_request_connection("conn-2".to_string(), async move {
        router_for_scope.request(owner, approval()).await
    })
    .await;
    assert!(is_approve(&response), "实际: {response:?}");
    assert!(first.frames().is_empty(), "必须只投递给请求所在连接");
    assert_eq!(second.frames().len(), 1);
}

#[tokio::test]
async fn host_request_without_scope_needs_a_unique_online_endpoint() {
    let router = router();
    let owner = TrustedInteractionOwner::host("peri/cron/scheduled-trigger", 1);

    // 零 endpoint：不投递。
    let response = router.request(owner.clone(), approval()).await;
    assert!(is_no_online_endpoint(&response), "实际: {response:?}");

    // 唯一在线 endpoint（调度路径的既有语义）：按唯一性投递。
    let only = FrameTransport::with_choice("allow_once");
    router
        .register_connection("conn-1", only.clone())
        .expect("注册");
    let response = router.request(owner.clone(), approval()).await;
    assert!(is_approve(&response), "实际: {response:?}");
    assert_eq!(only.frames().len(), 1);

    // 多 endpoint 且无请求作用域：fail-closed，不猜归属；本端**有**在线 endpoint
    // ⇒ 原因是归属不匹配，不是「没有在线 endpoint」。
    let second = FrameTransport::with_choice("allow_once");
    router
        .register_connection("conn-2", second.clone())
        .expect("注册");
    let response = router.request(owner, approval()).await;
    assert!(
        is_unanswered(&response, UnansweredCause::OwnerUnmatched),
        "多端点歧义必须报归属不匹配，实际: {response:?}"
    );
    assert_eq!(only.frames().len(), 1);
    assert!(second.frames().is_empty());
}

// ─── 提问路径：门的串行与超时语义沿用既有 broker ────────────────────────────

#[tokio::test]
async fn questions_use_the_same_session_gate_and_reach_the_routed_endpoint() {
    let router = router();
    let transport = FrameTransport::with_choice("allow_once");
    router
        .register_connection("conn-1", transport.clone())
        .expect("注册");
    router
        .publish_model_turn_for_transport(&erased(&transport))
        .expect("发布");

    let response = router
        .request(
            model_owner(),
            InteractionContext::Questions {
                requests: vec![QuestionItem {
                    id: "q1".to_string(),
                    header: "确认".to_string(),
                    question: "继续?".to_string(),
                    options: Vec::new(),
                    multi_select: false,
                }],
            },
        )
        .await;
    // stub 回 cancel 且未声明原因 ⇒ 沿用既有 elicitation 语义（空答案）。
    match response {
        InteractionResponse::Answers(answers) => {
            assert_eq!(answers.len(), 1);
            assert_eq!(answers[0].id, "q1");
            assert!(answers[0].selected.is_empty());
            assert_eq!(answers[0].text.as_deref(), Some(""));
        }
        other => panic!("expected Answers, got {other:?}"),
    }
    assert_eq!(transport.frames().len(), 1);
    assert_eq!(transport.frames()[0].0, "elicitation/create");
}

#[test]
fn ask_user_timeout_defaults_are_shared_with_the_acp_broker() {
    // 路由不自造超时策略：沿用既有 `ask_user_timeout()` 解析（env 缺失 = 300s）。
    let router = router();
    assert_eq!(
        router.timeout,
        crate::broker::ask_user_timeout(),
        "router 与 AcpTransportBroker 必须共用同一超时事实源"
    );
}
