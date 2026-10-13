//! `apps/*` 第二组用例（A2–A13 的其余行）。夹具在 `apps_facade_test.rs` 同模块树。

use super::*;
use peri_acp_types::mcp_apps::NotExecutedCause;

/// [A2/A12] `apps/close` 撤销交互资格，但**不取消**已 dispatch 的执行；
/// close 之后同 key 复用不能绕过拒绝（D9 附加）。
#[tokio::test]
async fn close_app_revokes_interaction_but_keeps_execution() {
    let gate = Arc::new(tokio::sync::Notify::new());
    let transport = FrameTransport::with_choice("allow_once");
    let mut fixture = Fixture::with_gate(Arc::clone(&transport), Some(Arc::clone(&gate))).await;
    let app_id = fixture.instantiate(APP_TOOL, json!({"value": 1})).await;
    let attached = fixture.attach("conn-1", &app_id, "view-1").await;
    let binding = attached.attachment.clone();

    // 第 2 次 wire 调用（App 回调）被 gate 卡住：审批已完成、执行在飞。
    let admitted = fixture
        .request_response(
            "conn-1",
            &binding,
            1,
            Fixture::tools_call(APP_TOOL, json!({"value": 2})),
        )
        .await
        .expect("请求必须被受理");
    assert!(admitted.admitted);
    assert_eq!(admitted.outcome, Some(AppsRequestOutcome::Pending));
    // 执行是后台任务：等待「审批已送达 + wire 调用已发出」两条事实再 close。
    fixture.wait_for_calls(2).await;
    fixture.wait_for_permission_frames(1).await;
    assert_eq!(fixture.permission_frames().len(), 1);

    // close：结束交互资格（不取消已 dispatch 的执行）。
    let closed = fixture.send_close(&app_id).await;
    assert_eq!(closed.verdict, CloseVerdict::Closed);
    assert_eq!(
        closed.detached_attachments,
        vec![binding.attachment_id.clone()],
        "close 必须结束该 App 的全部附件"
    );

    // 在飞执行继续收尾：结果仍是真实结果，不被 close 改写、不重执行。
    // 断言落在 **service 的 invocation 级事实**上：close 之后请求级账本已被销毁
    // （同 key 复用不能绕过拒绝），但已 dispatch 的调用保持它自己的 outcome。
    gate.notify_waiters();
    let outcome = fixture.wait_for_terminal_outcome().await;
    assert!(
        matches!(outcome, SessionInvocationOutcome::Completed(_)),
        "已 dispatch 的执行必须保留真实结果事实，实际: {outcome:?}"
    );
    assert_eq!(
        fixture.completed_text(&AppsRequestOutcome::ToolsCall(outcome)),
        "canvas ready"
    );
    assert_eq!(fixture.calls(), 2, "实例化 + 一次真实执行，close 不在其中");

    // close 之后：附件已终态撤销 ⇒ 同 key 复用被拒绝（零新 wire、零新审批）。
    let refused = fixture
        .request_response(
            "conn-1",
            &binding,
            1,
            Fixture::tools_call(APP_TOOL, json!({"value": 2})),
        )
        .await
        .expect_err("close 后同 key 必须被拒绝");
    assert!(
        matches!(refused.data.as_ref().unwrap()["kind"], Value::String(ref kind) if kind == "unknown_namespace" || kind == "conflict"),
        "close 后拒绝必须是显式判决键（unknown_namespace / conflict），实际: {refused:?}"
    );
    assert_eq!(fixture.calls(), 2, "拒绝路径零 wire");
    assert_eq!(fixture.permission_frames().len(), 1, "拒绝路径零审批");
    fixture.shutdown().await;
}

