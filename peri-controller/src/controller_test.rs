//! Controller 控制面契约测试（Seam 2：`docs/top-level.md` §6/§9）。
//!
//! 只测外部行为不测实现细节（`docs/design/testing-standards.md` P0 分层）：
//! - 控制面五步：lite params → pick Resources → pick Runtime → run Session → pop events
//! - cancel 转发：按 (session_id, turn_id, attempt_id) 三元组定位，幂等判定归 Agent
//! - 事件协议化前分支：弹出队列 + 订阅（旁路消费者可订阅同一分支）

use std::sync::{Arc, Mutex};
use std::time::Duration;

use peri_acp_types::identity::{
    AttemptId, AttemptIdentity, CancelRequest, EventDeliveryClass, EventEnvelope, SessionEpoch,
    SessionSeq,
};
use peri_acp_types::store::ThreadStore;
use peri_acp_types::thread::CancelPolicy;
use peri_resources::sessions::FilesystemThreadStore;
use peri_runtime::{Runtime, SessionHandle, UnstampedEvent};

use super::{AgentRef, Controller, LiteParams};

/// 记录型 mock 句柄：记录 run/cancel 调用与收到的 cancel 请求。
struct MockHandle {
    runs: Mutex<usize>,
    last_cancel: Mutex<Option<CancelRequest>>,
}

impl MockHandle {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            runs: Mutex::new(0),
            last_cancel: Mutex::new(None),
        })
    }

    fn run_count(&self) -> usize {
        *self.runs.lock().unwrap()
    }

    fn last_cancel(&self) -> Option<CancelRequest> {
        self.last_cancel.lock().unwrap().clone()
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

    fn stop_accepting(&self) {}
    fn cancel_owned(&self) {}
    async fn join(&self, _deadline: Duration) -> bool {
        true
    }
    fn abort(&self) {}
    async fn persist(&self) -> Result<(), anyhow::Error> {
        Ok(())
    }
    fn drain(&self) -> Vec<UnstampedEvent> {
        Vec::new()
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

    // 无初始输入：None 显式表达（不伪装空串）
    let bare = LiteParams::new("session-2", AgentRef::new("default"), "/tmp", None);
    assert_eq!(bare.initial_input, None);
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
    assert_eq!(popped[0].turn_id, "t1");
    assert_eq!(popped[1].turn_id, "t2");

    // pop 后队列为空（事件已弹出）
    assert!(controller.pop_events().is_empty());

    // 订阅分支：订阅者收到同一事件（ACP 协议化输入）
    let received = sub.recv().await.unwrap();
    assert_eq!(received, e1);
    let received = sub.recv().await.unwrap();
    assert_eq!(received, e2);
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

    assert_eq!(acp.recv().await.unwrap(), env, "ACP 订阅者收到事件");
    assert_eq!(
        observer.recv().await.unwrap(),
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
