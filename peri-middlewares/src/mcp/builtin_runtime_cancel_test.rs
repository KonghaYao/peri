use super::*;

/// 外层取消必须返回 interrupted、不重放调用，并保持同一 pool 可继续服务。
/// 本用例的 FixtureBuiltinHandler 故意不消费取消，释放 GatedTool 后核对关闭；
/// 生产 handler 的通知传播与 shell 清理由 workspace_recovery_test 单独覆盖。
#[tokio::test]
async fn builtin_handler_in_flight_cancel_has_no_replay_and_keeps_pool_serving() {
    let web = find("web").expect("web 已实现");
    let declaration = &web.tools[0];
    let gated = Arc::new(GatedTool::new(declaration.original_name));
    let gated_tool: Arc<dyn BaseTool> = Arc::clone(&gated) as Arc<dyn BaseTool>;
    let link = TappedLink::connect(web, "runtime-fixture-web", vec![gated_tool]).await;
    let bridge = link.bridge(declaration.effective_name);
    let cancel = AgentCancellationToken::new();

    // 等「server 侧已进入本次 tools/call」再取消：这是「在飞」的定义点。
    let canceller = {
        let entered = Arc::clone(&gated.entered);
        let cancel = cancel.clone();
        async move {
            entered.notified().await;
            cancel.cancel();
        }
    };
    // 复刻 agent loop 的 race（`execution.rs:327-334` 同形）。
    let racer = async {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(EffectiveToolError::new(
                EffectiveToolErrorCode::Cancelled,
                "interrupted by user",
            )),
            result = bridge.invoke(
                json!({ "payload": "in-flight" }),
                fixture_tool_context("/tmp"),
            ) => result.map_err(|error| {
                EffectiveToolError::new(EffectiveToolErrorCode::ToolFailed, error.to_string())
            }),
        }
    };

    let ((), outcome) = tokio::join!(canceller, racer);

    // ① 取消形态。
    let error = outcome.expect_err("取消必须命中 race 的取消分支（不得让调用跑完）");
    assert_eq!(
        error.code,
        EffectiveToolErrorCode::Cancelled,
        "取消分支必须是 Cancelled: {error:?}"
    );
    assert!(
        error.message.contains("interrupted by user"),
        "取消文本必须与 agent loop 同文案: {error:?}"
    );

    // ② 无重放：取消不得在 wire 上产生第二次 tools/call，也不得让工具体再进入一次。
    assert_eq!(
        link.wire_call_tool_count(),
        1,
        "取消不得重放：wire 上只应有一次 tools/call，实际 methods={:?}",
        link.wire_methods()
    );
    assert_eq!(
        link.served_calls(),
        vec![declaration.original_name.to_string()],
        "server 侧只应见过一次 tools/call"
    );
    assert_eq!(
        gated.call_count(),
        1,
        "工具体只应被进入一次（取消不重放、不重试）"
    );

    // ③ 放行被弃置的在飞 handler，再确认 pool 仍可服务。
    gated.release.notify_one();
    gated.finished.notified().await;
    assert_eq!(
        gated.call_count(),
        1,
        "放行只收敛那一次在飞调用，不得引出新调用"
    );

    let text = bridge
        .invoke(
            json!({ "payload": "after-cancel" }),
            fixture_tool_context("/tmp"),
        )
        .await
        .expect("取消后同一条 wire / 同一 client service 必须仍可服务");
    assert_eq!(text, "gated");
    assert_eq!(gated.call_count(), 2);
    assert_eq!(link.wire_call_tool_count(), 2);
    assert_eq!(link.served_calls().len(), 2);

    // ④ 收尾：`Quit` 收敛 + task 排空（在 `shutdown` 内断言）。
    link.shutdown().await;
}