/// [A10/A7] rerun：同 key 重传复用结果且**不重复审批**；新 key 重新准入 + 审批。
#[tokio::test]
async fn rerun_same_key_reuses_result_and_new_key_reapproves() {
    let transport = FrameTransport::with_choice("allow_once");
    let mut fixture = Fixture::new(Arc::clone(&transport)).await;
    let app_id = fixture.instantiate(APP_TOOL, json!({"value": 1})).await;
    let attached = fixture.attach("conn-1", &app_id, "view-1").await;
    let binding = attached.attachment.clone();

    // 先产生一次真实执行，取得服务端记录里的 invocation 身份（rerun 的唯一目标来源）。
    let first = fixture
        .request_wait(
            "conn-1",
            &binding,
            1,
            Fixture::tools_call(APP_TOOL, json!({"value": 2})),
        )
        .await;
    assert_eq!(
        fixture.completed_text(first.outcome.as_ref().expect("终态")),
        "canvas ready"
    );
    let source = fixture
        .service
        .outcomes()
        .latest()
        .expect("执行记录")
        .invocation_id;
    let calls_before = fixture.calls();
    let frames_before = fixture.permission_frames();
    let host_namespace = fixture.host_namespace().await;

    // 新 key（seq=1）：新的用户意图 ⇒ 重新准入 + 审批 + 执行。
    let admitted = fixture
        .rerun(&host_namespace, 1, &source, json!({"value": 9}))
        .await
        .expect("rerun 必须被受理");
    assert!(
        admitted.admitted,
        "新序号必须登记（{:?}）",
        admitted.verdict
    );
    assert_eq!(admitted.outcome, Some(AppsRequestOutcome::Pending));
    let waited = fixture
        .rerun(&host_namespace, 1, &source, json!({"value": 9}))
        .await
        .expect("同 key 重传必须复用");
    assert_eq!(waited.verdict, RequestVerdict::JoinedPending);
    assert_eq!(
        fixture.completed_text(waited.outcome.as_ref().expect("终态")),
        "canvas ready",
        "rerun 复用同一结果"
    );
    assert_eq!(fixture.calls(), calls_before + 1, "rerun 恰好一次新 wire");
    assert_eq!(
        fixture.permission_frames().len(),
        frames_before.len() + 1,
        "新 key 必须重新审批"
    );

    // 同 key 再重传：终态复用，**不重复审批**、不重复执行。
    let replay = fixture
        .rerun(&host_namespace, 1, &source, json!({"value": 9}))
        .await
        .expect("同 key 必须返回同一结果");
    assert_eq!(replay.verdict, RequestVerdict::Duplicate);
    assert_eq!(fixture.calls(), calls_before + 1, "同 key 不二次执行");
    assert_eq!(
        fixture.permission_frames().len(),
        frames_before.len() + 1,
        "同 key 重传不得重复审批（D9）"
    );

    // 新 key（seq=2）：重新准入 + 审批（新一轮用户意图）。
    let second_intent = fixture
        .rerun(&host_namespace, 2, &source, json!({"value": 10}))
        .await
        .expect("新 key 必须被受理");
    assert!(second_intent.admitted);
    let waited = fixture
        .rerun(&host_namespace, 2, &source, json!({"value": 10}))
        .await
        .expect("同 key 取终态");
    assert_eq!(
        fixture.completed_text(waited.outcome.as_ref().expect("终态")),
        "canvas ready"
    );
    assert_eq!(fixture.calls(), calls_before + 2, "新 key 重新执行");
    assert_eq!(
        fixture.permission_frames().len(),
        frames_before.len() + 2,
        "新 key 重新审批"
    );
    fixture.shutdown().await;
}

