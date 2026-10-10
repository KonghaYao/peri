use super::*;
use peri_agent::agent::react::ToolCall;
use peri_agent::agent::stages::{run_react_loop, LoopResult, StageContext};
use peri_agent::session::{
    FrozenContext, MessageSource, MessageTranscript, QueuedMessage, Session,
};
use std::collections::BTreeMap;
use std::sync::Mutex;

const MISSING_THREAD: &str = "00000000-0000-0000-0000-000000000000";

struct MissingThreadParent {
    requests: Mutex<Vec<Vec<BaseMessage>>>,
}

#[async_trait::async_trait]
impl ReactLLM for MissingThreadParent {
    fn prepare_reasoning(
        &self,
        messages: &[BaseMessage],
        tools: &[&dyn BaseTool],
    ) -> peri_agent::error::AgentResult<peri_model::PreparedModelCall> {
        Ok(peri_model::PreparedModelCall::new(
            serde_json::json!({
                "provider": "resume-regression",
                "model": "parent-fixture",
                "endpoint": "http://127.0.0.1:1",
                "credentialRef": "fixture-no-credentials",
                "body": {
                    "messages": messages,
                    "tools": tools.iter().map(|tool| tool.definition()).collect::<Vec<_>>(),
                },
            }),
            |_| panic!("fixture reasoning has no network send"),
        ))
    }

    async fn generate_prepared_reasoning(
        &self,
        prepared: peri_model::PreparedModelCall,
        streaming: Option<StreamingContext>,
    ) -> peri_agent::error::AgentResult<Reasoning> {
        let messages: Vec<BaseMessage> =
            serde_json::from_value(prepared.checkpoint()["body"]["messages"].clone()).unwrap();
        self.generate_reasoning(&messages, &[], streaming).await
    }

    async fn generate_reasoning(
        &self,
        messages: &[BaseMessage],
        _: &[&dyn BaseTool],
        _: Option<StreamingContext>,
    ) -> peri_agent::error::AgentResult<Reasoning> {
        let mut requests = self.requests.lock().unwrap();
        requests.push(messages.to_vec());
        match requests.len() {
            1 => Ok(Reasoning::with_tools(
                "resume missing child",
                vec![ToolCall::new(
                    "missing-thread-resume",
                    "Agent",
                    serde_json::json!({
                        "resume_thread_id": MISSING_THREAD,
                        "prompt": "continue the child",
                    }),
                )],
            )),
            2 => {
                let tool_result = messages
                    .iter()
                    .find_map(|message| {
                        let value = serde_json::to_value(message).unwrap();
                        (value["role"] == "tool").then_some(value)
                    })
                    .expect("second Reason must receive the failed Agent tool result");
                assert_eq!(tool_result["is_error"], true);
                assert!(tool_result.to_string().contains("thread not found"));
                assert!(tool_result.to_string().contains(MISSING_THREAD));
                Ok(Reasoning::with_answer(
                    "",
                    "parent continues after missing child",
                ))
            }
            _ => panic!("parent should finish on its second Reason"),
        }
    }
}

/// [回归测试] 不存在的 child 是工具失败，父 Agent 仍完成且失败结果真实落库。
///
/// 历史背景：直接运行 RCRA 不包含宿主的 post-run flush；读取历史须等待 writer barrier。
#[tokio::test]
async fn parent_completes_after_real_agent_resume_missing_thread() {
    let directory = tempdir().unwrap();
    let fixture = SessionFixture::open_in(directory.path()).await;
    let cwd = fixture.workspace_cwd();
    let parent_id = fixture
        .create_thread(ThreadMeta::new_at(cwd.clone(), peri_time::now_wall()))
        .await
        .unwrap();
    let parent = Session::new(
        Arc::from(cwd.as_str()),
        FrozenContext::builder().build(),
        None,
    );
    *parent.transcript().write() =
        MessageTranscript::new().with_persistence(fixture.facade(), parent_id.clone());
    parent.queue().push(QueuedMessage::prompt(
        MessageSource::UserInput,
        BaseMessage::human("resume the missing child and report back"),
    ));
    let turn = parent.start_turn();
    let model = Arc::new(MissingThreadParent {
        requests: Mutex::new(Vec::new()),
    });
    let tool: Arc<dyn BaseTool> = Arc::new(
        SubAgentTool::new(
            Arc::new(Vec::new()),
            None,
            Arc::new(|_| panic!("missing child must never construct a child model")),
            cwd,
        )
        .with_session_resources(fixture.facade())
        .with_parent_thread_id(parent_id.clone()),
    );
    let context = StageContext::builder(turn, parent.transcript(), parent.queue().clone())
        .with_llm(model.clone())
        .with_tools(Arc::new(RwLock::new(BTreeMap::from([(
            "Agent".into(),
            tool,
        )]))))
        .build();
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        run_react_loop(context, 3),
    )
    .await
    .expect("parent must not stall on missing-thread failure");
    assert!(
        matches!(result, LoopResult::Completed),
        "parent loop result: {result:?}"
    );
    assert_eq!(model.requests.lock().unwrap().len(), 2);
    let persistence = parent.transcript().read().persist_tx_handle().unwrap();
    MessageTranscript::flush_via_tx(&persistence)
        .await
        .expect("parent transcript must be persisted before reading history");
    let history = fixture
        .resources
        .load_session_history(&parent_id)
        .await
        .unwrap();
    let history_messages: Vec<_> = history
        .iter()
        .filter_map(|payload| match payload {
            PersistedPayload::Message(message) => Some(message),
            PersistedPayload::SystemReminder { .. } => None,
        })
        .collect();
    let history_json = serde_json::to_string(&history_messages).unwrap();
    assert!(history_json.contains("thread not found"));
    let persisted_tools: Vec<_> = history_messages
        .iter()
        .map(|message| serde_json::to_value(message).unwrap())
        .filter(|message| message["role"] == "tool")
        .collect();
    assert_eq!(persisted_tools.len(), 1);
    assert_eq!(persisted_tools[0]["tool_call_id"], "missing-thread-resume");
    assert_eq!(persisted_tools[0]["is_error"], true);
    assert!(persisted_tools[0].to_string().contains(MISSING_THREAD));
    assert!(history_json.contains("parent continues after missing child"));
}