/// acceptance §7 第 8 条（**启动期取消 ⇒ `Interrupted`**）在 middleware 层的证据
/// （sub-plan H 冻结名口径）。
///
/// 构造：一个 `system_mcp = true` 的 builtin 实例、清单已发布（闸门**真的有** System
/// 依赖可等）、`system_mcp_timeout` 取远大于用例时长的 60s、**不**提交任何连接 /
/// discovery evidence ⇒ 闸门会停在等待；token **预先**取消。
///
/// 断言：`before_react_start` 返回 `Err(AgentError::Interrupted)`（**不是**
/// `MiddlewareError`）、不暂存候选、耗时远小于 `system_mcp_timeout`（排除 timeout 路径）。
///
/// 两条口径必须与断言一起读：
/// ① 本用例驱动的是**闸门内**取消，命中 `mcp/client/readiness.rs:395-397`（循环入口判
///    cancel）或 `:619-624`（`wait_for_readiness_change` 的 `tokio::select!`，取消分支
///    在 `:620`）**之一**；用例**不区分**这两条分支——它只有「返回值 + 未暂存候选」两个
///    观测点，两条分支产出同一个 `SystemReadinessError::Cancelled`。按预取消 token 的
///    **静态读法**应先命中 `:395-397`，但这是代码阅读结论，本用例不拿它当断言，也不
///    声称覆盖另一条。
/// ② host 层「prompt 启动后、闸门等待中被取消」的路径当前**没有确定性门闩**可断言
///    （`peri-agent/src/agent/stages/mod.rs:691-695` 会在 Receive 前先早退），因此本用例
///    是这套语义在 **middleware 层**的证据，不等于 host 层已端到端验证。
#[tokio::test]
async fn builtin_instance_cancellation_maps_to_interrupted() {
    /// 用例前提量：远大于本用例的实测时长，使「耗时远小于它」即排除 timeout 路径。
    const STARTUP_TIMEOUT_MS: u64 = 60_000;

    let web = find("web").expect("web 已实现");
    let pool = Arc::new(McpClientPool::new_pending());
    let mut config = builtin_entry(web);
    config.system_mcp_timeout = Some(STARTUP_TIMEOUT_MS);
    pool.configs.write().insert(web.name.to_string(), config);
    // 清单已发布（requirements 才非空）但**不提交任何连接 / discovery evidence**：
    // 闸门若无取消就必须一直等到 `system_mcp_timeout`。
    pool.publish_system_manifest(SystemMcpManifest::Loaded);

    let requirements = pool.system_requirements();
    assert_eq!(
        requirements.len(),
        1,
        "用例前提：闸门必须真的有 System 依赖可等（否则「取消」无等待可言）"
    );
    assert_eq!(requirements[0].server, web.name);
    assert_eq!(
        requirements[0].timeout,
        Duration::from_millis(STARTUP_TIMEOUT_MS),
        "用例前提：等待上界必须是本用例声明的 60s（timeout 路径的排除基线）"
    );
    assert!(
        !requirements[0].required_tools.is_empty(),
        "用例前提：builtin 实例必须声明必需工具"
    );

    let cancel = AgentCancellationToken::new();
    cancel.cancel();
    let middleware =
        McpMiddleware::new(Arc::clone(&pool)).with_skill_discovery(None, cancel.clone());
    let mut probe = StartupProbe::default();

    let started_at = std::time::Instant::now();
    let error = Middleware::before_react_start(&middleware, &mut probe)
        .await
        .expect_err("取消必须中断本次启动");
    let elapsed = started_at.elapsed();

    assert!(
        matches!(error, AgentError::Interrupted),
        "闸门内取消必须映射 Interrupted（不是 MiddlewareError / 不是 fatal）: {error:?}"
    );
    assert!(
        probe.staged.is_none(),
        "取消不得暂存启动候选（不发布 ready、不写宿主共享工具表）"
    );
    assert_eq!(probe.stage_calls, 0, "取消路径不得调用 stage_startup_tools");
    assert!(
        elapsed < Duration::from_millis(STARTUP_TIMEOUT_MS / 10),
        "启动取消必须立即返回，耗时不得接近 system_mcp_timeout（排除 timeout 路径）: {elapsed:?}"
    );

    pool.begin_shutdown();
    let report = pool.shutdown().await;
    assert!(report.is_complete(), "pool 关闭必须收敛: {report:?}");
    assert_eq!(
        pool.builtin_task_count(),
        0,
        "本用例不建 builtin 链路，关闭后 task 表必须为空（不留 orphan）"
    );
}

// ══════════════════════════════════════════════════════════════════════════════════