/// [A7 / D4] rerun 的目标**只**来自服务端记录：客户端伪造 server/tool 字段被
/// wire 形状直接拒绝（`deny_unknown_fields`），实际执行的目标是记录里的那一个。
#[tokio::test]
async fn rerun_target_comes_from_source_invocation_not_client_fields() {
    let transport = FrameTransport::with_choice("allow_once");
    let mut fixture = Fixture::new(Arc::clone(&transport)).await;
    let app_id = fixture.instantiate(APP_TOOL, json!({"value": 1})).await;
    let attached = fixture.attach("conn-1", &app_id, "view-1").await;
    let source_execution = fixture
        .request_wait(
            "conn-1",
            &attached.attachment,
            1,
            Fixture::tools_call(APP_TOOL, json!({"value": 2})),
        )
        .await;
    assert_eq!(
        fixture.completed_text(source_execution.outcome.as_ref().expect("终态")),
        "canvas ready"
    );
    let source = fixture
        .service
        .outcomes()
        .latest()
        .expect("执行记录")
        .invocation_id;
    let host_namespace = fixture.host_namespace().await;
    let calls_before = fixture.calls();

    // 伪造目标字段：请求形状不合法 ⇒ 显式拒绝（零登记、零 wire）。
    let forged = fixture
        .apps(
            "peri/mcp/apps/rerun",
            json!({
                "sessionId": fixture.session,
                "hostNamespace": host_namespace.as_str(),
                "seq": 1,
                "sourceInvocationRef": source.as_str(),
                "serverId": "evil-server",
                "toolName": "evil_tool",
                "arguments": {"value": 3},
            }),
        )
        .await
        .expect_err("客户端伪造目标字段必须被拒绝");
    assert_kind(
        &forged,
        McpAppsErrorKind::InvalidEnvelope,
        "带未知字段的 rerun 请求不合法",
    );
    assert_eq!(fixture.calls(), calls_before, "被拒请求零 wire");

    // 合法 rerun：执行的是**记录里的**目标（fixture 的 server + 工具名）。
    let admitted = fixture
        .rerun(&host_namespace, 1, &source, json!({"value": 4}))
        .await
        .expect("rerun 必须被受理");
    assert!(admitted.admitted);
    let waited = fixture
        .rerun(&host_namespace, 1, &source, json!({"value": 4}))
        .await
        .expect("同 key 取终态");
    assert_eq!(
        fixture.completed_text(waited.outcome.as_ref().expect("终态")),
        "canvas ready"
    );
    assert_eq!(
        fixture.last_tool_call().as_deref(),
        Some(APP_TOOL),
        "实际执行的目标是服务端记录里的工具"
    );
    let record = fixture.service.outcomes().latest().expect("执行记录");
    assert_eq!(
        record
            .target
            .as_ref()
            .map(|target| target.server_id.as_str()),
        Some(SERVER),
        "outcome 记录的目标身份来自服务端记录"
    );
    assert_eq!(
        record
            .target
            .as_ref()
            .map(|target| target.wire_tool_name.as_str()),
        Some(APP_TOOL)
    );
    fixture.shutdown().await;
}

/// [A9] 两个会话 + 两条连接不串 namespace；仅知 sessionId 不能 attach。
#[tokio::test]
async fn two_sessions_two_connections_do_not_cross_namespaces() {
    let first_transport = FrameTransport::with_choice("allow_once");
    let second_transport = FrameTransport::with_choice("allow_once");
    let mut first = Fixture::new(Arc::clone(&first_transport)).await;
    let mut second = Fixture::for_session(
        "facade-session-b",
        Arc::clone(&second_transport),
        None,
        1,
        Fixture::deployment_authorization(),
    )
    .await;

    let first_app = first.instantiate(APP_TOOL, json!({"value": 1})).await;
    let first_attach = first.attach("conn-1", &first_app, "view-1").await;
    let second_app = second.instantiate(APP_TOOL, json!({"value": 2})).await;
    let second_attach = second.attach("conn-1", &second_app, "view-1").await;

    assert_ne!(
        first_attach.attachment.namespace, second_attach.attachment.namespace,
        "两个会话的附件 namespace 必须不同（UUID v7 永不复用）"
    );
    assert_ne!(
        first_attach.attachment.attachment_id,
        second_attach.attachment.attachment_id
    );

    // 会话 A 上使用会话 B 的 sessionId / 附件：显式拒绝（零执行）。
    let foreign_session = first
        .apps(
            "peri/mcp/apps/attach",
            json!({"sessionId": "facade-session-b", "appId": first_app, "clientViewId": "view-x"}),
        )
        .await
        .expect_err("跨会话 sessionId 必须被拒绝");
    assert_kind(
        &foreign_session,
        McpAppsErrorKind::InvalidSession,
        "sessionId 只作定位，与环境不符即拒绝",
    );
    let calls_before = first.calls();
    let foreign_attachment = first
        .request_response(
            "conn-1",
            &second_attach.attachment,
            1,
            Fixture::tools_call(APP_TOOL, json!({"value": 3})),
        )
        .await
        .expect_err("跨会话附件必须被拒绝");
    assert!(
        foreign_attachment.data.is_some(),
        "跨会话附件拒绝必须有稳定键：{foreign_attachment:?}"
    );
    assert_eq!(first.calls(), calls_before, "跨会话请求零 wire");

    // 仅知 sessionId（连接不是该会话在线 endpoint）不能 attach。
    let mut stranger = ConnectionContext::new(true);
    stranger.commit_initialize();
    let stranger_result = crate::host::mcp_apps::handle_request(
        "peri/mcp/apps/attach",
        &json!({"sessionId": first.session, "appId": first_app, "clientViewId": "view-z"}),
        &mut stranger,
        Some(&first.facade),
    )
    .await
    .expect_err("非在线 endpoint 的连接必须被拒绝");
    assert_kind(
        &stranger_result,
        McpAppsErrorKind::InvalidSession,
        "仅知 sessionId 不能 attach（A9）",
    );

    first.shutdown().await;
    second.shutdown().await;
}

