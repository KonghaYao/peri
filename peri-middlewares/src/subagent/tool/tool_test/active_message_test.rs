use super::*;
use peri_agent::agent::async_tasks::TaskManager;
use peri_agent::agent::react::ToolCall;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::{mpsc, Semaphore};

#[derive(Clone)]
struct GatedMessageLlm {
    calls: Arc<AtomicUsize>,
    release: Arc<Semaphore>,
    snapshots: mpsc::UnboundedSender<Vec<BaseMessage>>,
    first_answer: Reasoning,
}

impl GatedMessageLlm {
    async fn respond(
        &self,
        request: peri_model::ModelRequest,
        cancellation: tokio_util::sync::CancellationToken,
    ) -> Vec<peri_model::ModelResult<peri_model::ModelStreamEvent>> {
        use crate::subagent::test_support::*;
        let _ = &cancellation;
        let messages = base_messages(&request);
        let defined = defined_tools(&request);
        let tools: Vec<&dyn BaseTool> = defined.iter().map(|t| t as &dyn BaseTool).collect();

        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        self.snapshots.send(messages.to_vec()).unwrap();
        if call == 0 {
            self.release.acquire().await.unwrap().forget();
            return events_from_reasoning(self.first_answer.clone());
        }
        text_events("finished")
    }
}
crate::subagent::test_support::fixture_model_impl!(GatedMessageLlm);

struct MessageFixture {
    dir: tempfile::TempDir,
    store: SessionFixture,
    /// 本夹具父会话 id 与句柄（同库的第二个工具必须用它才不越根）。
    parent_id: String,
    parent: Arc<peri_agent::session::Session>,
    tool: SubAgentTool,
    manager: Arc<TaskManager>,
    calls: Arc<AtomicUsize>,
    factories: Arc<AtomicUsize>,
    release: Arc<Semaphore>,
    snapshots: mpsc::UnboundedReceiver<Vec<BaseMessage>>,
    events: mpsc::UnboundedReceiver<ExecutorEvent>,
}

impl MessageFixture {
    async fn new(first_answer: Reasoning) -> Self {
        let dir = tempdir().unwrap();
        write_test_agent(&dir);
        let store = SessionFixture::open_in(dir.path()).await;
        // 会话 cwd 与父子链：child 保存要求父会话存在且 cwd 与调用 cwd 一致。
        let cwd = store.workspace_cwd();
        let parent_id = store
            .create_thread(ThreadMeta::new_at(cwd.clone(), peri_time::now_wall()))
            .await
            .expect("建立父会话失败");
        let parent = peri_agent::session::Session::new(
            std::sync::Arc::from(cwd.as_str()),
            peri_agent::session::FrozenContext::builder().build(),
            Some(parent_id.clone()),
        );
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
                crate::subagent::test_support::fixture_source(
                    std::sync::Arc::new(GatedMessageLlm {
                        calls: llm_calls.clone(),
                        release: llm_release.clone(),
                        snapshots: snapshots_tx.clone(),
                        first_answer: first_answer.clone(),
                    }),
                    "fixture-scripted",
                )
            }),
            cwd.clone(),
        )
        .with_session_resources(store.facade())
        .with_parent_thread_id(parent_id.clone())
        .with_parent_session(parent.clone())
        .with_task_manager(manager.clone())
        .with_bg_event_sender(events_tx)
        // 冻结空摘要，避免测试读取用户目录中的指引或 skill 列表。
        .with_frozen_data(
            Some(Arc::new(String::new())),
            None,
            Some(Arc::new(String::new())),
        );
        let tool = with_agent_face(tool, dir.path()).await;
        Self {
            dir,
            store,
            parent_id,
            parent,
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
    ))
    .await;
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

/// [回归测试] 父代理在末轮推理期间发送的补充任务不能因 Info 调度被静默跳过。
#[tokio::test]
async fn test_active_message_background_fork_defer_extends_final_answer() {
    let mut fixture = MessageFixture::new(Reasoning::with_answer("", "finished")).await;
    let id = fixture.start(serde_json::json!({"fork": true})).await;
    let receipt = fixture
        .invoke(serde_json::json!({"resume_thread_id": id, "prompt": "supplement-one"}))
        .await
        .unwrap();
    assert!(receipt.starts_with("action: send\nstatus: queued"));
    fixture.finish().await;
    assert_eq!(
        fixture.calls.load(Ordering::SeqCst),
        2,
        "末轮收到 Defer 必须再次推理消费补充任务"
    );
    let next = fixture.snapshots.recv().await.unwrap();
    assert!(next
        .iter()
        .any(|message| message.content().contains("supplement-one")));
    assert_eq!(fixture.factories.load(Ordering::SeqCst), 1);
    assert!(fixture
        .manager
        .send_subagent_message(&id, Some("late"))
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn test_active_message_resumed_background_execution_accepts_defer() {
    let mut fixture = MessageFixture::new(Reasoning::with_tools(
        "",
        vec![ToolCall::new("probe", "Probe", serde_json::json!({}))],
    ))
    .await;
    let id = uuid::Uuid::now_v7().to_string();
    // 被恢复的 thread 必须属于夹具父会话的同一执行根，否则 resume 会被归属校验拒绝。
    preset_resumable_thread(
        &fixture.store,
        &id,
        "fork",
        Some(fixture.parent_id.as_str()),
        Vec::new(),
    )
    .await;
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
    let mut fixture = MessageFixture::new(Reasoning::with_answer("", "finished")).await;
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
    let mut fixture = MessageFixture::new(Reasoning::with_answer("", "finished")).await;
    let id = fixture.start(serde_json::json!({"fork": true})).await;
    // 同库、同父会话的第二个工具实例：被拒绝的原因必须是「无活跃接收者」，
    // 而不是缺父身份——否则测不到 cross-session 拒绝本身。
    let stranger = make_subagent_tool(Vec::new())
        .with_session_resources(fixture.store.facade())
        .with_parent_thread_id(fixture.parent_id.clone())
        .with_parent_session(Arc::clone(&fixture.parent))
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
    #[derive(Clone)]
    struct PanicLlm;
    impl PanicLlm {
        async fn respond(
            &self,
            request: peri_model::ModelRequest,
            cancellation: tokio_util::sync::CancellationToken,
        ) -> Vec<peri_model::ModelResult<peri_model::ModelStreamEvent>> {
            use crate::subagent::test_support::*;
            let _ = &cancellation;
            let messages = base_messages(&request);
            let defined = defined_tools(&request);
            let tools: Vec<&dyn BaseTool> = defined.iter().map(|t| t as &dyn BaseTool).collect();

            panic!("模拟后台执行崩溃");
        }
    }
    crate::subagent::test_support::fixture_model_impl!(PanicLlm);
    let dir = tempdir().unwrap();
    let manager = Arc::new(TaskManager::new());
    let (tx, mut rx) = mpsc::unbounded_channel();
    let cleanup_manager = manager.clone();
    let tool = SubAgentTool::new(
        Arc::new(Vec::new()),
        None,
        Arc::new(|_| {
            crate::subagent::test_support::fixture_source(
                std::sync::Arc::new(PanicLlm),
                "fixture-scripted",
            )
        }),
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
