use super::*;
use peri_agent::agent::async_tasks::TaskManager;
use peri_agent::agent::react::ToolCall;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::{mpsc, Semaphore};

struct GatedMessageLlm {
    calls: Arc<AtomicUsize>,
    release: Arc<Semaphore>,
    snapshots: mpsc::UnboundedSender<Vec<BaseMessage>>,
    first_answer: Reasoning,
}

#[async_trait::async_trait]
impl ReactLLM for GatedMessageLlm {
    async fn generate_reasoning(
        &self,
        messages: &[BaseMessage],
        _tools: &[&dyn BaseTool],
        _streaming: Option<StreamingContext>,
    ) -> peri_agent::error::AgentResult<Reasoning> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        self.snapshots.send(messages.to_vec()).unwrap();
        if call == 0 {
            self.release.acquire().await.unwrap().forget();
            return Ok(self.first_answer.clone());
        }
        Ok(Reasoning::with_answer("", "finished"))
    }
}

struct MessageFixture {
    dir: tempfile::TempDir,
    store: Arc<peri_agent::thread::FilesystemThreadStore>,
    tool: SubAgentTool,
    manager: Arc<TaskManager>,
    calls: Arc<AtomicUsize>,
    factories: Arc<AtomicUsize>,
    release: Arc<Semaphore>,
    snapshots: mpsc::UnboundedReceiver<Vec<BaseMessage>>,
    events: mpsc::UnboundedReceiver<ExecutorEvent>,
}

impl MessageFixture {
    fn new(first_answer: Reasoning) -> Self {
        let dir = tempdir().unwrap();
        write_test_agent(&dir);
        let store = make_fs_store(&dir);
        let manager = Arc::new(TaskManager::new());
        let calls = Arc::new(AtomicUsize::new(0));
        let factories = Arc::new(AtomicUsize::new(0));
        let release = Arc::new(Semaphore::new(0));
        let (snapshots_tx, snapshots) = mpsc::unbounded_channel();
        let (events_tx, events) = mpsc::unbounded_channel();
        let factory_calls = factories.clone();
        let llm_calls = calls.clone();
        let llm_release = release.clone();
        let tool = SubAgentTool::new(
            Arc::new(vec![make_tool("Probe")]),
            None,
            Arc::new(move |_| {
                factory_calls.fetch_add(1, Ordering::SeqCst);
                Box::new(GatedMessageLlm {
                    calls: llm_calls.clone(),
                    release: llm_release.clone(),
                    snapshots: snapshots_tx.clone(),
                    first_answer: first_answer.clone(),
                })
            }),
            dir.path().to_str().unwrap().into(),
        )
        .with_thread_store(store.clone())
        .with_task_manager(manager.clone())
        .with_bg_event_sender(events_tx)
        // 冻结空摘要，避免测试读取用户目录中的指引或 skill 列表。
        .with_frozen_data(
            Some(Arc::new(String::new())),
            None,
            Some(Arc::new(String::new())),
        )
        .with_frozen_system_prompt(Arc::new("Frozen test system".into()));
        Self {
            dir,
            store,
            tool,
            manager,
            calls,
            factories,
            release,
            snapshots,
            events,
        }
    }

    async fn invoke(
        &self,
        input: serde_json::Value,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        self.tool
            .invoke(
                input,
                peri_agent::tools::ToolContext::new(&[], self.dir.path().to_str().unwrap()),
            )
            .await
    }

    async fn start(&mut self, mut input: serde_json::Value) -> String {
        input["run_in_background"] = serde_json::json!(true);
        input["prompt"] = serde_json::json!("initial task");
        let result = self.invoke(input).await.unwrap();
        let id = result
            .split("(thread: ")
            .nth(1)
            .unwrap()
            .split(')')
            .next()
            .unwrap()
            .to_owned();
        let initial =
            tokio::time::timeout(std::time::Duration::from_secs(5), self.snapshots.recv())
                .await
                .unwrap()
                .unwrap();
        assert!(!initial
            .iter()
            .any(|message| message.content().contains("supplement-one")));
        id
    }