/// [A3] 辅助工具经生产链可达；模型专用工具不可回调；拒批零 wire 且不得伪装成
/// 「审批不可用」。
#[tokio::test]
async fn app_only_helpers_are_reachable_and_model_only_tool_is_not_callbackable() {
    let transport = FrameTransport::with_choice("allow_once");
    let mut fixture = Fixture::new(Arc::clone(&transport)).await;
    let app_id = fixture.instantiate(APP_TOOL, json!({"value": 1})).await;
    let attached = fixture.attach("conn-1", &app_id, "view-1").await;
    let binding = attached.attachment.clone();
    let calls_before = fixture.calls();

    // App 面辅助工具：白名单里可达（生产链执行）。
    let helper = fixture
        .request_wait(
            "conn-1",
            &binding,
            1,
            Fixture::tools_call(APP_HELPER, json!({"value": 2})),
        )
        .await;
    assert_eq!(
        fixture.completed_text(helper.outcome.as_ref().expect("终态")),
        "helper ran"
    );
    assert_eq!(fixture.last_tool_call().as_deref(), Some(APP_HELPER));
    assert_eq!(fixture.calls(), calls_before + 1);

    // 模型专用工具（不在 App 白名单）：显式未执行、零 wire。
    let rejected = fixture
        .request_wait(
            "conn-1",
            &binding,
            2,
            Fixture::tools_call(MODEL_TOOL, json!({"value": 3})),
        )
        .await;
    assert_eq!(
        rejected.outcome,
        Some(AppsRequestOutcome::ToolsCall(
            SessionInvocationOutcome::NotExecuted {
                cause: NotExecutedCause::TargetUnbound
            }
        )),
        "不在 App 白名单的工具必须 TargetUnbound（零 wire）"
    );
    assert_eq!(fixture.calls(), calls_before + 1, "拒绝路径零新增 wire");

    // 拒批：与「审批不可用」严格区分，且零 wire。
    let mut rejecting = Fixture::new(FrameTransport::with_choice("reject_once")).await;
    let app_id = rejecting.instantiate(APP_TOOL, json!({"value": 1})).await;
    let attached = rejecting.attach("conn-1", &app_id, "view-1").await;
    let calls_before = rejecting.calls();
    let denied = rejecting
        .request_wait(
            "conn-1",
            &attached.attachment,
            1,
            Fixture::tools_call(APP_TOOL, json!({"value": 2})),
        )
        .await;
    assert_eq!(
        denied.outcome,
        Some(AppsRequestOutcome::ToolsCall(
            SessionInvocationOutcome::NotExecuted {
                cause: NotExecutedCause::UserRejected
            }
        )),
        "用户拒绝必须如实记为 UserRejected"
    );
    assert_eq!(rejecting.calls(), calls_before, "拒批零 wire");
    fixture.shutdown().await;
    rejecting.shutdown().await;
}

/// [A4] `DisplayOnly`（模型可见 + UI URI）：可显示、**零 callback 资格**。
#[tokio::test]
async fn display_only_app_never_grants_callback() {
    let transport = FrameTransport::with_choice("allow_once");
    let mut fixture = Fixture::new(Arc::clone(&transport)).await;
    let app_id = fixture.instantiate(MODEL_TOOL, json!({"value": 1})).await;
    let attached = fixture.attach("conn-1", &app_id, "view-1").await;

    assert_eq!(
        attached.snapshot.descriptor.qualification,
        AppQualification::DisplayOnly,
        "模型可见 + UI URI 的实例化必须签发 DisplayOnly"
    );
    assert_eq!(
        attached.snapshot.ui_resource.uri, MODEL_URI,
        "DisplayOnly 仍可显示（引用可读）"
    );
    let calls_before = fixture.calls();
    let denied = fixture
        .request_wait(
            "conn-1",
            &attached.attachment,
            1,
            Fixture::tools_call(MODEL_TOOL, json!({"value": 2})),
        )
        .await;
    assert_eq!(
        denied.outcome,
        Some(AppsRequestOutcome::ToolsCall(
            SessionInvocationOutcome::NotExecuted {
                cause: NotExecutedCause::TargetUnbound
            }
        )),
        "DisplayOnly 的 callback 必须 TargetUnbound（零资格）"
    );
    assert_eq!(fixture.calls(), calls_before, "DisplayOnly 回调零 wire");
    fixture.shutdown().await;
}

