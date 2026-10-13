//! App relay / canonical 入口的**用例集**（夹具与真实对象见 `apps_relay_test.rs`，
//! 同一模块树）。
//!
//! S8 起旧 relay 面方法已删除：App 回调经与 façade 同一的 canonical 入口驱动
//! （`RelayFixture::app_callback`），断言落在 `SessionInvocationOutcome` 与其
//! `error_kind()` 投影上。

use super::*;
use peri_acp_types::mcp_apps_facade::{AppExecutionState, AttachmentDetachReason, CloseVerdict};

/// 关闭连接 = retire endpoint + 附件 **detach**（D6）：不撤销 record、不取消执行。
#[tokio::test]
async fn closed_connection_detaches_without_revoking_record_or_cancelling_execution() {
    let calls = Arc::new(AtomicUsize::new(0));
    let inputs = Arc::new(Mutex::new(Vec::new()));
    let fixture = relay_with_service(calls, inputs).await;
    let epoch = ConnectionEpoch::initial();

    fixture.relay.close_connection(CONNECTION);
    let error = fixture
        .app_callback(&fixture.attachment_id.clone(), epoch)
        .await
        .expect_err("连接关闭后 App 回调必须显式未执行");
    assert_eq!(error, McpAppsErrorKind::PolicyDenied);
    assert_eq!(
        fixture.calls.load(Ordering::SeqCst),
        0,
        "detach 后不得发出 wire 调用"
    );
    assert!(
        !fixture.service.session_cancelled(),
        "关闭连接不得取消会话执行"
    );

    // D6 的新语义：record 仍在注册表、未撤销（旧语义是「连接关闭 ⇒ 撤销」），
    // 只摘掉该连接的附件；窗口内重挂接后恢复可执行（真实 peer 恰好一次）。
    let record = fixture
        .relay
        .pool
        .app_binding_leases
        .find(&fixture.record_id)
        .expect("连接关闭不得撤销 record");
    assert!(record.attachments().is_empty(), "该连接的附件必须被摘掉");
    let reattached_attachment = AttachmentId::new("attachment-after-close");
    let reattached_epoch = epoch.next();
    fixture
        .relay
        .attach_app(fixture.attach_request(AttachmentBinding {
            attachment_id: reattached_attachment.clone(),
            namespace: RequestNamespaceId::new(),
            connection_epoch: reattached_epoch,
            next_seq: RequestSeq::new(1),
        }))
        .await
        .expect("record 未被撤销 ⇒ 必须可重挂接");
    fixture
        .app_callback(&reattached_attachment, reattached_epoch)
        .await
        .expect("重挂接后 App 回调必须可执行");
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 1);
    fixture.shutdown().await;
}

// ─── 阶段 4a App facts 面（真实 peer + 真实池 + 真实 service + 真实 relay）──────

