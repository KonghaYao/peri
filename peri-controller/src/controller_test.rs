//! Controller 控制面契约测试（Seam 2：`docs/top-level.md` §6/§9）。
//!
//! 只测外部行为不测实现细节（`docs/design/testing-standards.md` P0 分层）：
//! - 控制面五步：lite params → pick Resources → pick Runtime → run Session → pop events
//! - cancel 转发：按 (session_id, turn_id, attempt_id) 三元组定位，幂等判定归 Agent
//! - 会话生命周期面：join（deadline）/ destroy（§9 六阶段编排 + drain 双投递）/ 枚举
//! - 消息/工具注入面：submit_input 经 Runtime 透传句柄；LiteParams 初始消息/工具装载
//! - 事件协议化前分支：弹出队列 + 订阅（显式注册/退订；旁路消费者可订阅同一分支）

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use peri_acp_types::identity::{
    AttemptId, AttemptIdentity, CancelRequest, EventDeliveryClass, EventEnvelope, SessionEpoch,
    SessionSeq,
};
use peri_acp_types::messages::MessageContent;
use peri_acp_types::store::ThreadStore;
use peri_acp_types::thread::CancelPolicy;
use peri_resources::sessions::FilesystemThreadStore;
use peri_runtime::{Runtime, SessionHandle, UnstampedEvent};

use super::{AgentRef, Controller, LiteParams};

/// 记录型 mock 句柄：记录 run/cancel/submit_input 调用与销毁阶段顺序。
struct MockHandle {
    runs: Mutex<usize>,
    last_cancel: Mutex<Option<CancelRequest>>,
    calls: Mutex<Vec<&'static str>>,
    drained: Mutex<VecDeque<UnstampedEvent>>,
    last_input: Mutex<Option<MessageContent>>,
}

impl MockHandle {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            runs: Mutex::new(0),
            last_cancel: Mutex::new(None),
            calls: Mutex::new(Vec::new()),
            drained: Mutex::new(VecDeque::new()),
            last_input: Mutex::new(None),
        })
    }

    /// 带 drain 事件构造（销毁路径断言双投递用）。
    fn with_drained(drained: Vec<UnstampedEvent>) -> Arc<Self> {
        let handle = Self::new();
        *handle.drained.lock().unwrap() = drained.into();
        handle
    }

    fn run_count(&self) -> usize {
        *self.runs.lock().unwrap()
    }

    fn last_cancel(&self) -> Option<CancelRequest> {
        self.last_cancel.lock().unwrap().clone()
    }

    fn last_input(&self) -> Option<MessageContent> {
        self.last_input.lock().unwrap().clone()
    }

    /// 销毁阶段调用顺序（stop_accepting/cancel_owned/join/abort/persist/drain）。
    fn destroy_sequence(&self) -> Vec<&'static str> {
        self.calls.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl SessionHandle for MockHandle {
    async fn run(&self) -> Result<(), anyhow::Error> {
        *self.runs.lock().unwrap() += 1;
        Ok(())
    }

    fn cancel(&self, request: &CancelRequest) {
        *self.last_cancel.lock().unwrap() = Some(request.clone());
    }

    fn submit_input(&self, input: MessageContent) -> Result<(), anyhow::Error> {
        *self.last_input.lock().unwrap() = Some(input);
        Ok(())
    }

    fn stop_accepting(&self) {
        self.calls.lock().unwrap().push("stop_accepting");
    }
    fn cancel_owned(&self) {
        self.calls.lock().unwrap().push("cancel_owned");
    }
    async fn join(&self, _deadline: Duration) -> bool {
        self.calls.lock().unwrap().push("join");
        true
    }
    fn abort(&self) {
        self.calls.lock().unwrap().push("abort");
    }
    async fn persist(&self) -> Result<(), anyhow::Error> {
        self.calls.lock().unwrap().push("persist");
        Ok(())
    }
    fn drain(&self) -> Vec<UnstampedEvent> {
        self.calls.lock().unwrap().push("drain");
        self.drained.lock().unwrap().drain(..).collect()
    }
}

/// 构造未补打事件（默认 Critical 交付）。
fn ev(turn_id: &str, agent_id: &str) -> UnstampedEvent {
    UnstampedEvent {
        turn_id: turn_id.to_string(),
        agent_id: agent_id.to_string(),
        message_id: None,
        delivery_class: EventDeliveryClass::Critical,
    }
}

/// 构造临时 ThreadStore（Filesystem，与 peri-acp 既有测试同模式）。
fn temp_store() -> Arc<dyn ThreadStore> {
    let tmp = tempfile::tempdir().unwrap();
    Arc::new(FilesystemThreadStore::new(tmp.path().join("threads")))
}