/// [A6] 资源 read / list 只发给**记录绑定的** server（无 URL / 本机文件直读旁路）。
#[tokio::test]
async fn resources_read_and_list_use_bound_server_only() {
    let transport = FrameTransport::with_choice("allow_once");
    let mut fixture = Fixture::new(Arc::clone(&transport)).await;
    let app_id = fixture.instantiate(APP_TOOL, json!({"value": 1})).await;
    let attached = fixture.attach("conn-1", &app_id, "view-1").await;
    let binding = attached.attachment.clone();

    let read = fixture
        .request_wait(
            "conn-1",
            &binding,
            1,
            json!({"method": "resources/read", "uri": VIDEO_URI}),
        )
        .await;
    let resources = match read.outcome {
        Some(AppsRequestOutcome::ResourcesRead { resources }) => resources,
        other => panic!("read 必须返回 contents，实际: {other:?}"),
    };
    assert_eq!(resources.len(), 1);
    assert_eq!(
        resources[0].blob.as_deref(),
        Some("aGk="),
        "读取结果来自绑定 server"
    );
    assert_eq!(fixture.resource_reads.load(Ordering::SeqCst), 1);

    let list = fixture
        .request_wait("conn-1", &binding, 2, json!({"method": "resources/list"}))
        .await;
    match list.outcome {
        Some(AppsRequestOutcome::ResourcesList {
            resources,
            next_cursor,
        }) => {
            assert_eq!(resources.len(), 1, "目录项来自绑定 server");
            assert_eq!(resources[0].uri, VIDEO_URI);
            assert_eq!(
                resources[0].meta.get("futureField"),
                Some(&Value::Bool(true)),
                "目录项未知字段保真"
            );
            assert_eq!(next_cursor.as_deref(), Some("cursor-1"));
        }
        other => panic!("list 必须返回目录，实际: {other:?}"),
    }
    assert_eq!(fixture.resource_lists.load(Ordering::SeqCst), 1);

    // 伪装成本机文件的 URI 也只走绑定 server：没有任何本机读取旁路。
    let shadow = fixture
        .request_wait(
            "conn-1",
            &binding,
            3,
            json!({"method": "resources/read", "uri": LOCAL_URI}),
        )
        .await;
    match shadow.outcome {
        Some(AppsRequestOutcome::ResourcesRead { resources }) => {
            assert_eq!(
                resources.len(),
                1,
                "读取仍由绑定 server 回答（本机文件不被读取）"
            )
        }
        other => panic!("read 必须返回 server 的答案，实际: {other:?}"),
    }
    assert_eq!(fixture.resource_reads.load(Ordering::SeqCst), 2);
    assert_eq!(fixture.calls(), 1, "资源操作不是 tools/call");
    fixture.shutdown().await;
}