/// [A5] attach 不调用任何业务工具：snapshot 直接给出 effective 输入与完整 raw，
/// 且**复用同一 `Arc`**（多 View 不复制结果，设计 §9.1）。
#[tokio::test]
async fn attach_returns_snapshot_without_calling_business_tool() {
    let calls = Arc::new(AtomicUsize::new(0));
    let inputs = Arc::new(Mutex::new(Vec::new()));
    let fixture = relay_with_service(calls, inputs).await;

    let record = fixture.issue_unattached_record();
    fixture
        .relay
        .pool
        .app_binding_leases
        .issue(Arc::clone(&record))
        .expect("夹具自检：世代一致的记录必须可登记");
    let strong_before = Arc::strong_count(record.initial_result());

    let first = fixture
        .relay
        .attach_app(fixture.attach_request_for(
            &record.record_id,
            AttachmentBinding {
                attachment_id: AttachmentId::new("attachment-first"),
                namespace: fixture.namespace.clone(),
                connection_epoch: ConnectionEpoch::initial(),
                next_seq: RequestSeq::new(1),
            },
        ))
        .await
        .expect("首附必须成功（目标字段由服务端记录导出）");
    assert!(!first.reattached, "首次附着不是重挂接");
    assert_eq!(
        first.descriptor.app_id, record.record_id,
        "snapshot 的 descriptor 指向同一 record"
    );
    assert_eq!(
        first.descriptor.execution_state,
        AppExecutionState::Live,
        "绑定 server 世代未变 ⇒ Live"
    );
    assert_eq!(first.descriptor.qualification, record.qualification);
    assert_eq!(
        first.initial_input.get("source"),
        Some(&serde_json::json!("fixture")),
        "初始输入是 effective（审批后）参数，不是客户端提交值"
    );
    assert_eq!(
        first.initial_result.clone().into_value()["content"][0]["text"],
        serde_json::json!("second"),
        "完整 raw 逐字进入 snapshot"
    );
    assert!(
        first.initial_result.is_error(),
        "`isError:true` 同样是可展示的完整结果事实（不得因失败而不签发）"
    );
    // 另一条签发事实：structuredContent 同样保真（主 record）。
    let main = fixture
        .relay
        .attach_app(
            fixture.attach_request(fixture.attachment_binding(ConnectionEpoch::initial().next())),
        )
        .await
        .expect("主记录重挂接必须成功");
    assert!(main.reattached);
    assert_eq!(
        main.initial_result.clone().into_value()["structuredContent"],
        serde_json::json!({"ready": true}),
        "完整 raw 保真（structuredContent）"
    );
    assert_eq!(
        main.initial_input.get("source"),
        Some(&serde_json::json!("fixture"))
    );
    assert_eq!(
        first.ui_resource.mime_type,
        peri_acp_types::mcp_apps::MCP_APPS_HTML_MIME
    );
    assert_eq!(first.features, McpAppsFeatures::implemented());
    assert_eq!(first.limits, McpAppsLimits::conservative());
    assert_eq!(
        first.attachment.connection_epoch,
        ConnectionEpoch::initial()
    );

    // 第二个 View 附着同一 record：仍是同一次执行事实，Arc 不因多 View 复制。
    let second = fixture
        .relay
        .attach_app(fixture.attach_request_for(
            &record.record_id,
            AttachmentBinding {
                attachment_id: AttachmentId::new("attachment-second"),
                namespace: fixture.namespace.clone(),
                connection_epoch: ConnectionEpoch::initial(),
                next_seq: RequestSeq::new(1),
            },
        ))
        .await
        .expect("第二个 View 必须可附着");
    assert!(second.reattached, "已有附件 ⇒ 重挂接语义");
    assert_eq!(
        Arc::strong_count(record.initial_result()),
        strong_before,
        "attach 不得为每个 View 复制结果 Arc"
    );
    assert_eq!(
        fixture.calls.load(Ordering::SeqCst),
        0,
        "attach 不得调用任何业务工具"
    );
    fixture.shutdown().await;
}

/// [A11] 重挂接递增纪元：旧纪元的请求被显式拒绝（零 wire），新纪元正常。
#[tokio::test]
async fn reattach_bumps_epoch_and_old_epoch_requests_are_rejected() {
    let calls = Arc::new(AtomicUsize::new(0));
    let inputs = Arc::new(Mutex::new(Vec::new()));
    let fixture = relay_with_service(calls, inputs).await;
    let old_epoch = ConnectionEpoch::initial();

    fixture
        .relay
        .read_resources(ResourceReadRequest {
            session_id: SESSION.into(),
            app_id: fixture.record_id.clone(),
            attachment_id: fixture.attachment_id.clone(),
            epoch: old_epoch,
            uri: "videos://clip".into(),
        })
        .await
        .expect("首纪元必须可用");
    assert_eq!(fixture.resource_reads.load(Ordering::SeqCst), 1);

    // 重挂接：同一附件身份换新纪元（纪元由调用方递增）。
    let new_epoch = old_epoch.next();
    let snapshot = fixture
        .relay
        .attach_app(fixture.attach_request(fixture.attachment_binding(new_epoch)))
        .await
        .expect("重挂接必须成功");
    assert!(snapshot.reattached);
    assert_eq!(snapshot.attachment.connection_epoch, new_epoch);

    let stale = fixture
        .relay
        .read_resources(ResourceReadRequest {
            session_id: SESSION.into(),
            app_id: fixture.record_id.clone(),
            attachment_id: fixture.attachment_id.clone(),
            epoch: old_epoch,
            uri: "videos://clip".into(),
        })
        .await
        .expect_err("旧纪元的请求必须被拒绝");
    assert_eq!(
        stale.kind,
        McpAppsErrorKind::StaleAttachmentEpoch,
        "旧纪元必须与「找不到附件」在类型上可区分"
    );
    assert_eq!(
        fixture.resource_reads.load(Ordering::SeqCst),
        1,
        "被拒绝的请求不得发出 wire 调用"
    );

    fixture
        .relay
        .read_resources(ResourceReadRequest {
            session_id: SESSION.into(),
            app_id: fixture.record_id.clone(),
            attachment_id: fixture.attachment_id.clone(),
            epoch: new_epoch,
            uri: "videos://clip".into(),
        })
        .await
        .expect("新纪元必须可用");
    assert_eq!(fixture.resource_reads.load(Ordering::SeqCst), 2);
    fixture.shutdown().await;
}