// ─── lite params（§6 控制面第一步） ──────────────────────────────────────────

#[test]
fn lite_params_construction() {
    let params = LiteParams::new(
        "session-1",
        AgentRef::new("default"),
        "/tmp/proj",
        Some("hello".to_string()),
    );
    assert_eq!(params.session_id, "session-1");
    assert_eq!(params.agent_ref.as_str(), "default");
    assert_eq!(params.cwd, std::path::PathBuf::from("/tmp/proj"));
    assert_eq!(params.initial_input.as_deref(), Some("hello"));

    // 注入面默认空（initial_messages/tools 显式空声明，不伪装缺省）
    assert!(params.initial_messages.is_empty());
    assert!(params.tools.is_empty());

    // 无初始输入：None 显式表达（不伪装空串）
    let bare = LiteParams::new("session-2", AgentRef::new("default"), "/tmp", None);
    assert_eq!(bare.initial_input, None);

    // 初始消息/工具集装载（消息/工具注入面声明；消费方为 Agent 层工厂 L5）
    let injected = LiteParams::new("session-3", AgentRef::new("default"), "/tmp", None)
        .with_initial_messages(vec![MessageContent::text("seed")])
        .with_tools(vec![peri_agent::tools::ToolDefinition {
            name: "search".into(),
            description: "extra tool".into(),
            parameters: serde_json::json!({}),
        }]);
    assert_eq!(injected.initial_messages.len(), 1);
    assert_eq!(injected.initial_messages[0], MessageContent::text("seed"));
    assert_eq!(injected.tools.len(), 1);
    assert_eq!(injected.tools[0].name, "search");
}

// ─── pick Resources / pick Runtime（§6 控制面第二/三步） ──────────────────────

#[test]
fn pick_resources_none_until_injected() {
    let controller = Controller::new(temp_store());
    assert!(
        controller.pick_resources().is_none(),
        "未注入时 Resources 为 None"
    );

    // 注入后可取（Resources 为 Clone 门面；此处用默认构造会失败——用 None 语义验证）
    // 实际注入测试见 pick_runtime_and_resources_injection。
}

#[test]
fn pick_runtime_injection_replaces_default() {
    let controller = Controller::new(temp_store());
    let injected = Arc::new(Runtime::new());
    let controller = controller.with_runtime(Arc::clone(&injected));
    assert!(
        Arc::ptr_eq(&controller.pick_runtime(), &injected),
        "pick Runtime 返回注入实例"
    );
}

// ─── run Session（§6 控制面第四步：Controller → Runtime → SessionHandle） ─────

#[tokio::test]
async fn run_session_forwards_via_runtime_to_handle() {
    let handle = MockHandle::new();
    let runtime = Arc::new(Runtime::new());
    runtime.register("s1", Arc::clone(&handle)).unwrap();
    let controller = Controller::new(temp_store()).with_runtime(runtime);

    controller.run_session("s1").await.unwrap();

    assert_eq!(handle.run_count(), 1, "run Session 经 Runtime 映射直达句柄");
}

#[tokio::test]
async fn run_session_unknown_session_typed_error() {
    let controller = Controller::new(temp_store());
    let err = controller.run_session("missing").await.unwrap_err();
    assert!(
        matches!(&err, super::ControllerError::RunFailed(s, _) if s == "missing"),
        "未注册 session 包 context 为 RunFailed: {err}"
    );
}

// ─── cancel 转发（§6/§9：三元组定位，幂等判定归 Agent） ───────────────────────

#[tokio::test]
async fn cancel_forwards_triple_to_handle() {
    let handle = MockHandle::new();
    let runtime = Arc::new(Runtime::new());
    runtime.register("s1", Arc::clone(&handle)).unwrap();
    let controller = Controller::new(temp_store()).with_runtime(runtime);

    let req = CancelRequest::new(
        AttemptIdentity::new("s1", SessionEpoch::initial(), "turn-7", AttemptId::new()),
        CancelPolicy::Cascade,
    );
    controller.cancel(&req).unwrap();

    let received = handle.last_cancel().expect("cancel 请求应到达句柄");
    assert_eq!(
        received, req,
        "cancel 请求完整透传（三元组 + policy + clear_queue）"
    );
    assert_eq!(received.identity.session_id, "s1");
    assert_eq!(received.identity.turn_id, "turn-7");
}