/// [A8] dispatch 后断线：真实 outcome 保留（不被改写、不重执行），旧连接不再收到
/// 数据，当前授权连接可用同 key 取回结果。
#[tokio::test]
async fn disconnect_after_dispatch_keeps_real_outcome_and_never_reexecutes() {
    let gate = Arc::new(tokio::sync::Notify::new());
    let transport = FrameTransport::with_choice("allow_once");
    let mut fixture = Fixture::for_session(
        SESSION,
        Arc::clone(&transport),
        Some(Arc::clone(&gate)),
        2,
        Fixture::deployment_authorization(),
    )
    .await;
    let app_id = fixture.instantiate(APP_TOOL, json!({"value": 1})).await;
    let attached = fixture.attach("conn-1", &app_id, "view-1").await;
    let binding = attached.attachment.clone();

    let admitted = fixture
        .request_response(
            "conn-1",
            &binding,
            1,
            Fixture::tools_call(APP_TOOL, json!({"value": 2})),
        )
        .await
        .expect("请求必须被受理");
    assert!(admitted.admitted);
    fixture.wait_for_calls(2).await;
    fixture.wait_for_permission_frames(1).await;
    let dispatched_calls = fixture.calls();
    let frames_after_approval = fixture.permission_frames().len();

    // 连接关闭（D6：retire endpoint + 附件 detach，不撤销 record、不取消已 dispatch）。
    let conn_one = fixture.connection_id("conn-1");
    let detached = fixture.router.retire_connection(&conn_one);
    assert_eq!(detached, vec![binding.attachment_id.clone()]);
    fixture.relay.close_connection(&conn_one);

    // 在飞执行完成：真实结果（不是 NotExecuted、不是 OutcomeUnknown）。
    gate.notify_waiters();
    let outcome = fixture
        .wait_slot(&binding, 1)
        .await
        .expect("已 dispatch 的执行必须收尾");
    assert_eq!(fixture.completed_text(&outcome), "canvas ready");
    assert_eq!(
        fixture.calls(),
        dispatched_calls,
        "断线不产生第二次 wire 调用"
    );

    // 旧连接不再有提交资格（endpoint 已离线）：显式拒绝。
    let stale_connection = fixture
        .request_response(
            "conn-1",
            &binding,
            1,
            Fixture::tools_call(APP_TOOL, json!({"value": 2})),
        )
        .await
        .expect_err("retire 的连接不得再提交");
    assert_kind(
        &stale_connection,
        McpAppsErrorKind::InvalidSession,
        "离线连接不是在线 endpoint",
    );

    // 当前授权连接（conn-2）重挂接后同 key 取回同一结果：不重执行、不重审批。
    let reattached = fixture
        .reattach("conn-2", &app_id, "view-1", &binding)
        .await
        .expect("窗口内必须可重挂接");
    let replay = fixture
        .request_response(
            "conn-2",
            &reattached.attachment,
            1,
            Fixture::tools_call(APP_TOOL, json!({"value": 2})),
        )
        .await
        .expect("同 key 必须可取回结果");
    assert_eq!(replay.verdict, RequestVerdict::Duplicate);
    assert_eq!(
        fixture.completed_text(replay.outcome.as_ref().expect("终态")),
        "canvas ready"
    );
    assert_eq!(fixture.calls(), dispatched_calls, "同 key 不二次执行");
    assert_eq!(
        fixture.permission_frames().len(),
        frames_after_approval,
        "同 key 不重复审批"
    );
    fixture.shutdown().await;
}

/// [A7/A12] 无在线 endpoint（连接已 retire）的请求：显式拒绝、零 wire，且**不得**
/// 产出任何 outcome 记录（不伪装成「用户拒绝」，也不伪造未执行结论）。
#[tokio::test]
async fn request_without_online_endpoint_is_refused_without_fabricated_outcome() {
    let transport = FrameTransport::with_choice("allow_once");
    let mut fixture = Fixture::new(Arc::clone(&transport)).await;
    let app_id = fixture.instantiate(APP_TOOL, json!({"value": 1})).await;
    let attached = fixture.attach("conn-1", &app_id, "view-1").await;
    let binding = attached.attachment.clone();
    let outcomes_before = fixture.service.outcomes().snapshot().len();

    let conn_one = fixture.connection_id("conn-1");
    assert_eq!(
        fixture.router.retire_connection(&conn_one),
        vec![binding.attachment_id.clone()]
    );
    fixture.relay.close_connection(&conn_one);
    let calls_before = fixture.calls();

    let refused = fixture
        .request_response(
            "conn-1",
            &binding,
            1,
            Fixture::tools_call(APP_TOOL, json!({"value": 2})),
        )
        .await
        .expect_err("无在线 endpoint 的连接不得提交");
    assert_kind(
        &refused,
        McpAppsErrorKind::InvalidSession,
        "连接不是在线 endpoint ⇒ 显式拒绝（AcpError）",
    );
    assert_eq!(fixture.calls(), calls_before, "拒绝路径零 wire");
    assert!(transport.permission_frames().is_empty(), "拒绝路径零审批");
    assert_eq!(
        fixture.service.outcomes().snapshot().len(),
        outcomes_before,
        "拒绝不得伪造任何 outcome 记录（尤其不得呈现为用户拒绝）"
    );
    fixture.shutdown().await;
}

