//! `apps/*` 附着 / 重挂接 / detach 用例（夹具在 `apps_facade_test.rs` 同模块树）。

use super::*;

/// [A5] attach **不调用任何业务工具**；snapshot 携带 effective 输入 + 完整 raw
/// （多模态 / structuredContent / `_meta`），`isError:true` 的实例化同样可附着。
#[tokio::test]
async fn attach_returns_snapshot_without_calling_business_tool() {
    let transport = FrameTransport::with_choice("allow_once");
    let mut fixture = Fixture::new(Arc::clone(&transport)).await;

    let app_id = fixture
        .instantiate(APP_TOOL, json!({"value": 1, "source": "effective"}))
        .await;
    let calls_after_instantiate = fixture.calls();
    let attached = fixture.attach("conn-1", &app_id, "view-1").await;

    assert_eq!(
        fixture.calls(),
        calls_after_instantiate,
        "attach 不得调用业务工具（零 wire）"
    );
    assert!(
        transport.permission_frames().is_empty(),
        "attach 不产生审批"
    );
    assert_eq!(
        attached.snapshot.initial_input.get("source"),
        Some(&json!("effective")),
        "snapshot 的初始输入是实际（审批后 effective）参数"
    );
    let raw = attached.snapshot.initial_result.as_value();
    assert_eq!(
        raw["content"].as_array().map(Vec::len),
        Some(2),
        "完整 raw 含多模态 content，实际: {raw}"
    );
    assert_eq!(raw["content"][1]["type"], "image", "多模态块保真");
    assert_eq!(
        raw["structuredContent"]["canvas"]["width"], 640,
        "structuredContent 保真"
    );
    assert_eq!(raw["_meta"]["trace"], "fixture-trace", "结果 `_meta` 保真");
    assert!(!attached.snapshot.initial_result.is_error());
    assert_eq!(
        attached.snapshot.ui_resource.uri, RESOURCE_URI,
        "UI 资源引用来自 record（不由客户端给出）"
    );
    assert!(!attached.snapshot.reattached, "首附不是重挂接");
    assert_eq!(attached.attachment.next_seq, RequestSeq::new(1));
    assert_eq!(
        attached.snapshot.attachment, attached.attachment,
        "响应与 snapshot 的 attachment 同源同值"
    );
    assert_eq!(
        attached.features,
        McpAppsFeatures::implemented(),
        "响应必须带实际（已实现）能力"
    );
    assert_eq!(attached.limits, McpAppsLimits::conservative());
    assert!(matches!(
        attached.snapshot.descriptor.qualification,
        AppQualification::Interactive { .. }
    ));

    // `isError:true` 的实例化同样签发 record，并可附着（设计 §4：不再是「无内容」）。
    let broken_id = fixture.instantiate(BROKEN_TOOL, json!({"value": 2})).await;
    let broken = fixture.attach("conn-1", &broken_id, "view-broken").await;
    assert!(
        broken.snapshot.initial_result.is_error(),
        "isError:true 是结果事实，必须逐字可附着"
    );
    assert_eq!(
        fixture.calls(),
        calls_after_instantiate + 1,
        "两次实例化各一次 wire，两次 attach 都零 wire"
    );
    fixture.shutdown().await;
}