    async fn finish(&mut self) {
        self.release.add_permits(1);
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if let Some(ExecutorEvent::BackgroundTaskCompleted(result)) =
                    self.events.recv().await
                {
                    assert!(result.success, "后台任务应成功: {}", result.output);
                    break;
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(self.manager.active_count(), 0);
    }
}

/// [回归测试] active 的 resume_thread_id 原来只报错；现在必须复用同一执行投递。
#[tokio::test]
async fn test_active_message_reaches_next_model_request_without_resume() {
    let mut fixture = MessageFixture::new(Reasoning::with_tools(
        "",
        vec![ToolCall::new("probe", "Probe", serde_json::json!({}))],
    ));
    let id = fixture
        .start(serde_json::json!({"subagent_type": "test-agent"}))
        .await;
    // 即使 agent 定义已消失，active 发送也不能重新加载定义或创建模型。
    std::fs::remove_file(fixture.dir.path().join(".claude/agents/test-agent.md")).unwrap();
    for text in ["supplement-one", "supplement-two"] {
        let receipt = fixture
            .invoke(serde_json::json!({
                "resume_thread_id": id,
                "prompt": text,
                "run_in_background": false,
                "subagent_type": "missing-definition",
                "fork": true,
                "model": "invalid-but-ignored",
            }))
            .await
            .unwrap();
        assert!(
            receipt.starts_with("action: send\nstatus: queued\n"),
            "应明确返回发送回执: {receipt}"
        );
        assert!(receipt.contains(&format!("child_thread_id: {id}")));
        assert!(receipt.contains("Queued does not mean read"));
    }
    assert_eq!(
        fixture.factories.load(Ordering::SeqCst),
        1,
        "发送不能创建新模型"
    );
    assert_eq!(
        fixture.calls.load(Ordering::SeqCst),
        1,
        "发送不打断当前请求"
    );
    assert_eq!(fixture.manager.active_count(), 1);
    fixture.finish().await;
    let next = fixture.snapshots.recv().await.unwrap();
    let messages: Vec<_> = next.iter().map(BaseMessage::content).collect();
    let first = messages
        .iter()
        .position(|text| text.contains("supplement-one"))
        .unwrap();
    let second = messages
        .iter()
        .position(|text| text.contains("supplement-two"))
        .unwrap();
    assert!(first < second, "补充消息应按入队顺序出现");
    assert!(messages[first].contains("parent_message"));
    assert_eq!(
        messages
            .iter()
            .filter(|text| text.contains("supplement-one"))
            .count(),
        1
    );
    assert_eq!(fixture.factories.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 2);
    assert_eq!(
        fixture.store.list_session_threads(&id).await.unwrap().len(),
        1
    );
}

#[tokio::test]
async fn test_active_message_background_fork_info_does_not_extend_final_answer() {
    let mut fixture = MessageFixture::new(Reasoning::with_answer("", "finished"));
    let id = fixture.start(serde_json::json!({"fork": true})).await;
    let receipt = fixture
        .invoke(serde_json::json!({"resume_thread_id": id, "prompt": "supplement-one"}))
        .await
        .unwrap();
    assert!(receipt.starts_with("action: send\nstatus: queued"));
    fixture.finish().await;
    assert_eq!(
        fixture.calls.load(Ordering::SeqCst),
        1,
        "末轮 Info 不能额外触发模型"
    );
    assert!(fixture
        .manager
        .send_subagent_message(&id, Some("late"))
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn test_active_message_resumed_background_execution_accepts_info() {
    let mut fixture = MessageFixture::new(Reasoning::with_tools(
        "",
        vec![ToolCall::new("probe", "Probe", serde_json::json!({}))],
    ));
    let id = uuid::Uuid::now_v7().to_string();
    preset_resumable_thread(&fixture.store, &id, "fork", None, Vec::new()).await;
    let resumed_id = fixture
        .start(serde_json::json!({"resume_thread_id": id}))
        .await;
    assert_eq!(resumed_id, id);
    let receipt = fixture.invoke(serde_json::json!({"resume_thread_id": id, "prompt": "supplement-one", "run_in_background": true})).await.unwrap();
    assert!(receipt.starts_with("action: send\nstatus: queued"));
    fixture.finish().await;
    let next = fixture.snapshots.recv().await.unwrap();
    assert!(next
        .iter()
        .any(|message| message.content().contains("supplement-one")));
    assert_eq!(fixture.factories.load(Ordering::SeqCst), 1);
    assert_eq!(
        fixture.store.list_session_threads(&id).await.unwrap().len(),
        1
    );
}

#[tokio::test]
async fn test_active_message_rejects_empty_prompt_without_resuming() {
    let mut fixture = MessageFixture::new(Reasoning::with_answer("", "finished"));
    let id = fixture.start(serde_json::json!({"fork": true})).await;
    for prompt in [
        serde_json::Value::Null,
        serde_json::json!(""),
        serde_json::json!(" \n"),
    ] {
        let error = fixture
            .invoke(serde_json::json!({"resume_thread_id": id, "prompt": prompt}))
            .await
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("requires a non-empty prompt"),
            "错误应说明 active 发送需要正文: {error}"
        );
    }
    assert_eq!(fixture.factories.load(Ordering::SeqCst), 1);
    fixture.finish().await;
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn test_active_message_cross_session_is_rejected_without_spawning() {
    let mut fixture = MessageFixture::new(Reasoning::with_answer("", "finished"));
    let id = fixture.start(serde_json::json!({"fork": true})).await;
    let stranger = make_subagent_tool(Vec::new())
        .with_thread_store(fixture.store.clone())
        .with_task_manager(Arc::new(TaskManager::new()));
    let error = stranger
        .invoke(
            serde_json::json!({"resume_thread_id": id, "prompt": "other session"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("no live background receiver"),
        "必须拒绝跨 session 投递: {error}"
    );
    assert_eq!(
        fixture.store.list_session_threads(&id).await.unwrap().len(),
        1
    );
    fixture.finish().await;
}

/// [回归测试] panic 的逆序 Drop 必须先撤销收件箱，再发布注销/停止事件。
#[tokio::test]
async fn test_active_message_panic_revokes_before_runtime_deregistration() {
    struct PanicLlm;
    #[async_trait::async_trait]
    impl ReactLLM for PanicLlm {
        async fn generate_reasoning(
            &self,
            _messages: &[BaseMessage],
            _tools: &[&dyn BaseTool],
            _streaming: Option<StreamingContext>,
        ) -> peri_agent::error::AgentResult<Reasoning> {
            panic!("模拟后台执行崩溃");
        }
    }
    let dir = tempdir().unwrap();
    let manager = Arc::new(TaskManager::new());
    let (tx, mut rx) = mpsc::unbounded_channel();
    let cleanup_manager = manager.clone();
    let tool = SubAgentTool::new(
        Arc::new(Vec::new()),
        None,
        Arc::new(|_| Box::new(PanicLlm)),
        dir.path().to_str().unwrap().into(),
    )
    .with_task_manager(manager.clone())
    .with_frozen_data(
        Some(Arc::new(String::new())),
        None,
        Some(Arc::new(String::new())),
    )
    .with_deregister_runtime(Arc::new(move |thread_id| {
        tx.send(matches!(
            cleanup_manager.send_subagent_message(thread_id, Some("cleanup message")),
            Err(peri_agent::agent::async_tasks::SubagentMessageError::Closed)
        ))
        .unwrap();
    }));
    tool.invoke(
        serde_json::json!({"fork": true, "run_in_background": true, "prompt": "panic task"}),
        peri_agent::tools::ToolContext::new(&[], dir.path().to_str().unwrap()),
    )
    .await
    .unwrap();
    let closed = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(closed, "runtime 注销回调发生时收件箱必须已经关闭");
    manager.cancel_all();
}
