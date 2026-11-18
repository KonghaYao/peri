// Responsibility-specific cases share the original test namespace and fixtures.

/// Both properties are real API fields: approval must describe the same input
/// that the target receives, including any approved change to `path`.
async fn assert_dual_path_schema_survives_dispatch(
    approved_path: Option<&str>,
    wrapper_name: Option<&str>,
) {
    type InputTrace = Arc<parking_lot::Mutex<Vec<(&'static str, serde_json::Value)>>>;
    struct DualPathTool(InputTrace);

    #[async_trait::async_trait]
    impl BaseTool for DualPathTool {
        fn name(&self) -> &str {
            "DualPath"
        }
        fn description(&self) -> &str {
            "Uses path and file_path as distinct optional fields"
        }
        fn parameters(&self) -> serde_json::Value {
            json!({"type": "object", "properties": {
                "path": {"type": "string"},
                "file_path": {"type": "string"}
            }})
        }
        async fn invoke(
            &self,
            input: serde_json::Value,
            _ctx: crate::tools::ToolContext<'_>,
        ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
            self.0.lock().push(("invoke", input));
            Ok("executed".into())
        }
    }

    // Use the production binding seam: policy/events see the target, while the
    // transcript must retain the model's original wrapper request for pairing.
    struct Wrapper(Arc<dyn BaseTool>);
    #[async_trait::async_trait]
    impl BaseTool for Wrapper {
        fn name(&self) -> &str {
            "ExecuteExtraTool"
        }
        fn description(&self) -> &str {
            "binds the fixture's canonical target"
        }
        fn parameters(&self) -> serde_json::Value {
            json!({"type": "object", "properties": {
                "name": {"type": "string"},
                "arguments": {"type": "object"},
                "tool_name": {"type": "string"}, "params": {"type": "object"}
            }})
        }
        fn bind_invocation(
            &self,
            input: serde_json::Value,
        ) -> Result<
            Option<crate::tools::BoundToolInvocation>,
            Box<dyn std::error::Error + Send + Sync>,
        > {
            let policy_input = input
                .get("arguments")
                .or_else(|| input.get("params"))
                .cloned()
                .unwrap_or_default();
            Ok(Some(crate::tools::BoundToolInvocation {
                policy_name: self.0.name().into(),
                policy_input,
                target: Arc::clone(&self.0),
            }))
        }
        async fn invoke(
            &self,
            _input: serde_json::Value,
            _ctx: crate::tools::ToolContext<'_>,
        ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
            panic!("dispatch must invoke the bound target")
        }
    }

    struct ApprovePath {
        trace: InputTrace,
        replacement: Option<String>,
    }
    #[async_trait::async_trait]
    impl Middleware for ApprovePath {
        fn name(&self) -> &str {
            "ApprovePath"
        }
        async fn before_tools_batch(
            &self,
            _state: &mut dyn hook_state::BeforeToolState,
            calls: &[ToolCall],
        ) -> Vec<crate::error::AgentResult<ToolCall>> {
            calls
                .iter()
                .cloned()
                .map(|mut call| {
                    self.trace.lock().push(("approval", call.input.clone()));
                    if let Some(path) = &self.replacement {
                        call.input["path"] = json!(path);
                    }
                    Ok(call)
                })
                .collect()
        }
    }

    let trace: InputTrace = Arc::new(parking_lot::Mutex::new(Vec::new()));
    let mut ctx = make_test_ctx();
    let (bus, mut events) = crate::agent::events_v2::EventBus::new(Default::default());
    ctx.runtime.event_bus = Arc::new(bus);
    let target: Arc<dyn BaseTool> = Arc::new(DualPathTool(Arc::clone(&trace)));
    ctx.runtime.tools.write().insert(
        wrapper_name.unwrap_or("DualPath").into(),
        if wrapper_name.is_some() {
            Arc::new(Wrapper(target))
        } else {
            target
        },
    );
    let mut chain = MiddlewareChain::new();
    chain.add(Box::new(ApprovePath {
        trace: Arc::clone(&trace),
        replacement: approved_path.map(str::to_owned),
    }));
    ctx.runtime.middleware_chain = Arc::new(chain);
    let call_input = match wrapper_name {
        Some("ExecuteExtraTool") => {
            json!({"tool_name": "DualPath", "params": {"path": "requested/root"}})
        }
        Some(_) => json!({"name": "DualPath", "arguments": {"path": "requested/root"}}),
        None => json!({"path": "requested/root"}),
    };
    let raw_call = ToolCall::new(
        "dual-path",
        wrapper_name.unwrap_or("DualPath"),
        call_input,
    );
    let reasoning = Reasoning::with_tools("", vec![raw_call.clone()]);
    // 真实模型路径会带上本条 assistant 消息；审批补发的 ToolStarted 必须携带同一
    // 身份，TUI 才能在纯工具轮次建立可淘汰归属。
    let source_message = crate::messages::BaseMessage::ai_with_tool_calls(
        "",
        vec![crate::messages::ToolCallRequest::new(
            raw_call.id.clone(),
            raw_call.name.clone(),
            raw_call.input.clone(),
        )],
    );
    let source_message_id = source_message.id();
    let mut reasoning = reasoning;
    reasoning.source_message = Some(source_message);
    let catalog = ctx
        .runtime
        .tool_catalog
        .pin_working_tools(&ctx.runtime.tools.read())
        .expect("the pin working tools fixture result is required for this assertion");
    let outcome = crate::agent::stages::tool_dispatch::dispatch_tools(
        &ctx,
        &reasoning,
        &catalog,
        &CancellationToken::new(),
    )
    .await
    .expect("valid declared input must dispatch");
    assert_eq!(outcome.results.len(), 1);
    assert!(!outcome.results[0].1.is_error);
    assert_eq!(
        *trace.lock(),
        vec![
            ("approval", json!({"path": "requested/root"})),
            (
                "invoke",
                json!({"path": approved_path.unwrap_or("requested/root")})
            ),
        ],
        "dispatch must not reinterpret a declared field after policy approval"
    );
    let transcript = ctx.session.transcript.read();
    let messages = transcript.visible_messages();
    let raw = &messages[0].tool_calls()[0];
    assert_eq!(raw.id, raw_call.id);
    assert_eq!(raw.name, raw_call.name);
    assert_eq!(
        raw.arguments, raw_call.input,
        "approval must not rewrite the model's source call"
    );
    assert_eq!(outcome.results[0].1.tool_call_id, raw_call.id);
    let starts: Vec<_> = std::iter::from_fn(|| events.try_render())
        .filter_map(|event| match event {
            RenderEvent::ToolStarted {
                tool_call_id,
                name,
                input,
                origin_message_id,
                origin_readable,
                ..
            } => Some((
                tool_call_id,
                name,
                input,
                origin_message_id,
                origin_readable,
            )),
            _ => None,
        })
        .collect();
    assert_eq!(
        starts,
        vec![(
            raw_call.id,
            "DualPath".into(),
            json!({"path": approved_path.unwrap_or("requested/root")}),
            Some(source_message_id),
            Some(true),
        )],
        "审批通过后补发的 ToolStarted 必须描述同一 approved target input，并携带 assistant 消息身份与可回读标记"
    );
}

#[tokio::test]
async fn test_dispatch_preserves_dual_path_schema_after_approval() {
    assert_dual_path_schema_survives_dispatch(None, None).await;
}

#[tokio::test]
async fn test_dispatch_preserves_approved_replacement_of_declared_path() {
    assert_dual_path_schema_survives_dispatch(Some("approved/root"), None).await;
}

#[tokio::test]
async fn test_approved_wrapper_started_uses_edited_target_input() {
    assert_dual_path_schema_survives_dispatch(Some("approved/root"), Some("ExecuteExtraTool")).await;
    assert_dual_path_schema_survives_dispatch(Some("approved/root"), Some("ToolInvoke")).await;
}

/// 审批拒绝同样补发 ToolStarted（随后 ToolEnded）：必须携带真实 assistant 消息
/// 身份，否则 TUI 只能把拒绝卡片记为不可淘汰的未归属正文。
#[tokio::test]
async fn test_approval_rejection_started_carries_origin_message_id() {
    use crate::agent::events_v2::{EventBus, RenderEvent};

    struct EchoTool;
    #[async_trait::async_trait]
    impl BaseTool for EchoTool {
        fn name(&self) -> &str {
            "Bash"
        }
        fn description(&self) -> &str {
            "approval rejection fixture"
        }
        fn parameters(&self) -> serde_json::Value {
            json!({"type": "object"})
        }
        async fn invoke(
            &self,
            _input: serde_json::Value,
            _ctx: crate::tools::ToolContext<'_>,
        ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
            panic!("rejected call must not execute")
        }
    }

    struct RejectAll;
    #[async_trait::async_trait]
    impl Middleware for RejectAll {
        fn name(&self) -> &str {
            "RejectAll"
        }
        async fn before_tools_batch(
            &self,
            _state: &mut dyn hook_state::BeforeToolState,
            calls: &[ToolCall],
        ) -> Vec<crate::error::AgentResult<ToolCall>> {
            calls
                .iter()
                .map(|call| {
                    Err(AgentError::ToolRejected {
                        tool: call.name.clone(),
                        reason: "user rejected".to_string(),
                        cause: peri_acp_types::error::ToolRejectionCause::UserRejected,
                    })
                })
                .collect()
        }
    }

    let (bus, mut events) = EventBus::new(Default::default());
    let mut ctx = make_test_ctx();
    ctx.runtime.event_bus = Arc::new(bus);
    ctx.runtime
        .tools
        .write()
        .insert("Bash".into(), Arc::new(EchoTool));
    let mut chain = MiddlewareChain::new();
    chain.add(Box::new(RejectAll));
    ctx.runtime.middleware_chain = Arc::new(chain);

    let call = ToolCall::new("call-rejected", "Bash", json!({"command": "true"}));
    let source_message = crate::messages::BaseMessage::ai_with_tool_calls(
        "",
        vec![crate::messages::ToolCallRequest::new(
            call.id.clone(),
            call.name.clone(),
            call.input.clone(),
        )],
    );
    let source_message_id = source_message.id();
    let mut reasoning = Reasoning::with_tools("", vec![call]);
    reasoning.source_message = Some(source_message);
    let catalog = ctx
        .runtime
        .tool_catalog
        .pin_working_tools(&ctx.runtime.tools.read())
        .expect("the pin working tools fixture result is required for this assertion");
    let outcome = crate::agent::stages::tool_dispatch::dispatch_tools(
        &ctx,
        &reasoning,
        &catalog,
        &CancellationToken::new(),
    )
    .await
    .expect("rejected call settles as a tool error");
    assert_eq!(outcome.results.len(), 1);
    assert!(outcome.results[0].1.is_error);

    let mut started_origin = None;
    let mut ended = false;
    while let Some(event) = events.try_render() {
        match event {
            RenderEvent::ToolStarted {
                tool_call_id,
                origin_message_id,
                origin_readable,
                ..
            } if tool_call_id == "call-rejected" => {
                started_origin = Some((origin_message_id, origin_readable));
            }
            RenderEvent::ToolEnded {
                tool_call_id,
                is_error,
                ..
            } if tool_call_id == "call-rejected" => {
                assert!(is_error);
                ended = true;
            }
            _ => {}
        }
    }
    assert_eq!(
        started_origin,
        Some((Some(source_message_id), Some(true))),
        "审批拒绝补发的 ToolStarted 必须携带 assistant 消息身份与可回读标记"
    );
    assert!(ended, "rejection must emit ToolEnded");
}
