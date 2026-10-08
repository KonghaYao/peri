use super::*;

struct AppCaller;

#[async_trait]
impl BaseTool for AppCaller {
    fn name(&self) -> &str {
        "AppCaller"
    }
    fn description(&self) -> &str {
        "app dispatch fixture"
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {}})
    }
    fn is_direct(&self) -> bool {
        true
    }
    async fn invoke(
        &self,
        _input: Value,
        ctx: ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let dispatcher = ctx
            .effective_tool_dispatcher
            .as_ref()
            .expect("app dispatcher must be available");
        Ok(dispatcher
            .dispatch(
                peri_acp_types::tools::EffectiveToolCall {
                    invocation_id: "app-call".into(),
                    tool_name: effective_name(APP_ONLY_TOOL),
                    input: json!({"id": "n1"}),
                    parent_invocation_id: None,
                },
                CancellationToken::new(),
            )
            .await?)
    }
}

#[tokio::test]
async fn model_act_rejects_app_only_calls_but_app_dispatch_still_reaches_wire() {
    let fixture = spawn_fixture(false).await;
    let hidden_name = effective_name(APP_ONLY_TOOL);
    let visible_name = effective_name(DEFERRED_TOOL);
    let mut tools = bridge_tools(&[fixture.bridge(APP_ONLY_TOOL), fixture.bridge(DEFERRED_TOOL)]);
    tools.insert(
        EXECUTE_EXTRA_TOOL_NAME.to_string(),
        Arc::new(ExecuteExtraTool::new(Arc::new(RwLock::new(tools.clone())))),
    );
    tools.insert("AppCaller".into(), Arc::new(AppCaller));
    let broker = RecordingBroker::new(true);
    let chain_seen = Arc::new(Mutex::new(Vec::new()));
    let (context, _events, _catalog) = make_context(
        tools,
        approval_chain(broker.clone(), Arc::clone(&chain_seen)),
    );
    let reasoning = Reasoning::with_tools(
        "",
        vec![
            ToolCall::new("hidden-direct", &hidden_name, json!({"id": "n1"})),
            ToolCall::new(
                "hidden-direct-case",
                hidden_name.to_uppercase(),
                json!({"id": "n1"}),
            ),
            ToolCall::new(
                "hidden-wrapped",
                EXECUTE_EXTRA_TOOL_NAME,
                json!({"tool_name": hidden_name, "params": {"id": "n1"}}),
            ),
            ToolCall::new(
                "hidden-wrapped-case",
                EXECUTE_EXTRA_TOOL_NAME,
                json!({"tool_name": hidden_name.to_uppercase(), "params": {"id": "n1"}}),
            ),
        ],
    );
    let outcome = dispatch_tools(
        &context,
        &reasoning,
        &context.runtime.tool_catalog.snapshot(),
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(outcome.results.len(), 4);
    for (_, result) in &outcome.results {
        assert!(result.is_error, "{result:?}");
        assert!(
            result.output.contains("not available to the model"),
            "{result:?}"
        );
    }
    assert!(fixture.log.calls().is_empty());
    assert!(broker.seen().is_empty());
    assert!(chain_seen.lock().is_empty());

    let reasoning = Reasoning::with_tools(
        "",
        vec![ToolCall::new("visible", &visible_name, json!({"id": "n1"}))],
    );
    let outcome = dispatch_tools(
        &context,
        &reasoning,
        &context.runtime.tool_catalog.snapshot(),
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(outcome.results.len(), 1);
    assert!(!outcome.results[0].1.is_error, "{:?}", outcome.results);
    assert_eq!(
        fixture.log.called_tool_names(),
        vec![DEFERRED_TOOL.to_string()]
    );

    let reasoning =
        Reasoning::with_tools("", vec![ToolCall::new("app-entry", "AppCaller", json!({}))]);
    let outcome = dispatch_tools(
        &context,
        &reasoning,
        &context.runtime.tool_catalog.snapshot(),
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(outcome.results.len(), 1);
    assert!(!outcome.results[0].1.is_error, "{:?}", outcome.results);
    assert_eq!(
        fixture.log.called_tool_names(),
        vec![DEFERRED_TOOL.to_string(), APP_ONLY_TOOL.to_string()]
    );
    assert!(chain_seen
        .lock()
        .iter()
        .any(|call| call.name == hidden_name));
}

/// 能力：H5 app-only 否决（wire 记账 + 模型面）。
///
/// server 以 `_meta.ui.visibility = ["app"]` 声明的工具：不进模型直连表；
/// 经 `ExecuteExtraTool` 按名（含大小写变体）强行调用在解析阶段失败且 wire
/// 零 `tools/call`；同 fixture 的模型可见 deferred 目标解析成功，wire 记账通路
/// 以一次对照调用自证存活（索引/列表/元工具描述面由 tool_search 单元用例覆盖）。
#[tokio::test]
async fn app_only_mcp_bridge_is_invisible_and_rejected_before_any_wire_call() {
    let fixture = spawn_fixture(false).await;
    let app_only = fixture.bridge(APP_ONLY_TOOL);
    let visible = fixture.bridge(DEFERRED_TOOL);
    assert!(!app_only.visible_to_model(), "fixture 自检：app-only 生效");
    assert!(visible.visible_to_model(), "fixture 自检：对照工具模型可见");

    let app_only_effective = effective_name(APP_ONLY_TOOL);
    let visible_effective = effective_name(DEFERRED_TOOL);
    let mut tools = bridge_tools(&[app_only, visible]);
    tools.insert(
        EXECUTE_EXTRA_TOOL_NAME.to_string(),
        Arc::new(ExecuteExtraTool::new(Arc::new(RwLock::new(tools.clone())))),
    );

    // (a) direct 面（Reason 阶段同一谓词）不含 app-only。
    let direct = direct_definition_names(
        &SessionToolCatalog::try_new(tools.clone(), None)
            .expect("fixture catalog must build")
            .snapshot(),
    );
    assert!(
        !direct.contains(&app_only_effective),
        "app-only 工具不得进入模型直连工具列表: {direct:?}"
    );

    // (b) 按名强行调用（含大小写变体）必须在解析阶段失败，且 wire 零调用。
    let resolver = ExecuteExtraToolResolver::default();
    let resolve_call = |name: &str| {
        resolver.resolve(
            &ToolCall::new(
                "call-target",
                EXECUTE_EXTRA_TOOL_NAME,
                json!({"tool_name": name, "params": {"id": "n1"}}),
            ),
            &tools,
        )
    };
    assert!(
        resolve_call(&app_only_effective).is_err()
            && resolve_call(&app_only_effective.to_uppercase()).is_err(),
        "app-only 工具必须在解析阶段被拒绝（精确名与大小写变体）"
    );
    assert!(
        fixture.log.calls().is_empty(),
        "app-only 工具不得触发任何真实 MCP tools/call: {:?}",
        fixture.log.calls()
    );

    // (c) 正向对照：模型可见 deferred 目标解析成功。
    let invocation = resolve_call(&visible_effective).expect("模型可见 deferred 目标必须解析成功");
    assert_eq!(invocation.policy_call.name, visible_effective);

    // (d) wire 记账通路存活证明：(b) 的零调用是「否决生效」而非「通路坏了」。
    let params = CallToolRequestParams::new(DEFERRED_TOOL.to_string())
        .with_arguments(JsonObject::from_iter([("id".to_string(), json!("n1"))]));
    fixture
        .service
        .call_tool_once(params)
        .await
        .expect("control wire call must succeed");
    assert_eq!(
        fixture.log.called_tool_names(),
        vec![DEFERRED_TOOL.to_string()],
        "对照调用必须恰好记账一次"
    );

    // (e) app-only 持续被拒绝、不追加 wire 调用，bridge 仍在注册表内。
    assert!(
        resolve_call(&app_only_effective).is_err(),
        "app-only 目标必须持续被拒绝"
    );
    assert_eq!(
        fixture.log.called_tool_names(),
        vec![DEFERRED_TOOL.to_string()],
        "app-only 解析失败不得追加 wire 调用"
    );
    assert!(
        tools.contains_key(&app_only_effective),
        "app-only bridge 必须保留在共享注册表中（App 合法调用路径）"
    );
}
