//! idle App 回调 / Host invoke 的**用例集**（夹具与真实对象见
//! `mcp_apps_idle_test.rs`，同一模块树）。
//!
//! App 回调经 canonical 入口（与 `apps/request` façade 同一条链）驱动；Host invoke
//! 经 relay 的 `invoke_app`（= `peri/mcp/invoke` 的生产路径）。

use super::*;

/// [A12] idle App 回调：审批经 session 交互路由送达在线连接（owner = App），
/// 批准后恰好一次 wire 调用，结果逐字回传。
#[tokio::test]
async fn idle_app_callback_is_approved_by_session_router_and_runs_once() {
    let transport = FrameTransport::with_choice("allow_once");
    let fixture = Fixture::new(Arc::clone(&transport)).await;
    let view = fixture.instantiate("conn-1").await;
    let calls_before = fixture.calls();

    let raw = fixture
        .app_callback(&view)
        .await
        .expect("批准后必须真实执行");

    assert_eq!(
        fixture.calls(),
        calls_before + 1,
        "批准 ⇒ 恰好一次新增 wire 调用"
    );
    let frames = transport.permission_frames();
    assert_eq!(frames.len(), 1, "审批帧必须送达在线 endpoint");
    assert_eq!(frames[0]["sessionId"], SESSION);
    assert!(
        frames[0]["toolCall"]["title"]
            .as_str()
            .is_some_and(|title| title.contains(APP_TOOL)),
        "审批帧必须携带被审批的目标，实际: {}",
        frames[0]
    );
    assert_eq!(
        raw.into_value()["content"][0]["text"],
        json!("canvas ready"),
        "App 面必须拿到真实结果载荷（完整 raw，不经合成）"
    );
    assert!(matches!(
        fixture.latest_outcome(),
        SessionInvocationOutcome::Completed(_)
    ));
    fixture.shutdown().await;
}

/// 无在线 endpoint（连接关闭 / 从未注册）：显式**未执行**，零新增 wire 调用，
/// 且不是「用户拒绝」。
#[tokio::test]
async fn no_online_endpoint_is_not_executed_and_never_reported_as_user_rejection() {
    let transport = FrameTransport::with_choice("allow_once");
    let fixture = Fixture::new(Arc::clone(&transport)).await;
    let view = fixture.instantiate("conn-1").await;
    assert_eq!(fixture.calls(), 1, "只有实例化那一次 wire 调用");
    assert!(transport.permission_frames().is_empty());

    // 连接关闭：endpoint 离线、attachment 路由 detach（record 本身不由本层撤销）。
    assert_eq!(
        fixture.router.retire_connection("conn-1"),
        vec![view.attachment_id.clone()]
    );

    let kind = fixture
        .app_callback(&view)
        .await
        .expect_err("无在线 endpoint 不得执行");
    assert_kind(
        kind,
        McpAppsErrorKind::ApprovalUnavailable,
        "必须用独立错误码表达「未获批准」",
    );
    assert_eq!(fixture.calls(), 1, "零新增 wire 调用");
    assert!(transport.permission_frames().is_empty());
    let outcome = fixture.latest_outcome();
    assert_eq!(
        outcome,
        SessionInvocationOutcome::NotExecuted {
            cause: NotExecutedCause::ApprovalUnavailable(
                peri_acp_types::interaction::UnansweredCause::NoOnlineEndpoint
            )
        },
        "无在线 endpoint 必须是 ApprovalUnavailable(NoOnlineEndpoint)"
    );
    assert_ne!(
        outcome,
        SessionInvocationOutcome::NotExecuted {
            cause: NotExecutedCause::UserRejected
        },
        "「没有人可以决定」不得呈现为「用户拒绝」"
    );
    fixture.shutdown().await;
}

