//! Controller 控制面契约测试（Seam 2：`docs/top-level.md` §6/§9）。
//!
//! 只测外部行为不测实现细节（`docs/design/testing-standards.md` P0 分层）：
//! - 控制面五步：lite params → pick Resources → pick Runtime → run Session → 消费事件
//! - cancel 转发：按 (session_id, turn_id, attempt_id) 三元组定位，幂等判定归 Agent
//! - 会话生命周期面：join（deadline）/ destroy（§9 六阶段编排 + drain 投递）/ 枚举
//! - 消息/工具注入面：submit_input 经 Runtime 透传句柄；LiteParams 初始消息/工具装载
//! - 事件协议化前分支：订阅广播（显式注册/退订；旁路消费者可订阅同一分支）

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use peri_acp_types::identity::{
    AttemptId, AttemptIdentity, CancelRequest, EventDeliveryClass, EventEnvelope, SessionEpoch,
    SessionSeq,
};
use peri_acp_types::messages::MessageContent;
use peri_acp_types::session_resources::SessionResources;
use peri_acp_types::thread::CancelPolicy;
use peri_runtime::{Runtime, SessionHandle, UnstampedEvent};

use crate::controller::AgentRef;
use crate::controller::Controller;
use crate::controller::LiteParams;

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

    /// 带 drain 事件构造（销毁路径断言事件投递用）。
    fn with_drained(drained: Vec<UnstampedEvent>) -> Arc<Self> {
        let handle = Self::new();
        *handle
            .drained
            .lock()
            .expect("handle drained mutex must remain unpoisoned throughout the test") =
            drained.into();
        handle
    }

    fn run_count(&self) -> usize {
        *self
            .runs
            .lock()
            .expect("self runs mutex must remain unpoisoned throughout the test")
    }

    fn last_cancel(&self) -> Option<CancelRequest> {
        self.last_cancel
            .lock()
            .expect("self last cancel mutex must remain unpoisoned throughout the test")
            .clone()
    }

    fn last_input(&self) -> Option<MessageContent> {
        self.last_input
            .lock()
            .expect("self last input mutex must remain unpoisoned throughout the test")
            .clone()
    }

    /// 销毁阶段调用顺序（stop_accepting/cancel_owned/join/abort/persist/drain）。
    fn destroy_sequence(&self) -> Vec<&'static str> {
        self.calls
            .lock()
            .expect("self calls mutex must remain unpoisoned throughout the test")
            .clone()
    }
}

#[async_trait::async_trait]
impl SessionHandle for MockHandle {
    async fn run(&self) -> Result<(), anyhow::Error> {
        *self
            .runs
            .lock()
            .expect("self runs mutex must remain unpoisoned throughout the test") += 1;
        Ok(())
    }

    fn cancel(&self, request: &CancelRequest) {
        *self
            .last_cancel
            .lock()
            .expect("self last cancel mutex must remain unpoisoned throughout the test") =
            Some(request.clone());
    }

    fn submit_input(&self, input: MessageContent) -> Result<(), anyhow::Error> {
        *self
            .last_input
            .lock()
            .expect("self last input mutex must remain unpoisoned throughout the test") =
            Some(input);
        Ok(())
    }