/// §8 六种判决在 ACP 层按键可区分（同一 method，同一 error 形状不含混）。
#[tokio::test]
async fn verdicts_are_distinguishable_by_stable_keys() {
    let transport = FrameTransport::with_choice("allow_once");
    let mut fixture = Fixture::new(Arc::clone(&transport)).await;
    let app_id = fixture.instantiate(APP_TOOL, json!({"value": 1})).await;
    let attached = fixture.attach("conn-1", &app_id, "view-1").await;
    let binding = attached.attachment.clone();

    // SequenceGap：跳过 seq=1 直接用 7。
    let gap = fixture
        .request_response(
            "conn-1",
            &binding,
            7,
            Fixture::tools_call(APP_TOOL, json!({"value": 2})),
        )
        .await
        .expect("跳跃是 in-band 判决");
    assert_eq!(
        gap.verdict,
        RequestVerdict::SequenceGap {
            expected: RequestSeq::new(1)
        }
    );
    assert!(!gap.admitted);
    assert_eq!(gap.outcome, None);

    // Conflict：同 key 不同参数。
    let first = fixture
        .request_wait(
            "conn-1",
            &binding,
            1,
            Fixture::tools_call(APP_TOOL, json!({"value": 2})),
        )
        .await;
    assert_eq!(first.verdict, RequestVerdict::JoinedPending);
    let conflict = fixture
        .request_response(
            "conn-1",
            &binding,
            1,
            Fixture::tools_call(APP_TOOL, json!({"value": 99})),
        )
        .await
        .expect("冲突是 in-band 判决");
    assert_eq!(conflict.verdict, RequestVerdict::Conflict);
    assert!(!conflict.admitted);
    assert_eq!(conflict.outcome, None);

    // Duplicate：同 key 同指纹。
    let duplicate = fixture
        .request_response(
            "conn-1",
            &binding,
            1,
            Fixture::tools_call(APP_TOOL, json!({"value": 2})),
        )
        .await
        .expect("终态后同 key 是 in-band 判决");
    assert_eq!(duplicate.verdict, RequestVerdict::Duplicate);
    assert_eq!(
        fixture.completed_text(duplicate.outcome.as_ref().expect("终态")),
        "canvas ready"
    );

    // UnknownNamespace：未知附件的判决（无结果形状可承载 ⇒ 按键投影，D3）。
    let unknown_cancel = fixture
        .apps(
            "peri/mcp/apps/cancel",
            json!({
                "sessionId": fixture.session,
                "attachmentId": "att-missing",
                "epoch": 0,
                "seq": 1,
            }),
        )
        .await
        .expect_err("未知附件必须被拒绝");
    assert_kind(
        &unknown_cancel,
        McpAppsErrorKind::UnknownNamespace,
        "未知附件 ⇒ UnknownNamespace 判决键",
    );

    // CancelVerdict：命中未决请求 = Accepted；终态后 = AlreadyFinished；无记录 = Unknown。
    let admitted = fixture
        .request_response(
            "conn-1",
            &binding,
            2,
            Fixture::tools_call(APP_TOOL, json!({"value": 5})),
        )
        .await
        .expect("新序号必须被登记");
    assert!(admitted.admitted);
    let accepted = fixture.cancel(&binding, 2).await;
    assert_eq!(accepted, CancelVerdict::Accepted);
    let unknown = fixture.cancel(&binding, 9).await;
    assert_eq!(unknown, CancelVerdict::Unknown);
    let finished = fixture.wait_slot(&binding, 2).await;
    assert!(
        finished.is_some(),
        "被取消的请求仍必须产出显式未执行事实（零伪造）"
    );
    assert_eq!(
        fixture.cancel(&binding, 1).await,
        CancelVerdict::AlreadyFinished,
        "终态请求的取消判决必须是 AlreadyFinished"
    );
    fixture.shutdown().await;
}