/// [A13 部分] detach 幂等且**不撤销 record**：分离期间零 wire，重挂接后恢复可用
///（窗口到期判定在 `peri-acp` 的 `interaction_router_test.rs` 用真实时钟钉住）。
#[tokio::test]
async fn detach_is_idempotent_and_reattach_recovers_the_view() {
    let calls = Arc::new(AtomicUsize::new(0));
    let inputs = Arc::new(Mutex::new(Vec::new()));
    let fixture = relay_with_service(calls, inputs).await;

    fixture
        .relay
        .detach_attachment(&fixture.attachment_id, AttachmentDetachReason::ViewDetached);
    let error = fixture
        .app_callback(&fixture.attachment_id.clone(), ConnectionEpoch::initial())
        .await
        .expect_err("detach 后 App 回调必须显式未执行");
    assert_eq!(error, McpAppsErrorKind::PolicyDenied);
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 0, "零 wire");

    // 幂等：再次 detach 不产生状态转移，也不影响 record 存活。
    fixture
        .relay
        .detach_attachment(&fixture.attachment_id, AttachmentDetachReason::ViewDetached);
    assert!(
        fixture
            .relay
            .pool
            .app_binding_leases
            .find(&fixture.record_id)
            .is_some(),
        "detach 不得撤销 record（裁决 D6）"
    );

    // 重挂接后恢复可执行（真实 peer 恰好一次）。
    let new_epoch = ConnectionEpoch::initial().next();
    fixture
        .relay
        .attach_app(fixture.attach_request(AttachmentBinding {
            attachment_id: fixture.attachment_id.clone(),
            namespace: fixture.namespace.clone(),
            connection_epoch: new_epoch,
            next_seq: RequestSeq::new(1),
        }))
        .await
        .expect("窗口内必须可重挂接");
    fixture
        .app_callback(&fixture.attachment_id.clone(), new_epoch)
        .await
        .expect("重挂接后 App 回调必须可执行");
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 1);
    assert!(
        !fixture.service.session_cancelled(),
        "detach / 重挂接不得取消会话执行"
    );
    fixture.shutdown().await;
}

/// [A2/A12] close = 结束交互资格：撤销 record 与附件，不取消已提交执行、不删历史。
#[tokio::test]
async fn close_app_revokes_interaction_without_cancelling_execution() {
    let calls = Arc::new(AtomicUsize::new(0));
    let inputs = Arc::new(Mutex::new(Vec::new()));
    let fixture = relay_with_service(calls, inputs).await;

    let outcome = fixture.relay.close_app(SESSION, &fixture.record_id);
    assert_eq!(outcome.verdict, CloseVerdict::Closed);
    assert_eq!(
        outcome.detached_attachments,
        vec![fixture.attachment_id.clone()]
    );
    let error = fixture
        .app_callback(&fixture.attachment_id.clone(), ConnectionEpoch::initial())
        .await
        .expect_err("关闭后 App 回调必须显式未执行");
    assert_eq!(error, McpAppsErrorKind::PolicyDenied);
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 0, "关闭后零 wire");
    assert!(
        !fixture.service.session_cancelled(),
        "关闭 App 不得取消执行（设计 §2）"
    );
    assert!(
        fixture
            .relay
            .list_apps(SESSION)
            .await
            .expect("列表必须可用")
            .is_empty(),
        "已关闭的 App 不再发布 descriptor（模型历史不受影响）"
    );

    let again = fixture.relay.close_app(SESSION, &fixture.record_id);
    assert_eq!(again.verdict, CloseVerdict::AlreadyClosed);
    assert!(again.detached_attachments.is_empty());
    fixture.shutdown().await;
}