    fn stop_accepting(&self) {
        self.calls
            .lock()
            .expect("self calls mutex must remain unpoisoned throughout the test")
            .push("stop_accepting");
    }
    fn cancel_owned(&self) {
        self.calls
            .lock()
            .expect("self calls mutex must remain unpoisoned throughout the test")
            .push("cancel_owned");
    }
    async fn join(&self, _deadline: Duration) -> bool {
        self.calls
            .lock()
            .expect("self calls mutex must remain unpoisoned throughout the test")
            .push("join");
        true
    }
    fn abort(&self) {
        self.calls
            .lock()
            .expect("self calls mutex must remain unpoisoned throughout the test")
            .push("abort");
    }
    async fn persist(&self) -> Result<(), anyhow::Error> {
        self.calls
            .lock()
            .expect("self calls mutex must remain unpoisoned throughout the test")
            .push("persist");
        Ok(())
    }
    fn drain(&self) -> Vec<UnstampedEvent> {
        self.calls
            .lock()
            .expect("self calls mutex must remain unpoisoned throughout the test")
            .push("drain");
        self.drained
            .lock()
            .expect("self drained mutex must remain unpoisoned throughout the test")
            .drain(..)
            .collect()
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

/// 门面夹具：临时目录里的真实 SQLite 门面（Controller 只转发句柄，不解释存储语义）。
async fn temp_facade() -> Arc<dyn SessionResources> {
    let tmp = tempfile::tempdir().expect("the temporary test directory must be available");
    let facade =
        peri_resources::sessions::SessionResourcesImpl::open(tmp.path().join("threads.db"))
            .await
            .expect("the temporary session store fixture must open successfully");
    Arc::new(facade)
}

// ─── lite params（§6 控制面第一步） ──────────────────────────────────────────

#[tokio::test]
async fn lite_params_construction() {
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

// ─── 会话资源句柄 / pick Runtime（§6 控制面第二/三步） ────────────────────────

#[tokio::test]
async fn pick_runtime_injection_replaces_default() {
    let controller = Controller::new(temp_facade().await);
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
    runtime
        .register("s1", Arc::clone(&handle))
        .expect("the register fixture result is required for this assertion");
    let controller = Controller::new(temp_facade().await).with_runtime(runtime);

    controller
        .run_session("s1")
        .await
        .expect("the run session fixture result is required for this assertion");

    assert_eq!(handle.run_count(), 1, "run Session 经 Runtime 映射直达句柄");
}

#[tokio::test]
async fn run_session_unknown_session_typed_error() {
    let controller = Controller::new(temp_facade().await);
    let err = controller.run_session("missing").await.unwrap_err();
    assert!(
        matches!(&err, crate::error::ControllerError::RunFailed(s, _) if s == "missing"),
        "未注册 session 包 context 为 RunFailed: {err}"
    );
}

// ─── cancel 转发（§6/§9：三元组定位，幂等判定归 Agent） ───────────────────────

#[tokio::test]
async fn cancel_forwards_triple_to_handle() {
    let handle = MockHandle::new();
    let runtime = Arc::new(Runtime::new());
    runtime
        .register("s1", Arc::clone(&handle))
        .expect("the register fixture result is required for this assertion");
    let controller = Controller::new(temp_facade().await).with_runtime(runtime);

    let req = CancelRequest::new(
        AttemptIdentity::new("s1", SessionEpoch::initial(), "turn-7", AttemptId::new()),
        CancelPolicy::Cascade,
    );
    controller
        .cancel(&req)
        .expect("the cancel fixture result is required for this assertion");

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
    let controller = Controller::new(temp_facade().await);
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
        matches!(&err, crate::error::ControllerError::CancelFailed(s, _) if s == "missing"),
        "未注册 session 包 context 为 CancelFailed: {err}"
    );
}

// ─── 事件协议化前分支（§6 订阅） ──────────────────────────────────────────────

#[tokio::test]
async fn publish_and_subscribe_events() {
    let controller = Controller::new(temp_facade().await);
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

    // 订阅分支：订阅者按投递序收到事件（ACP 协议化输入）
    let received = sub
        .recv()
        .await
        .expect("sub recv producer must deliver the expected test event before closing");
    assert_eq!(received.envelope, e1);
    let received = sub
        .recv()
        .await
        .expect("sub recv producer must deliver the expected test event before closing");
    assert_eq!(received.envelope, e2);
}

#[tokio::test]
async fn bypass_consumer_subscribes_same_branch() {
    let controller = Controller::new(temp_facade().await);
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
        acp.recv()
            .await
            .expect("acp recv producer must deliver the expected test event before closing")
            .envelope,
        env,
        "ACP 订阅者收到事件"
    );
    assert_eq!(
        observer
            .recv()
            .await
            .expect("observer recv producer must deliver the expected test event before closing")
            .envelope,
        env,
        "旁路消费者（观测）收到同一事件"
    );
}

#[tokio::test]
async fn publish_event_preserves_source_message_id_when_session_is_unregistered() {
    let controller = Controller::new(temp_facade().await);
    let mut sub = controller.subscribe();
    let source = UnstampedEvent::new(
        "turn-1",
        "agent-1",
        Some("message-1".into()),
        EventDeliveryClass::Broadcast,
    );

    controller.publish_event(
        "late-session",
        &source,
        peri_acp_types::event::ExecutorEvent::SystemNotification {
            text: "late event".into(),
            level: "info".into(),
        },
    );

    let received = sub.recv().await.expect("event should reach subscribers");
    assert_eq!(received.envelope.message_id.as_deref(), Some("message-1"));
    assert_eq!(received.envelope.turn_id, "turn-1");
    assert!(received.event.is_some(), "v1 payload 随订阅分支投递");
}

// ─── sessions 存储通道（既有访问路径不回归） ───────────────────────────────────

#[tokio::test]
async fn sessions_channel_preserved() {
    let facade = temp_facade().await;
    let controller = Controller::new(Arc::clone(&facade));
    assert!(
        Arc::ptr_eq(&controller.sessions(), &facade),
        "sessions 通道保持同一门面句柄"
    );
}

// ─── 会话生命周期（枚举 / join / destroy） ─────────────────────────────────────

#[tokio::test]
async fn session_enumeration_reflects_runtime_map() {
    let runtime = Arc::new(Runtime::new());
    runtime
        .register("s1", MockHandle::new())
        .expect("the register fixture result is required for this assertion");
    runtime
        .register("s2", MockHandle::new())
        .expect("the register fixture result is required for this assertion");
    let controller = Controller::new(temp_facade().await).with_runtime(runtime);

    let mut ids = controller.session_ids();
    ids.sort();
    assert_eq!(ids, vec!["s1".to_string(), "s2".to_string()]);
    assert!(controller.contains_session("s1"));
    assert!(!controller.contains_session("missing"));

    // 销毁后枚举随之更新（list_sessions 语义的 Runtime 映射侧）
    controller
        .destroy_session("s1", Duration::from_secs(5))
        .await
        .expect("the destroy session fixture result is required for this assertion");
    assert_eq!(controller.session_ids(), vec!["s2".to_string()]);
}

#[tokio::test]
async fn join_session_forwards_deadline_result() {
    let handle = MockHandle::new();
    let runtime = Arc::new(Runtime::new());
    runtime
        .register("s1", Arc::clone(&handle))
        .expect("the register fixture result is required for this assertion");
    let controller = Controller::new(temp_facade().await).with_runtime(runtime);

    assert!(
        controller
            .join_session("s1", Duration::from_secs(5))
            .await
            .expect("the from secs fixture result is required for this assertion"),
        "deadline 内结束返回 true"
    );
    assert_eq!(handle.destroy_sequence(), vec!["join"]);

    // 未注册 session：类型化错误
    let err = controller
        .join_session("missing", Duration::from_secs(1))
        .await
        .unwrap_err();
    assert!(
        matches!(&err, crate::error::ControllerError::JoinFailed(s, _) if s == "missing"),
        "未注册 session 包 context 为 JoinFailed: {err}"
    );
}

#[tokio::test]
async fn destroy_session_orchestrates_phases_and_publishes_drained() {
    let handle = MockHandle::with_drained(vec![ev("t1", "a1"), ev("t2", "a1")]);
    let runtime = Arc::new(Runtime::new());
    runtime
        .register("s1", Arc::clone(&handle))
        .expect("the register fixture result is required for this assertion");
    let controller = Controller::new(temp_facade().await).with_runtime(runtime);
    let mut sub = controller.subscribe();

    let drained = controller
        .destroy_session("s1", Duration::from_secs(5))
        .await
        .expect("the destroy session fixture result is required for this assertion");

    // §9 六阶段编排顺序（经 Runtime::destroy 固定：停收 → 取消 owned → join →
    // 未超时无 abort → 持久化 → drain）
    assert_eq!(
        handle.destroy_sequence(),
        vec!["stop_accepting", "cancel_owned", "join", "persist", "drain"]
    );
    // 映射移除（枚举反映）
    assert!(!controller.contains_session("s1"));

    // drain 事件两路可见：返回值 + 订阅分支（publish 投递）
    assert_eq!(drained.len(), 2);
    assert_eq!(drained[0].session_id, "s1");
    assert_eq!(drained[0].session_seq, SessionSeq::initial());
    assert_eq!(
        sub.recv()
            .await
            .expect("sub recv producer must deliver the expected test event before closing")
            .envelope,
        drained[0],
        "drain 事件进入订阅分支"
    );
    assert_eq!(
        sub.recv()
            .await
            .expect("sub recv producer must deliver the expected test event before closing")
            .envelope,
        drained[1]
    );
}

#[tokio::test]
async fn destroy_session_unknown_typed_error() {
    let controller = Controller::new(temp_facade().await);
    let err = controller
        .destroy_session("missing", Duration::from_secs(5))
        .await
        .unwrap_err();
    assert!(
        matches!(&err, crate::error::ControllerError::DestroyFailed(s, _) if s == "missing"),
        "未注册 session 包 context 为 DestroyFailed: {err}"
    );
}

// ─── 消息/工具注入面（submit_input 经 Runtime 透传） ───────────────────────────

#[tokio::test]
async fn submit_input_forwards_via_runtime_to_handle() {
    let handle = MockHandle::new();
    let runtime = Arc::new(Runtime::new());
    runtime
        .register("s1", Arc::clone(&handle))
        .expect("the register fixture result is required for this assertion");
    let controller = Controller::new(temp_facade().await).with_runtime(runtime);

    controller
        .submit_input("s1", MessageContent::text("hi"))
        .expect("the submit input fixture result is required for this assertion");
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
        matches!(&err, crate::error::ControllerError::InjectFailed(s, _) if s == "missing"),
        "未注册 session 包 context 为 InjectFailed: {err}"
    );
}

