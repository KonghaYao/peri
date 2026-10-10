use super::*;

// ─── A33：builtin 实例上下文的一次性注入 / 晚注入 / 缺失 ───────────────────────

/// A33 一次性注入：首次生效；第二次（含传入**同一** `Arc`）typed 拒绝且**不覆盖**首个；
/// `initialize` 已开始（seal）后的注入一律 `InitializationStarted`。
///
/// 「同一 `Arc` 再注入」是明文要求：注入是宿主组合根的单一动作，重复的一律是双装配 bug，
/// 必须可见——不得因为「对象没变」而静默放行。
#[test]
fn builtin_context_injection_is_single_shot() {
    let pool = McpClientPool::new_pending();
    assert!(pool.builtin_instance_context().is_none(), "前置：尚未注入");

    let first = Arc::new(BuiltinInstanceContext::new("ctx-one"));
    assert_eq!(
        pool.set_builtin_instance_context(Arc::clone(&first)),
        Ok(()),
        "首次注入必须成功"
    );
    assert_eq!(
        pool.set_builtin_instance_context(Arc::clone(&first)),
        Err(BuiltinContextError::AlreadyInjected),
        "同一 Arc 再注入也是重复注入"
    );
    assert_eq!(
        pool.set_builtin_instance_context(Arc::new(BuiltinInstanceContext::new("ctx-two"))),
        Err(BuiltinContextError::AlreadyInjected),
        "换一个上下文对象同样是重复注入"
    );

    let kept = pool
        .builtin_instance_context()
        .expect("首个上下文必须继续生效");
    assert!(Arc::ptr_eq(&kept, &first), "重复注入不得覆盖首个上下文");
    assert_eq!(kept.cwd, "ctx-one", "保真的必须是首个上下文的字段");

    // seal 之后的注入：既有上下文不被覆盖。
    pool.seal_builtin_context();
    assert_eq!(
        pool.set_builtin_instance_context(Arc::new(BuiltinInstanceContext::new("ctx-three"))),
        Err(BuiltinContextError::InitializationStarted),
        "initialize 已开始后的注入必须被拒绝"
    );
    let still = pool
        .builtin_instance_context()
        .expect("首个上下文仍必须生效");
    assert!(Arc::ptr_eq(&still, &first));

    // 未被注入就被封口：拒绝原因是「晚」，不是「重复」（判定顺序冻结）。
    let sealed_without_context = McpClientPool::new_pending();
    sealed_without_context.seal_builtin_context();
    sealed_without_context.seal_builtin_context(); // seal 幂等
    assert_eq!(
        sealed_without_context
            .set_builtin_instance_context(Arc::new(BuiltinInstanceContext::new("ctx-late"))),
        Err(BuiltinContextError::InitializationStarted),
        "seal 后的首次注入也必须是晚注入拒绝"
    );
    assert!(
        sealed_without_context.builtin_instance_context().is_none(),
        "被拒绝的注入不得写入任何上下文"
    );
}

/// 上下文缺失时 `spawn_builtin_transport` 必须 typed 收口：不 panic、不降级成别的传输
/// 形态，错误文本只含实例名（不含路径 / env / 凭据）。
#[test]
fn builtin_context_missing_is_typed_error() {
    let pool = McpClientPool::new_pending();
    assert!(
        pool.builtin_instance_context().is_none(),
        "前置：未注入上下文"
    );

    let error = pool
        .spawn_builtin_transport("web")
        .err()
        .unwrap_or_else(|| panic!("未注入上下文时不得建立 builtin 链路"));
    match error {
        BuiltinSpawnError::ContextMissing { instance } => assert_eq!(instance, "web"),
        other => panic!("必须是 typed ContextMissing（不得是别的形态），实际: {other:?}"),
    }

    let text = BuiltinSpawnError::ContextMissing {
        instance: "web".to_string(),
    }
    .to_string();
    assert!(text.contains("builtin 实例上下文未注入"), "实际: {text}");
    assert!(text.contains("web"), "错误文本必须含实例名，实际: {text}");
    assert!(
        !text.contains('/') && !text.contains('\\'),
        "错误文本不得含路径，实际: {text}"
    );
}

/// X7/X8（J6/W3b）：MetaHarness 覆盖读取只认真实 builtin `workspace` 实例。
///
/// - 外部 server 抢占同名 `workspace` 且已连接 ⇒ 空批（不按 scheme/名字信任）；
/// - 关闭集命中 ⇒ 空批（覆盖不可用，宿主保持内置，无磁盘兜底）；
/// - 无句柄 / 未连接 ⇒ 空批（X8：覆盖不可得不是错误，由宿主按内置处理）。
#[tokio::test]
async fn meta_harness_reads_only_identity_verified_builtin_workspace() {
    let enabled: std::collections::HashSet<String> = ["01_intro".to_string()].into_iter().collect();

    // 无句柄：空批（不是错误）。
    let pool = McpClientPool::new_pending();
    assert!(pool
        .read_builtin_workspace_meta(&enabled)
        .await
        .unwrap()
        .is_empty());

    // 外部 server 占用 "workspace" 名字（source = None）且已连接：X7 拒绝。
    pool.clients
        .write()
        .insert("workspace".to_string(), connected_test_handle("workspace"));
    assert!(
        pool.read_builtin_workspace_meta(&enabled)
            .await
            .unwrap()
            .is_empty(),
        "外部同 scheme 来源不得进入覆盖扫描（X7）"
    );

    // 身份正确（ConfigSource::Builtin）但实例在关闭集内：X8 直接空批。
    let pool = McpClientPool::new_pending();
    let mut handle = connected_test_handle("workspace");
    Arc::get_mut(&mut handle).unwrap().source = Some(crate::mcp::config::ConfigSource::Builtin {
        instance: "workspace".to_string(),
    });
    pool.clients.write().insert("workspace".to_string(), handle);
    pool.set_builtin_instance_context(Arc::new(
        BuiltinInstanceContext::new("test-cwd")
            .with_closed(std::collections::BTreeSet::from(["workspace".to_string()])),
    ))
    .unwrap();
    assert!(
        pool.read_builtin_workspace_meta(&enabled)
            .await
            .unwrap()
            .is_empty(),
        "关闭的 workspace 实例不得提供覆盖（X8）"
    );
}