/// [§6] `apps/list` 的 descriptor 由服务端记录投影：身份 / 资格 / 模型关联 /
/// 执行状态 / 附件 / 保留期，且**不含** raw 结果。
#[tokio::test]
async fn list_apps_projects_descriptor_facts() {
    let calls = Arc::new(AtomicUsize::new(0));
    let inputs = Arc::new(Mutex::new(Vec::new()));
    let fixture = relay_with_service(calls, inputs).await;

    let descriptors = fixture
        .relay
        .list_apps(SESSION)
        .await
        .expect("列表必须可用");
    assert_eq!(descriptors.len(), 1);
    let descriptor = &descriptors[0];
    assert_eq!(descriptor.app_id, fixture.record_id);
    assert_eq!(descriptor.server_id, SERVER);
    assert_eq!(descriptor.server_generation, fixture.server_generation);
    assert_eq!(descriptor.resource_uri, "ui://app");
    assert_eq!(descriptor.instantiating_tool, "show_canvas");
    assert_eq!(
        descriptor.model.tool_call_id.as_ref().map(|id| id.as_str()),
        Some("call-1")
    );
    assert_eq!(descriptor.execution_state, AppExecutionState::Live);
    assert_eq!(descriptor.attachments.len(), 1);
    assert_eq!(
        descriptor.attachments[0].attachment_id,
        fixture.attachment_id
    );
    assert_eq!(
        descriptor.attachments[0].connection_id.as_deref(),
        Some(CONNECTION)
    );
    assert!(
        descriptor.retention_remaining_ms > 0,
        "保留期剩余是可观察的有界事实"
    );
    let value = serde_json::to_value(descriptor).expect("descriptor 必须可序列化");
    assert!(
        value.get("initialResult").is_none(),
        "descriptor 不得携带 raw 结果（raw 只经 attach 给出）"
    );

    // 换句柄（server 世代前进）：同一 record 显式投影为 GenerationUnavailable。
    let handle = fixture
        .relay
        .pool
        .get_client(SERVER)
        .expect("fixture server installed");
    fixture.relay.pool.advance_handle_generation(&handle);
    let observed = fixture.relay.pool.handle_generation(&handle);
    let descriptors = fixture
        .relay
        .list_apps(SESSION)
        .await
        .expect("列表必须可用");
    assert_eq!(
        descriptors[0].execution_state,
        AppExecutionState::GenerationUnavailable {
            bound: fixture.server_generation,
            observed,
        }
    );
    fixture.shutdown().await;
}