#[tokio::test]
async fn cancel_unknown_session_typed_error() {
    let controller = Controller::new(temp_store());
    let req = CancelRequest::new(
        AttemptIdentity::new(
            "missing",
            SessionEpoch::initial(),
            "turn-7",
            AttemptId::new(),
        ),
        CancelPolicy::Cascade,
    );
    let err = controller.cancel(&req).unwrap_err();
    assert!(
        matches!(&err, super::ControllerError::CancelFailed(s, _) if s == "missing"),
        "未注册 session 包 context 为 CancelFailed: {err}"
    );
}

// ─── 事件协议化前分支（§6 pop events / §6 观测订阅） ───────────────────────────

#[tokio::test]
async fn publish_pop_and_subscribe_events() {
    let controller = Controller::new(temp_store());
    let mut sub = controller.subscribe();

    let e1 = EventEnvelope::new(
        "s1",
        SessionEpoch::initial(),
        "t1",
        "a1",
        SessionSeq::initial(),
        EventDeliveryClass::Critical,
    );
    let e2 = EventEnvelope::new(
        "s1",
        SessionEpoch::initial(),
        "t2",
        "a1",
        SessionSeq::initial().next(),
        EventDeliveryClass::Broadcast,
    );
    controller.publish(e1.clone());
    controller.publish(e2.clone());

    // pop events：按投递序返回全部已入队事件（控制面第五步）
    let popped = controller.pop_events();
    assert_eq!(popped.len(), 2);
    assert_eq!(popped[0].envelope.turn_id, "t1");
    assert_eq!(popped[1].envelope.turn_id, "t2");

    // pop 后队列为空（事件已弹出）
    assert!(controller.pop_events().is_empty());

    // 订阅分支：订阅者收到同一事件（ACP 协议化输入）
    let received = sub.recv().await.unwrap();
    assert_eq!(received.envelope, e1);
    let received = sub.recv().await.unwrap();
    assert_eq!(received.envelope, e2);
}

#[tokio::test]
async fn bypass_consumer_subscribes_same_branch() {
    let controller = Controller::new(temp_store());
    // 主订阅（ACP 协议化）+ 旁路订阅（Langfuse bridge 形态：旁路消费者不参与业务链路）
    let mut acp = controller.subscribe();
    let mut observer = controller.subscribe();

    let env = EventEnvelope::new(
        "s1",
        SessionEpoch::initial(),
        "t1",
        "a1",
        SessionSeq::initial(),
        EventDeliveryClass::Broadcast,
    );
    controller.publish(env.clone());

    assert_eq!(
        acp.recv().await.unwrap().envelope,
        env,
        "ACP 订阅者收到事件"
    );
    assert_eq!(
        observer.recv().await.unwrap().envelope,
        env,
        "旁路消费者（观测）收到同一事件"
    );
    assert_eq!(controller.pop_events().len(), 1, "弹出队列独立于订阅者");
}

// ─── sessions 存储通道（既有访问路径不回归） ───────────────────────────────────

#[test]
fn sessions_channel_preserved() {
    let store = temp_store();
    let controller = Controller::new(Arc::clone(&store));
    assert!(
        Arc::ptr_eq(&controller.sessions(), &store),
        "sessions 通道保持同一存储"
    );
}

// ─── 会话生命周期（枚举 / join / destroy） ─────────────────────────────────────

#[tokio::test]
async fn session_enumeration_reflects_runtime_map() {
    let runtime = Arc::new(Runtime::new());
    runtime.register("s1", MockHandle::new()).unwrap();
    runtime.register("s2", MockHandle::new()).unwrap();
    let controller = Controller::new(temp_store()).with_runtime(runtime);

    let mut ids = controller.session_ids();
    ids.sort();
    assert_eq!(ids, vec!["s1".to_string(), "s2".to_string()]);
    assert!(controller.contains_session("s1"));
    assert!(!controller.contains_session("missing"));

    // 销毁后枚举随之更新（list_sessions 语义的 Runtime 映射侧）
    controller
        .destroy_session("s1", Duration::from_secs(5))
        .await
        .unwrap();
    assert_eq!(controller.session_ids(), vec!["s2".to_string()]);
}

#[tokio::test]
async fn join_session_forwards_deadline_result() {
    let handle = MockHandle::new();
    let runtime = Arc::new(Runtime::new());
    runtime.register("s1", Arc::clone(&handle)).unwrap();
    let controller = Controller::new(temp_store()).with_runtime(runtime);

    assert!(
        controller
            .join_session("s1", Duration::from_secs(5))
            .await
            .unwrap(),
        "deadline 内结束返回 true"
    );
    assert_eq!(handle.destroy_sequence(), vec!["join"]);

    // 未注册 session：类型化错误
    let err = controller
        .join_session("missing", Duration::from_secs(1))
        .await
        .unwrap_err();
    assert!(
        matches!(&err, super::ControllerError::JoinFailed(s, _) if s == "missing"),
        "未注册 session 包 context 为 JoinFailed: {err}"
    );
}