/// [A2 · ACP 层] 连接关闭不取消已 dispatch 的调用，也不影响其他连接与该 App
/// 的其他调用（record 保持附着，执行由 invocation owner 收尾）。
#[tokio::test]
async fn retiring_one_connection_does_not_cancel_dispatched_call_or_other_connection() {
    let transport = FrameTransport::with_choice("allow_once");
    let gate = Arc::new(tokio::sync::Notify::new());
    let fixture =
        Fixture::with_connections(Arc::clone(&transport), Some(Arc::clone(&gate)), 2).await;
    let first = fixture.instantiate("conn-1").await;
    let calls_before = fixture.calls();

    // 第一次调用进入「已 dispatch 但被 peer 挂住」的窗口。
    let fixture = Arc::new(fixture);
    let call = {
        let fixture = Arc::clone(&fixture);
        let view = first.clone();
        tokio::spawn(async move { fixture.app_callback(&view).await })
    };
    while fixture.calls() == calls_before {
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    // 关闭 conn-1：endpoint 离线 + attachment detach（不取消执行）。
    fixture.router.retire_connection("conn-1");
    gate.notify_waiters();
    let raw = call
        .await
        .expect("spawn 未 panic")
        .expect("已 dispatch 的调用不得被连接关闭取消");
    assert_eq!(
        raw.into_value()["content"][0]["text"],
        json!("canvas ready"),
        "连接关闭后已 dispatch 的调用仍须拿到真实结果"
    );

    // record 未被连接级收口撤销：再调一次得到的是「未获批准」（ApprovalUnavailable），
    // 而不是目标已撤销 / 策略拒绝。
    let fixture = Arc::try_unwrap(fixture).unwrap_or_else(|_| panic!("夹具无其他持有者"));
    let kind = fixture
        .app_callback(&first)
        .await
        .expect_err("路由已 detach ⇒ 未获批准");
    assert_kind(
        kind,
        McpAppsErrorKind::ApprovalUnavailable,
        "record 必须仍附着（未撤销）",
    );

    // 其他连接：第二条 record 的调用照常批准执行。
    let second = fixture.instantiate("conn-2").await;
    fixture
        .app_callback(&second)
        .await
        .expect("其他连接的 App 回调必须仍然可达");
    assert_eq!(
        fixture.calls(),
        calls_before + 3,
        "两次实例化 + 两次 App 调用 = 4 次 wire 调用"
    );
    fixture.shutdown().await;
}

/// Host invoke 路径证明：归属齐备 + 批准 ⇒ 恰好一次 wire 调用；无在线 endpoint
/// ⇒ 零新增 wire 调用且为显式「未获批准」。
#[tokio::test]
async fn host_invoke_runs_through_the_canonical_entry_exactly_once() {
    let transport = FrameTransport::with_choice("allow_once");
    let fixture = Fixture::new(Arc::clone(&transport)).await;

    let value = fixture.host_invoke().await.expect("批准后必须真实执行");
    assert_eq!(fixture.calls(), 1, "Host invoke 恰好一次 wire 调用");
    assert!(
        value["toolCallId"]
            .as_str()
            .is_some_and(|id| !id.is_empty()),
        "回执必须带稳定执行身份"
    );
    assert_eq!(value["serverId"], SERVER);
    assert!(matches!(
        fixture.latest_outcome(),
        SessionInvocationOutcome::Completed(_)
    ));
    assert_eq!(transport.permission_frames().len(), 1);

    // 无在线 endpoint：显式未执行，零新增 wire 调用。
    fixture.router.retire_connection("conn-1");
    let error = fixture
        .host_invoke()
        .await
        .expect_err("无在线 endpoint 不得执行");
    assert_eq!(
        error.kind,
        McpAppsErrorKind::ApprovalUnavailable,
        "无在线 endpoint"
    );
    assert_eq!(fixture.calls(), 1);
    assert!(matches!(
        fixture.latest_outcome(),
        SessionInvocationOutcome::NotExecuted {
            cause: NotExecutedCause::ApprovalUnavailable(_)
        }
    ));
    fixture.shutdown().await;
}

/// 用户拒绝 ⇒ `PolicyDenied`（**已作出的决定**），与「无人可答」严格区分。
#[tokio::test]
async fn user_rejection_is_reported_as_policy_denied_not_approval_unavailable() {
    let transport = FrameTransport::with_choice("reject_once");
    let fixture = Fixture::new(Arc::clone(&transport)).await;
    let view = fixture.instantiate("conn-1").await;

    let kind = fixture
        .app_callback(&view)
        .await
        .expect_err("用户拒绝不得执行");
    assert_kind(kind, McpAppsErrorKind::PolicyDenied, "用户拒绝");
    assert_eq!(fixture.calls(), 1, "拒绝 ⇒ 零新增 wire 调用");
    assert_eq!(transport.permission_frames().len(), 1, "拒绝来自真实应答帧");
    assert_eq!(
        fixture.latest_outcome(),
        SessionInvocationOutcome::NotExecuted {
            cause: NotExecutedCause::UserRejected
        }
    );
    fixture.shutdown().await;
}

/// 会话关闭：撤销全部 record（`session_closing`），此后调用显式拒绝；已发生的
/// 执行不被改写。
#[tokio::test]
async fn session_closing_revokes_records_without_rewriting_past_execution() {
    let transport = FrameTransport::with_choice("allow_once");
    let fixture = Fixture::new(Arc::clone(&transport)).await;
    let view = fixture.instantiate("conn-1").await;

    fixture.relay.close_session(SESSION);
    assert!(
        fixture.service.session_cancelled(),
        "会话关闭必须取消 session 层令牌（invocation 令牌的父）"
    );

    let kind = fixture
        .app_callback(&view)
        .await
        .expect_err("已撤销的 record 不得执行");
    assert_kind(kind, McpAppsErrorKind::PolicyDenied, "撤销是显式拒绝");
    assert_eq!(fixture.calls(), 1, "零新增 wire 调用");
    fixture.shutdown().await;
}

/// 会话终结（取消父已取消，而 record **未**撤销）：审批不得发出、零新增 wire 调用、
/// 不得谎报已完成。
///
/// 这条用例是装配点必须让取消父保持有效的**原因**（高 1 的语义面）：取消父一旦在
/// 会话存活期就被取消（如误用装配哨兵 `activation`），本用例描述的状态就是**每一次**
/// idle 执行的常态——审批 hook 被 `biased` select 抢先判中断，App 回调与 Host invoke
/// 全不可用。
#[tokio::test]
async fn cancelled_session_parent_aborts_before_approval_without_wire() {
    let transport = FrameTransport::with_choice("allow_once");
    let fixture = Fixture::new(Arc::clone(&transport)).await;
    let view = fixture.instantiate("conn-1").await;
    assert_eq!(fixture.calls(), 1, "只有实例化那一次 wire 调用");

    // 会话终结：不等同于 record 撤销——record 仍在注册表、attachment 仍绑定，
    // 唯一变化是取消父已取消。
    fixture.cancel_session();

    let kind = fixture
        .app_callback(&view)
        .await
        .expect_err("会话终结后不得继续执行");
    assert_ne!(
        kind,
        McpAppsErrorKind::PolicyDenied,
        "会话终结不是策略拒绝（record 未被撤销）"
    );
    assert!(
        transport.permission_frames().is_empty(),
        "取消父已取消 ⇒ 审批帧不得发出"
    );
    assert_eq!(fixture.calls(), 1, "零新增 wire 调用");
    assert!(
        matches!(
            fixture.latest_outcome(),
            SessionInvocationOutcome::OutcomeUnknown { .. }
        ),
        "会话已终结 ⇒ 执行事实是「可能已执行但没有终态」，不得谎报 Completed 或 NotExecuted；\
         实际: {:?}",
        fixture.latest_outcome()
    );
    fixture.shutdown().await;
}

// ─── 多 View：detach / 重附着（A11）────────────────────────────────────────

/// [A11] 多附件 + 重附着（ACP 层真实路径）：同一 record 的两个 View 各自可达；retire
/// 一条连接只 detach 该 View（另一个 View 不受影响，原因是 `AttachmentDetached` 而非
/// 「没有在线 endpoint」）；重挂接递增纪元后旧纪元批准失效、新纪元同 key 可继续。
#[tokio::test]
async fn multi_view_detach_and_reattach_keeps_epoch_authority() {
    let transport = FrameTransport::with_choice("allow_once");
    let fixture = Fixture::with_connections(Arc::clone(&transport), None, 3).await;
    let view_one = fixture.instantiate("conn-1").await;
    let epoch_one = view_one.epoch;
    assert_eq!(
        epoch_one,
        ConnectionEpoch::initial(),
        "首附纪元必须从 initial 起"
    );

    // 同一 record 的第二个 View 附到 conn-2（真实 attach_view + 真实 attach_app）。
    let view_two = fixture
        .attach_view(&view_one.app_id, "conn-2", ClientViewId::new("view-2"))
        .await;
    assert_ne!(
        view_two.attachment_id, view_one.attachment_id,
        "两个 View 必须是两个附件身份"
    );

    fixture
        .app_callback(&view_one)
        .await
        .expect("view-1 必须可达");
    fixture
        .app_callback(&view_two)
        .await
        .expect("view-2 必须可达");
    assert_eq!(
        transport.permission_frames().len(),
        2,
        "两个 View 各一次审批"
    );

    // conn-1 retire ⇒ 只 detach view-1；record 与 view-2 保留。
    assert_eq!(
        fixture.router.retire_connection("conn-1"),
        vec![view_one.attachment_id.clone()]
    );
    let kind = fixture
        .app_callback(&view_one)
        .await
        .expect_err("已 detach 的 View 不得执行");
    assert_kind(
        kind,
        McpAppsErrorKind::ApprovalUnavailable,
        "detach ⇒ 未获批准",
    );
    assert_eq!(
        fixture.latest_outcome(),
        SessionInvocationOutcome::NotExecuted {
            cause: NotExecutedCause::ApprovalUnavailable(
                peri_acp_types::interaction::UnansweredCause::AttachmentDetached
            )
        },
        "本端仍有在线 endpoint（conn-2/conn-3）⇒ 原因必须是 AttachmentDetached"
    );
    assert_eq!(
        transport.permission_frames().len(),
        2,
        "detach ⇒ 零新增审批帧"
    );
    fixture
        .app_callback(&view_two)
        .await
        .expect("另一个 View 必须不受影响");
    assert_eq!(transport.permission_frames().len(), 3);

    // 重挂接 view-1 到 conn-3：纪元递增（record 与 router 同源更新）。
    let view_three = fixture.reattach_view(&view_one, "conn-3").await;
    assert_eq!(view_three.epoch, epoch_one.next());

    // 旧纪元的批准：经真实 router 发出 ⇒ 显式未投递、零新增帧。
    {
        use peri_acp_types::interaction::{
            ApprovalItem, InteractionContext, TrustedInteractionOwner, UserInteractionBroker,
        };
        let stale = TrustedInteractionOwner::app(
            view_one.app_id.as_str(),
            view_one.attachment_id.clone(),
            epoch_one,
        );
        let response = UserInteractionBroker::request(
            fixture.router.as_ref(),
            stale,
            InteractionContext::Approval {
                items: vec![ApprovalItem {
                    tool_call_id: "call-stale".to_string(),
                    tool_name: APP_TOOL.to_string(),
                    tool_input: json!({}),
                }],
            },
        )
        .await;
        assert!(
            matches!(
                response,
                peri_acp_types::interaction::InteractionResponse::Unanswered {
                    cause: peri_acp_types::interaction::UnansweredCause::OwnerUnmatched
                }
            ),
            "旧纪元批准必须被丢弃（归属不匹配），实际: {response:?}"
        );
        assert_eq!(transport.permission_frames().len(), 3, "旧纪元 ⇒ 零新增帧");
    }

    // 新纪元同 key 可继续：经 conn-3 回调照常执行（真实 peer 恰好一次）。
    fixture
        .app_callback(&view_three)
        .await
        .expect("重挂接后新纪元必须可继续");
    assert!(matches!(
        fixture.latest_outcome(),
        SessionInvocationOutcome::Completed(_)
    ));
    assert_eq!(transport.permission_frames().len(), 4);
    fixture.shutdown().await;
}

// ─── 路由表边界 ────────────────────────────────────────────────────────────
/// 路由表有界：超过上限的连接注册被显式拒绝（不淘汰仍在线的 endpoint）。
#[tokio::test]
async fn session_router_endpoint_table_is_bounded() {
    let transport = FrameTransport::with_choice("allow_once");
    let fixture = Fixture::with_connections(Arc::clone(&transport), None, 8).await;
    let erased: Arc<dyn AcpTransport> = Arc::clone(&transport) as Arc<dyn AcpTransport>;
    assert!(
        fixture
            .router
            .register_connection("conn-9", erased)
            .is_err(),
        "第 9 个在线 endpoint 必须被拒绝"
    );
    fixture.shutdown().await;
}

#[test]
fn approval_unavailable_error_kind_is_distinct_from_policy_denial() {
    assert_ne!(
        McpAppsErrorKind::ApprovalUnavailable,
        McpAppsErrorKind::PolicyDenied
    );
    let kinds = serde_json::to_value([
        McpAppsErrorKind::ApprovalUnavailable,
        McpAppsErrorKind::PolicyDenied,
        McpAppsErrorKind::Cancelled,
    ])
    .unwrap();
    assert_eq!(kinds[0], "approval_unavailable");
    assert_eq!(kinds[1], "policy_denied");
}