/// [A6] 资源读取与列表只发给**绑定 server**；越权 / 伪造身份一律显式拒绝且零 wire。
#[tokio::test]
async fn resources_read_and_list_use_the_bound_server_only() {
    let calls = Arc::new(AtomicUsize::new(0));
    let inputs = Arc::new(Mutex::new(Vec::new()));
    let fixture = relay_with_service(calls, inputs).await;
    let epoch = ConnectionEpoch::initial();

    let contents = fixture
        .relay
        .read_resources(ResourceReadRequest {
            session_id: SESSION.into(),
            app_id: fixture.record_id.clone(),
            attachment_id: fixture.attachment_id.clone(),
            epoch,
            uri: "videos://clip".into(),
        })
        .await
        .expect("绑定 server 的资源必须可读");
    assert_eq!(contents.len(), 1);
    assert_eq!(contents[0].uri, "videos://clip");
    assert_eq!(contents[0].blob.as_deref(), Some("aGk="));
    assert_eq!(fixture.resource_reads.load(Ordering::SeqCst), 1);

    let listed = fixture
        .relay
        .list_resources(ResourceListRequest {
            session_id: SESSION.into(),
            app_id: fixture.record_id.clone(),
            attachment_id: fixture.attachment_id.clone(),
            epoch,
            cursor: None,
        })
        .await
        .expect("绑定 server 的资源目录必须可列");
    assert_eq!(listed.resources.len(), 1);
    assert_eq!(listed.resources[0].uri, "videos://clip");
    assert_eq!(listed.resources[0].name.as_deref(), Some("clip"));
    assert_eq!(listed.resources[0].mime_type.as_deref(), Some("video/mp4"));
    assert_eq!(listed.next_cursor.as_deref(), Some("cursor-1"));
    assert_eq!(
        listed.resources[0].meta.get("futureField"),
        Some(&serde_json::json!(true)),
        "目录项的 `_meta` 扩展必须保真"
    );
    assert_eq!(fixture.resource_lists.load(Ordering::SeqCst), 1);

    // 伪造 app 身份 / 跨会话：显式拒绝，且不再发出任何 wire 调用。
    let forged = fixture
        .relay
        .read_resources(ResourceReadRequest {
            session_id: SESSION.into(),
            app_id: "forged-app".into(),
            attachment_id: fixture.attachment_id.clone(),
            epoch,
            uri: "videos://clip".into(),
        })
        .await
        .expect_err("未知 App 身份必须被拒绝");
    assert_eq!(forged.kind, McpAppsErrorKind::PolicyDenied);
    let cross_session = fixture
        .relay
        .list_resources(ResourceListRequest {
            session_id: "other-session".into(),
            app_id: fixture.record_id.clone(),
            attachment_id: fixture.attachment_id.clone(),
            epoch,
            cursor: None,
        })
        .await
        .expect_err("跨会话必须被拒绝（ID 只定位）");
    assert_eq!(cross_session.kind, McpAppsErrorKind::PolicyDenied);
    assert_eq!(fixture.resource_reads.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.resource_lists.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 0, "资源面不触工具面");
    fixture.shutdown().await;
}

/// [D4] rerun 目标权威：无记录 ⇒ 显式拒绝、无回落；有记录 ⇒ 目标逐字来自服务端记录。
#[tokio::test]
async fn source_target_comes_from_the_retained_outcome_record_only() {
    let calls = Arc::new(AtomicUsize::new(0));
    let inputs = Arc::new(Mutex::new(Vec::new()));
    let fixture = relay_with_service(calls, inputs).await;

    let error = fixture
        .relay
        .source_target(
            SESSION,
            &peri_acp_types::tool_invocation::InvocationId::generate(),
        )
        .expect_err("无记录时必须显式拒绝，绝不按客户端字段回落");
    assert_eq!(error.kind, McpAppsErrorKind::PolicyDenied);
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 0);

    // 真实执行一次，再从服务端记录导出目标。
    fixture
        .app_callback(&fixture.attachment_id.clone(), ConnectionEpoch::initial())
        .await
        .expect("夹具自检：批准后必须真实执行");
    let retained = fixture
        .service
        .outcomes()
        .latest()
        .expect("夹具自检：执行必须入账");
    let target = fixture
        .relay
        .source_target(SESSION, &retained.invocation_id)
        .expect("已执行调用必须可导出目标");
    assert_eq!(target.server_id, SERVER);
    assert_eq!(
        target.wire_tool_name, "show_canvas",
        "wire 名是服务端声明名"
    );
    assert_eq!(
        target.effective_tool_name,
        crate::mcp::tool_bridge::effective_mcp_tool_name(SERVER, "show_canvas")
    );
    let cross_session = fixture
        .relay
        .source_target("relay-session-other", &retained.invocation_id)
        .expect_err("跨会话必须显式拒绝");
    assert_eq!(cross_session.kind, McpAppsErrorKind::PolicyDenied);
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 1, "导出目标不触 wire");
    fixture.shutdown().await;
}