/// [A11] 重挂接递增纪元；旧纪元请求被拒绝且零 wire；新纪元同 key 取回同一结果。
#[tokio::test]
async fn reattach_bumps_epoch_and_old_epoch_requests_are_rejected() {
    let transport = FrameTransport::with_choice("allow_once");
    let mut fixture = Fixture::with_connections(Arc::clone(&transport), 2).await;
    let app_id = fixture.instantiate(APP_TOOL, json!({"value": 1})).await;
    let attached = fixture.attach("conn-1", &app_id, "view-1").await;
    let binding = attached.attachment.clone();
    let calls_before = fixture.calls();

    // 同一 View 在**另一条在线连接**上重挂接（页面刷新）：纪元递增，namespace 不变。
    let detached = fixture
        .apps(
            "peri/mcp/apps/detach",
            json!({
                "sessionId": fixture.session,
                "attachmentId": binding.attachment_id.as_str(),
                "epoch": binding.connection_epoch.get(),
            }),
        )
        .await
        .expect("detach 必须成功");
    assert_eq!(
        serde_json::from_value::<peri_acp_types::mcp_apps_facade::AppsDetachResponse>(detached)
            .unwrap()
            .verdict,
        DetachVerdict::Detached
    );

    let reattached = fixture
        .reattach("conn-2", &app_id, "view-1", &binding)
        .await
        .expect("窗口内必须可重挂接");
    assert_eq!(
        reattached.attachment.connection_epoch,
        binding.connection_epoch.next(),
        "重挂接必须递增纪元"
    );
    assert_eq!(
        reattached.attachment.namespace, binding.namespace,
        "namespace 随附件身份保留（不重建）"
    );
    assert!(reattached.snapshot.reattached, "必须标记为重挂接");

    // 旧纪元请求：显式拒绝（零登记、零 wire）。
    let stale = fixture
        .request_response(
            "conn-2",
            &binding,
            1,
            Fixture::tools_call(APP_TOOL, json!({"value": 3})),
        )
        .await
        .expect_err("旧纪元必须被拒绝");
    assert_kind(
        &stale,
        McpAppsErrorKind::StaleAttachmentEpoch,
        "旧纪元的提交必须被拒绝",
    );
    assert_eq!(fixture.calls(), calls_before, "被拒请求零 wire");

    // 新纪元同 key 取结果：第一次执行 + 同 key 复用。
    let first = fixture
        .request_wait(
            "conn-2",
            &reattached.attachment,
            1,
            Fixture::tools_call(APP_TOOL, json!({"value": 3})),
        )
        .await;
    assert_eq!(first.verdict, RequestVerdict::JoinedPending);
    assert_eq!(
        fixture.completed_text(first.outcome.as_ref().expect("终态")),
        "canvas ready"
    );
    assert_eq!(fixture.calls(), calls_before + 1, "恰好一次新 wire 调用");

    let replay = fixture
        .request_response(
            "conn-2",
            &reattached.attachment,
            1,
            Fixture::tools_call(APP_TOOL, json!({"value": 3})),
        )
        .await
        .expect("终态后同 key 必须可取结果");
    assert_eq!(replay.verdict, RequestVerdict::Duplicate);
    assert_eq!(fixture.calls(), calls_before + 1, "同 key 不二次执行");
    assert_eq!(transport.permission_frames().len(), 1, "同 key 不重复审批");
    fixture.shutdown().await;
}

/// [A13 部分] detach 幂等；窗口内重挂接恢复；窗口过期后不可再挂接（到期清理可见）。
#[tokio::test]
async fn detach_is_idempotent_and_reattach_window_recovers_view() {
    let transport = FrameTransport::with_choice("allow_once");
    let mut fixture = Fixture::with_connections(Arc::clone(&transport), 2).await;
    let app_id = fixture.instantiate(APP_TOOL, json!({"value": 1})).await;
    let attached = fixture.attach("conn-1", &app_id, "view-1").await;
    let binding = attached.attachment.clone();

    let first = fixture.detach_view(&binding, "conn-1").await;
    assert_eq!(first.verdict, DetachVerdict::Detached);
    let second = fixture.detach_view(&binding, "conn-1").await;
    assert_eq!(second.verdict, DetachVerdict::AlreadyDetached);

    // 窗口内重挂接：恢复同一 View（namespace 不变，纪元 +1）。
    let reattached = fixture
        .reattach("conn-2", &app_id, "view-1", &binding)
        .await
        .expect("窗口内必须可重挂接");
    assert_eq!(
        reattached.attachment.connection_epoch,
        binding.connection_epoch.next()
    );

    // 窗口到期 ⇒ 附件不可再挂接（到期判定本身在此可观测：显式拒绝，不静默复用）。
    let expired = fixture.router.expire_detached();
    assert_eq!(expired, 0, "附着中的附件不受到期清理影响");
    let unknown = fixture
        .reattach("conn-2", &app_id, "view-1", &reattached.attachment)
        .await;
    assert!(
        unknown.is_ok(),
        "同一 View 在附着状态下重挂接是允许的（页面刷新），实际: {unknown:?}"
    );

    // 未知附件身份的 detach：显式 UnknownAttachment（不创建条目）。
    let unknown_detach = fixture
        .apps(
            "peri/mcp/apps/detach",
            json!({
                "sessionId": fixture.session,
                "attachmentId": "att-does-not-exist",
                "epoch": 0,
            }),
        )
        .await
        .expect("未知附件必须给出判决而不是错误");
    assert_eq!(
        serde_json::from_value::<peri_acp_types::mcp_apps_facade::AppsDetachResponse>(
            unknown_detach
        )
        .unwrap()
        .verdict,
        DetachVerdict::UnknownAttachment
    );
    fixture.shutdown().await;
}
