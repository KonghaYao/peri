use super::*;
use peri_agent::agent::react::{Reasoning, StreamingContext};
use peri_agent::messages::BaseMessage;
use peri_agent::tools::BaseTool;
use peri_model::ModelCallContext;

struct CapturedCall {
    context: Option<ModelCallContext>,
    seen: Arc<parking_lot::Mutex<Vec<ModelCallContext>>>,
}

#[async_trait::async_trait]
impl ReactLLM for CapturedCall {
    fn set_call_context(
        &mut self,
        context: ModelCallContext,
    ) -> peri_agent::error::AgentResult<()> {
        assert!(
            self.context.is_none(),
            "the factory must supply a fresh bridge"
        );
        self.context = Some(context);
        Ok(())
    }

    async fn generate_reasoning(
        &self,
        _: &[BaseMessage],
        _: &[&dyn BaseTool],
        _: Option<StreamingContext>,
    ) -> peri_agent::error::AgentResult<Reasoning> {
        self.seen
            .lock()
            .push(self.context.clone().expect("hook identity"));
        Ok(Reasoning::with_answer("", r#"{"continue":true}"#))
    }
}

fn registered(matcher: &str) -> RegisteredHook {
    RegisteredHook {
        hook: serde_json::from_value(serde_json::json!({"type":"prompt","prompt":"check"}))
            .unwrap(),
        event: HookEvent::PreToolUse,
        matcher: Some(matcher.into()),
        plugin_name: "fixture".into(),
        plugin_id: "fixture".into(),
        plugin_source: None,
        plugin_root: "/fixture".into(),
        plugin_data_dir: "/fixture".into(),
        plugin_options: HashMap::new(),
    }
}

async fn fire_tool_hook(dispatcher: &HookDispatcher, tool_id: &str, execution: &str) -> HookAction {
    let tool_input = serde_json::json!({"command":"true"});
    let input = HookInput::tool_call(
        "owner",
        "/transcript",
        "/fixture",
        "Bypass",
        "Bash",
        &tool_input,
        tool_id,
    );
    dispatcher
        .fire_event_with_call_context(
            HookEvent::PreToolUse,
            &input,
            Some("Bash"),
            Some(&tool_input),
            Some(ModelCallContext {
                owner: "owner".into(),
                execution: execution.into(),
                call: format!("reason:7:{tool_id}"),
                purpose: "hook".into(),
                parent: None,
            }),
        )
        .await
}

#[tokio::test]
async fn concurrent_tool_hooks_keep_execution_group_and_config_slot_identity() {
    let seen = Arc::new(parking_lot::Mutex::new(Vec::new()));
    let make_dispatcher = |scope: &str| {
        let factory_seen = Arc::clone(&seen);
        HookDispatcher::new(
            Arc::new(RwLock::new(HashMap::from([(
                HookEvent::PreToolUse,
                vec![registered("Read"), registered("Bash"), registered("Bash")],
            )]))),
            Arc::new(move || {
                Box::new(CapturedCall {
                    context: None,
                    seen: Arc::clone(&factory_seen),
                })
            }),
            Arc::new(OnceTracker::new()),
            "/fixture".into(),
        )
        .with_model_call_scope(scope.into())
    };
    let local = make_dispatcher("local");
    let group_0 = make_dispatcher("group:0");
    let group_1 = make_dispatcher("group:1");
    // The same triggering execution/tool and configured hook slots must remain
    // distinct when two independently configured hook groups both match.
    let (left, right, first_group, second_group) = tokio::join!(
        fire_tool_hook(&local, "tool-a", "exec-a"),
        fire_tool_hook(&local, "tool-b", "exec-b"),
        fire_tool_hook(&group_0, "shared-tool", "shared-exec"),
        fire_tool_hook(&group_1, "shared-tool", "shared-exec"),
    );
    for action in [left, right, first_group, second_group] {
        assert!(matches!(action, HookAction::Allow));
    }
    let mut contexts = seen.lock().clone();
    contexts.sort_by(|left, right| left.call.cmp(&right.call));
    assert_eq!(contexts.len(), 8);
    for (context, (execution, call)) in contexts.iter().zip([
        (
            "shared-exec",
            "hook:group:0:PreToolUse:reason:7:shared-tool:1",
        ),
        (
            "shared-exec",
            "hook:group:0:PreToolUse:reason:7:shared-tool:2",
        ),
        (
            "shared-exec",
            "hook:group:1:PreToolUse:reason:7:shared-tool:1",
        ),
        (
            "shared-exec",
            "hook:group:1:PreToolUse:reason:7:shared-tool:2",
        ),
        ("exec-a", "hook:local:PreToolUse:reason:7:tool-a:1"),
        ("exec-a", "hook:local:PreToolUse:reason:7:tool-a:2"),
        ("exec-b", "hook:local:PreToolUse:reason:7:tool-b:1"),
        ("exec-b", "hook:local:PreToolUse:reason:7:tool-b:2"),
    ]) {
        assert_eq!(context.owner, "owner");
        assert_eq!(context.execution, execution);
        assert_eq!(context.call, call);
        assert_eq!(context.purpose, "hook");
        assert_eq!(context.parent, None);
    }
}