#[tokio::test]
async fn destroy_session_orchestrates_phases_and_publishes_drained() {
    let handle = MockHandle::with_drained(vec![ev("t1", "a1"), ev("t2", "a1")]);
    let runtime = Arc::new(Runtime::new());
    runtime.register("s1", Arc::clone(&handle)).unwrap();
    let controller = Controller::new(temp_store()).with_runtime(runtime);
    let mut sub = controller.subscribe();

    let drained = controller
        .destroy_session("s1", Duration::from_secs(5))
        .await
        .unwrap();

    // §9 六阶段编排顺序（经 Runtime::destroy 固定：停收 → 取消 owned → join →
    // 未超时无 abort → 持久化 → drain）
    assert_eq!(
        handle.destroy_sequence(),
        vec!["stop_accepting", "cancel_owned", "join", "persist", "drain"]
    );
    // 映射移除（枚举反映）
    assert!(!controller.contains_session("s1"));

    // drain 事件三路可见：返回值 + 弹出队列 + 订阅分支（publish 双投递）
    assert_eq!(drained.len(), 2);
    assert_eq!(drained[0].session_id, "s1");
    assert_eq!(drained[0].session_seq, SessionSeq::initial());
    let popped = controller.pop_events();
    assert_eq!(
        popped
            .iter()
            .map(|m| m.envelope.clone())
            .collect::<Vec<_>>(),
        drained,
        "drain 事件进入弹出队列（投递序一致）"
    );
    assert_eq!(
        sub.recv().await.unwrap().envelope,
        drained[0],
        "drain 事件进入订阅分支"
    );
    assert_eq!(sub.recv().await.unwrap().envelope, drained[1]);
}

#[tokio::test]
async fn destroy_session_unknown_typed_error() {
    let controller = Controller::new(temp_store());
    let err = controller
        .destroy_session("missing", Duration::from_secs(5))
        .await
        .unwrap_err();
    assert!(
        matches!(&err, super::ControllerError::DestroyFailed(s, _) if s == "missing"),
        "未注册 session 包 context 为 DestroyFailed: {err}"
    );
}

// ─── 消息/工具注入面（submit_input 经 Runtime 透传） ───────────────────────────

#[test]
fn submit_input_forwards_via_runtime_to_handle() {
    let handle = MockHandle::new();
    let runtime = Arc::new(Runtime::new());
    runtime.register("s1", Arc::clone(&handle)).unwrap();
    let controller = Controller::new(temp_store()).with_runtime(runtime);

    controller
        .submit_input("s1", MessageContent::text("hi"))
        .unwrap();
    assert_eq!(
        handle.last_input(),
        Some(MessageContent::text("hi")),
        "运行时输入经 Controller → Runtime 透传到句柄"
    );

    // 未注册 session：类型化错误
    let err = controller
        .submit_input("missing", MessageContent::text("hi"))
        .unwrap_err();
    assert!(
        matches!(&err, super::ControllerError::InjectFailed(s, _) if s == "missing"),
        "未注册 session 包 context 为 InjectFailed: {err}"
    );
}

// ─── 订阅显式注册/退订（Subscription 句柄） ────────────────────────────────────

#[tokio::test]
async fn subscription_register_and_unsubscribe() {
    let controller = Controller::new(temp_store());
    let mut sub = controller.subscribe();

    // 注册：收到 publish 事件
    let e1 = EventEnvelope::new(
        "s1",
        SessionEpoch::initial(),
        "t1",
        "a1",
        SessionSeq::initial(),
        EventDeliveryClass::Critical,
    );
    controller.publish(e1.clone());
    assert_eq!(sub.recv().await.unwrap().envelope, e1, "注册订阅者收到事件");

    // 显式退订：drop 接收端；不影响其他订阅者（broadcast 语义）
    let mut observer = controller.subscribe();
    sub.unsubscribe();
    let e2 = EventEnvelope::new(
        "s1",
        SessionEpoch::initial(),
        "t2",
        "a1",
        SessionSeq::initial().next(),
        EventDeliveryClass::Broadcast,
    );
    controller.publish(e2.clone());
    assert_eq!(
        observer.recv().await.unwrap().envelope,
        e2,
        "退订不影响其他订阅者"
    );
    assert_eq!(controller.pop_events().len(), 2, "弹出队列独立于订阅者");
}