/// [fail-closed] 记录不属当前 incarnation ⇒ 附着显式拒绝（旧环境不得复活）。
#[tokio::test]
async fn attach_rejects_records_from_another_incarnation() {
    let calls = Arc::new(AtomicUsize::new(0));
    let inputs = Arc::new(Mutex::new(Vec::new()));
    let fixture = relay_with_service(calls, inputs).await;
    let other_session = "relay-session-other";
    let other_incarnation = fixture.service.runtime_epoch() + 1;
    fixture
        .relay
        .pool
        .app_binding_leases
        .register_session_incarnation(other_session, other_incarnation);
    let record = Arc::new(McpAppRecord::new(McpAppRecordSpec {
        owner_session_id: other_session.into(),
        server_id: SERVER.into(),
        server_generation: fixture.server_generation,
        resource_uri: "ui://app".into(),
        instantiating_tool: "show_canvas".into(),
        invocation_id: peri_acp_types::tool_invocation::InvocationId::generate(),
        invocation_token: "token-other".into(),
        tool_call_id: None,
        incarnation: other_incarnation,
        qualification: AppQualification::DisplayOnly,
        effective_input: serde_json::Map::new(),
        initial_result: Arc::new(peri_acp_types::tool_outcome::RawToolResult::new(
            serde_json::json!({"content": []}),
        )),
    }));
    fixture
        .relay
        .pool
        .app_binding_leases
        .issue(Arc::clone(&record))
        .expect("夹具自检：世代一致的记录必须可登记");

    let error = fixture
        .relay
        .attach_app(AppAttachRequest {
            session_id: other_session.into(),
            app_id: record.record_id.clone(),
            owner_connection_id: CONNECTION.into(),
            attachment: AttachmentBinding {
                attachment_id: AttachmentId::new("attachment-other"),
                namespace: RequestNamespaceId::new(),
                connection_epoch: ConnectionEpoch::initial(),
                next_seq: RequestSeq::new(1),
            },
            client_view_id: ClientViewId::new("view-other"),
        })
        .await
        .expect_err("旧 incarnation 的记录必须被拒绝");
    assert_eq!(error.kind, McpAppsErrorKind::StaleAttachmentEpoch);
    fixture.shutdown().await;
}

/// [D-4] `reattached` 第三情形：record 曾有附件、当前附件**全部** detach、本次为新
/// View 首附 ⇒ 仍必须报**重挂接**。
///
/// detach 会摘掉交付集合里的条目，仅看集合会把这种重挂接误报成首附；判据因此取
/// 「本次之前集合非空 ∨ 本附件身份此前在册 ∨ 记录曾被附着过」的并集（有界记忆位，
/// 无权威）。真首附（记录从未附着）仍必须报 `false` —— 见
/// `attach_returns_snapshot_without_calling_business_tool`。
#[tokio::test]
async fn new_view_first_attach_after_all_views_detached_is_reported_as_reattached() {
    let calls = Arc::new(AtomicUsize::new(0));
    let inputs = Arc::new(Mutex::new(Vec::new()));
    let fixture = relay_with_service(calls, inputs).await;

    fixture
        .relay
        .detach_attachment(&fixture.attachment_id, AttachmentDetachReason::ViewDetached);
    let record = fixture
        .relay
        .pool
        .app_binding_leases
        .find(&fixture.record_id)
        .expect("detach 不得撤销 record");
    assert!(record.attachments().is_empty(), "附件集合此时为空");

    let snapshot = fixture
        .relay
        .attach_app(fixture.attach_request(AttachmentBinding {
            attachment_id: AttachmentId::new("attachment-fresh-view"),
            namespace: RequestNamespaceId::new(),
            connection_epoch: ConnectionEpoch::initial(),
            next_seq: RequestSeq::new(1),
        }))
        .await
        .expect("record 未被撤销 ⇒ 新 View 必须可附着");
    assert!(
        snapshot.reattached,
        "record 曾有附件 ⇒ 新 View 首附仍是重挂接（D-4 第三情形）"
    );
    assert_eq!(
        fixture.calls.load(Ordering::SeqCst),
        0,
        "attach 不得调用任何业务工具"
    );
    fixture.shutdown().await;
}