/// [D1 · 旧 wire 形状不回退] `peri/mcp/{open,app}` 别名仍是同一 façade 的薄别名：
/// 请求的 JSON-RPC id 逐字往返（不合成新 id）、响应携带 `appSessionId`（= 附件身份）
/// 与服务端记录导出的 `resourceUri`。
///
/// 别名自身不得有第二权威：本条只经 `handle_request` 的真实分发面驱动。
#[tokio::test]
async fn legacy_aliases_round_trip_json_rpc_id_and_attachment_identity() {
    use peri_acp_types::interaction::AttachmentId;
    use peri_acp_types::mcp_apps::{
        JsonRpcId, JsonRpcResponse, McpAppResponse, MCP_APPS_ENVELOPE_VERSION,
        MCP_APPS_PROTOCOL_VERSION,
    };

    let transport = FrameTransport::with_choice("allow_once");
    let mut fixture = Fixture::new(Arc::clone(&transport)).await;
    let app_id = fixture.instantiate(APP_TOOL, json!({"value": 1})).await;
    let attached = fixture.attach("conn-1", &app_id, "view-1").await;
    let binding = attached.attachment.clone();
    let conn = fixture.connection_id("conn-1");

    let value = fixture
        .apps_from(
            &conn,
            "peri/mcp/app",
            json!({
                "envelopeVersion": MCP_APPS_ENVELOPE_VERSION,
                "appsProtocolVersion": MCP_APPS_PROTOCOL_VERSION,
                "serverId": SERVER,
                "appSessionId": binding.attachment_id.as_str(),
                "resourceUri": RESOURCE_URI,
                "payload": {
                    "jsonrpc": "2.0",
                    "id": "call-42",
                    "method": "tools/call",
                    "params": {"name": APP_TOOL, "arguments": {"value": 2}}
                }
            }),
        )
        .await
        .expect("旧别名必须成功（同一 façade 的薄别名）");
    let response: McpAppResponse =
        serde_json::from_value(value).expect("旧 `peri/mcp/app` 响应必须可解码");
    assert_eq!(
        response.app_session_id,
        binding.attachment_id.as_str(),
        "`appSessionId` 的语义 = 附件身份（附件 ID，不是客户端自述值）"
    );
    assert_eq!(
        response.resource_uri, RESOURCE_URI,
        "`resourceUri` 取自服务端记录投影"
    );
    assert_eq!(response.server_id, SERVER);
    assert_eq!(response.envelope_version, MCP_APPS_ENVELOPE_VERSION);
    match response.payload {
        JsonRpcResponse::Result(result) => {
            assert_eq!(
                result.id,
                JsonRpcId::String("call-42".into()),
                "JSON-RPC id 必须逐字往返（别名不合成 / 不吞 id）"
            );
            assert_eq!(result.result["content"][0]["text"], "canvas ready");
        }
        other => panic!("App 别名必须回投影为 JSON-RPC result，实际: {other:?}"),
    }
    // 旧别名同样只经 canonical 入口执行一次业务调用。
    fixture.wait_for_calls(2).await;
    assert_eq!(fixture.last_tool_call().as_deref(), Some(APP_TOOL));

    // `peri/mcp/open` 别名：入参是签发点的 `invocationToken`（与 record 的
    // `model.invocationId` 同源），响应把附件身份交给客户端（本条不新增 wire 执行）。
    let session = fixture.session.clone();
    let listed = fixture
        .apps("peri/mcp/apps/list", json!({"sessionId": session}))
        .await
        .expect("apps/list 必须成功");
    let listed: peri_acp_types::mcp_apps_facade::AppsListResponse =
        serde_json::from_value(listed).expect("list 响应必须可解码");
    let invocation_token = listed
        .descriptors
        .iter()
        .find(|descriptor| descriptor.app_id == app_id)
        .expect("record 必须出现在 apps/list")
        .model
        .invocation_id
        .as_str()
        .to_string();
    let calls_before_open = fixture.calls();
    let opened = fixture
        .apps_from(
            &conn,
            "peri/mcp/open",
            json!({
                "envelopeVersion": MCP_APPS_ENVELOPE_VERSION,
                "appsProtocolVersion": MCP_APPS_PROTOCOL_VERSION,
                "serverId": SERVER,
                "toolName": APP_TOOL,
                "ownerSessionId": fixture.session.clone(),
                "invocationToken": invocation_token,
            }),
        )
        .await
        .expect("旧 `peri/mcp/open` 别名必须成功");
    let opened: peri_acp_types::mcp_apps::McpAppOpenResponse =
        serde_json::from_value(opened).expect("open 响应必须可解码");
    assert_eq!(opened.resource_uri, RESOURCE_URI);
    assert_eq!(opened.server_id, SERVER);
    assert_eq!(
        fixture.calls(),
        calls_before_open,
        "open 别名 zero wire：只领取记录 + 附着附件"
    );
    assert!(
        fixture
            .router
            .lookup(&AttachmentId::new(opened.app_session_id.clone()))
            .is_some(),
        "open 返回的 `appSessionId` 必须是 router 真实登记的附件身份"
    );
    fixture.shutdown().await;
}