// ─── 订阅显式注册/退订（Subscription 句柄） ────────────────────────────────────

#[tokio::test]
async fn subscription_register_and_unsubscribe() {
    let controller = Controller::new(temp_facade().await);
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
    assert_eq!(
        sub.recv()
            .await
            .expect("sub recv producer must deliver the expected test event before closing")
            .envelope,
        e1,
        "注册订阅者收到事件"
    );

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
        observer
            .recv()
            .await
            .expect("observer recv producer must deliver the expected test event before closing")
            .envelope,
        e2,
        "退订不影响其他订阅者"
    );
}

/// 广播交付语义：慢消费者不静默丢事件，而是显式收到 [`SubscriptionError::Lagged`]。
#[tokio::test]
async fn slow_subscriber_sees_lagged_instead_of_silent_drop() {
    let controller = Controller::new(temp_facade().await);
    let mut sub = controller.subscribe();
    let env = EventEnvelope::new(
        "s1",
        SessionEpoch::initial(),
        "t1",
        "a1",
        SessionSeq::initial(),
        EventDeliveryClass::Critical,
    );
    for _ in 0..=crate::controller::EVENT_BROADCAST_CAPACITY {
        controller.publish(env.clone());
    }
    let err = sub.recv().await.unwrap_err();
    assert!(
        matches!(err, crate::error::SubscriptionError::Lagged(skipped) if skipped > 0),
        "慢消费者必须显式收到 Lagged: {err:?}"
    );
}
