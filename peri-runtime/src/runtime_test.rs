//! §9 时序契约测试：映射增删、事件补打（session_id/session_seq 单调）、
//! 销毁顺序（停收 → 取消 → join → abort → 持久化 → drain → 移除）。
//!
//! 只测外部行为不测实现细节（`docs/design/testing-standards.md` P0 分层）：
//! 句柄以记录型 mock 替换 trait 接口，验证 Runtime 编排契约。

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use peri_acp_types::identity::{
    AttemptId, AttemptIdentity, CancelRequest, EventDeliveryClass, SessionEpoch, SessionSeq,
};
use peri_acp_types::messages::MessageContent;
use peri_acp_types::thread::CancelPolicy;

use super::{Runtime, RuntimeError, SessionHandle, UnstampedEvent};

/// 记录型 mock 句柄：记录阶段调用顺序，join/persist/drain 行为可配置。
struct MockHandle {
    calls: Mutex<Vec<&'static str>>,
    join_ok: AtomicBool,
    persist_ok: AtomicBool,
    drained: Mutex<VecDeque<UnstampedEvent>>,
    /// 最近收到的 cancel 请求（断言三元组透传/幂等转发用）。
    last_cancel: Mutex<Option<CancelRequest>>,
    /// 最近收到的运行时输入（submit_input 透传断言用）。
    last_input: Mutex<Option<MessageContent>>,
    /// submit_input 是否报错（注入失败路径断言用）。
    submit_ok: AtomicBool,
}

impl MockHandle {
    fn new(join_ok: bool, drained: Vec<UnstampedEvent>) -> Arc<Self> {
        Arc::new(Self {
            calls: Mutex::new(Vec::new()),
            join_ok: AtomicBool::new(join_ok),
            persist_ok: AtomicBool::new(true),
            drained: Mutex::new(drained.into()),
            last_cancel: Mutex::new(None),
            last_input: Mutex::new(None),
            submit_ok: AtomicBool::new(true),
        })
    }

    fn call_sequence(&self) -> Vec<&'static str> {
        self.calls.lock().unwrap().clone()
    }

    fn last_cancel(&self) -> Option<CancelRequest> {
        self.last_cancel.lock().unwrap().clone()
    }

    fn last_input(&self) -> Option<MessageContent> {
        self.last_input.lock().unwrap().clone()
    }

    fn cancel_count(&self) -> usize {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|c| **c == "cancel")
            .count()
    }
}

#[async_trait::async_trait]
impl SessionHandle for MockHandle {
    async fn run(&self) -> Result<(), anyhow::Error> {
        self.calls.lock().unwrap().push("run");
        Ok(())
    }

    fn cancel(&self, request: &CancelRequest) {
        self.calls.lock().unwrap().push("cancel");
        *self.last_cancel.lock().unwrap() = Some(request.clone());
    }

    fn submit_input(&self, input: MessageContent) -> Result<(), anyhow::Error> {
        self.calls.lock().unwrap().push("submit_input");
        *self.last_input.lock().unwrap() = Some(input);
        if self.submit_ok.load(Ordering::SeqCst) {
            Ok(())
        } else {
            Err(anyhow::anyhow!("submit input failed"))
        }
    }

    fn stop_accepting(&self) {
        self.calls.lock().unwrap().push("stop_accepting");
    }

    fn cancel_owned(&self) {
        self.calls.lock().unwrap().push("cancel_owned");
    }

    async fn join(&self, _deadline: Duration) -> bool {
        self.calls.lock().unwrap().push("join");
        self.join_ok.load(Ordering::SeqCst)
    }

    fn abort(&self) {
        self.calls.lock().unwrap().push("abort");
    }

    async fn persist(&self) -> Result<(), anyhow::Error> {
        self.calls.lock().unwrap().push("persist");
        if self.persist_ok.load(Ordering::SeqCst) {
            Ok(())
        } else {
            Err(anyhow::anyhow!("persist failed"))
        }
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

// ─── 映射增删 ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn map_register_lookup_remove() {
    let rt = Runtime::new();
    let h1 = MockHandle::new(true, vec![]);
    let h2 = MockHandle::new(true, vec![]);
    rt.register("s1", Arc::clone(&h1)).unwrap();
    rt.register("s2", Arc::clone(&h2)).unwrap();

    // 查询
    assert!(rt.contains("s1"));
    assert!(rt.contains("s2"));
    assert!(!rt.contains("missing"));
    assert_eq!(rt.session_ids().len(), 2);
    assert!(rt.handle("s1").is_some());
    assert!(rt.handle("missing").is_none());

    // 重复注册报错（防双注册撞车）
    let err = rt.register("s1", Arc::clone(&h1)).unwrap_err();
    assert!(matches!(
        err,
        RuntimeError::SessionAlreadyRegistered(s) if s == "s1"
    ));

    // 销毁后移除
    rt.destroy("s1", Duration::from_secs(5)).await.unwrap();
    assert!(!rt.contains("s1"));
    assert!(rt.handle("s1").is_none());
    assert_eq!(rt.session_ids(), vec!["s2".to_string()]);
}

// ─── 事件补打（session_id / session_seq 单调） ───────────────────────────────

#[tokio::test]
async fn stamp_fills_session_id_and_seq_monotonic() {
    let rt = Runtime::new();
    rt.register("s1", MockHandle::new(true, vec![])).unwrap();
    rt.register("s2", MockHandle::new(true, vec![])).unwrap();

    let e1 = rt.stamp("s1", &ev("t1", "a1")).unwrap();
    let e2 = rt.stamp("s1", &ev("t2", "a1")).unwrap();
    let e3 = rt.stamp("s1", &ev("t3", "a2")).unwrap();
    let f1 = rt.stamp("s2", &ev("t1", "a1")).unwrap();

    // session_id 按 session 维度补打（Agent 层事件不携带）
    assert_eq!(e1.session_id, "s1");
    assert_eq!(f1.session_id, "s2");

    // session_seq 同 session 单调递增，per-session 独立自 initial（1）起
    assert_eq!(e1.session_seq, SessionSeq::initial());
    assert!(e2.session_seq > e1.session_seq);
    assert!(e3.session_seq > e2.session_seq);
    assert_eq!(f1.session_seq, SessionSeq::initial());

    // turn_id / agent_id / session_epoch 透传
    assert_eq!(e1.turn_id, "t1");
    assert_eq!(e1.agent_id, "a1");
    assert_eq!(e3.turn_id, "t3");
    assert_eq!(e3.agent_id, "a2");
    assert_eq!(e1.session_epoch, SessionEpoch::initial());
    assert_eq!(e1.delivery_class, EventDeliveryClass::Critical);

    // 未注册 session 无法补打（迟到事件被拒）
    let err = rt.stamp("missing", &ev("t1", "a1")).unwrap_err();
    assert!(matches!(err, RuntimeError::UnknownSession(_)));
}

// ─── 销毁顺序（§9） ──────────────────────────────────────────────────────────

#[tokio::test]
async fn destroy_follows_order_and_drains_stamped_events() {
    let handle = MockHandle::new(true, vec![ev("t1", "a1"), ev("t2", "a1")]);
    let rt = Runtime::new();
    rt.register("s1", Arc::clone(&handle)).unwrap();

    let envelopes = rt.destroy("s1", Duration::from_secs(5)).await.unwrap();

    // 阶段顺序：停收 → 取消 owned → join →（未超时，无 abort）→ 持久化 → drain
    assert_eq!(
        handle.call_sequence(),
        vec!["stop_accepting", "cancel_owned", "join", "persist", "drain"]
    );

    // drain 事件在移除映射前补打：session_id 正确 + seq 单调
    assert_eq!(envelopes.len(), 2);
    assert_eq!(envelopes[0].session_id, "s1");
    assert_eq!(envelopes[0].session_seq, SessionSeq::initial());
    assert!(envelopes[1].session_seq > envelopes[0].session_seq);

    // 映射已移除：迟到事件无法补打
    assert!(!rt.contains("s1"));
    assert!(matches!(
        rt.stamp("s1", &ev("t3", "a1")),
        Err(RuntimeError::UnknownSession(_))
    ));
}

#[tokio::test]
async fn destroy_aborts_on_join_timeout() {
    let handle = MockHandle::new(false, vec![]); // join 超时
    let rt = Runtime::new();
    rt.register("s1", Arc::clone(&handle)).unwrap();

    rt.destroy("s1", Duration::from_millis(1)).await.unwrap();

    // 超时分支：join 后插入 abort，再继续持久化 → drain
    assert_eq!(
        handle.call_sequence(),
        vec![
            "stop_accepting",
            "cancel_owned",
            "join",
            "abort",
            "persist",
            "drain"
        ]
    );
    assert!(!rt.contains("s1"));
}

#[tokio::test]
async fn destroy_persist_failure_keeps_mapping() {
    let handle = MockHandle::new(true, vec![]);
    handle.persist_ok.store(false, Ordering::SeqCst);
    let rt = Runtime::new();
    rt.register("s1", Arc::clone(&handle)).unwrap();

    let err = rt.destroy("s1", Duration::from_secs(5)).await.unwrap_err();
    assert!(matches!(
        err,
        RuntimeError::PersistFailed(s, _) if s == "s1"
    ));

    // 映射保留（可重试销毁；已执行阶段幂等）
    assert!(rt.contains("s1"));
    // drain 未执行、映射未移除——但已执行到 persist（幂等重试安全）
    assert_eq!(
        handle.call_sequence(),
        vec!["stop_accepting", "cancel_owned", "join", "persist"]
    );
}

// ─── cancel / run 转发 ───────────────────────────────────────────────────────

/// 构造 cancel 请求（默认 Cascade、不清队列）。
fn cancel_req(session_id: &str, turn_id: &str) -> CancelRequest {
    CancelRequest::new(
        AttemptIdentity::new(
            session_id,
            SessionEpoch::initial(),
            turn_id,
            AttemptId::new(),
        ),
        CancelPolicy::Cascade,
    )
}

#[tokio::test]
async fn cancel_and_run_forward_to_handle() {
    let handle = MockHandle::new(true, vec![]);
    let rt = Runtime::new();
    rt.register("s1", Arc::clone(&handle)).unwrap();

    let req = cancel_req("s1", "t1");
    rt.cancel(&req).unwrap();
    rt.run("s1").await.unwrap();

    // 只定位与转发：cancel 与 run 直达句柄
    assert_eq!(handle.call_sequence(), vec!["cancel", "run"]);

    // 三元组随请求透传：句柄收到与请求一致的完整 cancel 请求（§9 身份契约）
    let received = handle.last_cancel().expect("cancel 请求应到达句柄");
    assert_eq!(received.identity.session_id, "s1");
    assert_eq!(received.identity.turn_id, "t1");
    assert_eq!(received.identity.attempt_id, req.identity.attempt_id);
    assert_eq!(received.policy, CancelPolicy::Cascade);
    assert!(!received.clear_queue, "默认不清除 MQ 待办");

    // 未注册 session：定位失败
    let missing = cancel_req("missing", "t1");
    assert!(matches!(
        rt.cancel(&missing),
        Err(RuntimeError::UnknownSession(_))
    ));
    assert!(matches!(
        rt.run("missing").await,
        Err(RuntimeError::UnknownSession(_))
    ));
}

// ─── cancel 幂等（§9：重复 cancel 针对同一三元组结果一致） ─────────────────────

#[tokio::test]
async fn cancel_idempotent_repeated_forward_same_request() {
    let handle = MockHandle::new(true, vec![]);
    let rt = Runtime::new();
    rt.register("s1", Arc::clone(&handle)).unwrap();

    let req = cancel_req("s1", "t1");
    // 同一三元组重复 cancel：两次均成功转发同一请求（不报错、不翻转状态）；
    // 终态唯一（Completed/Interrupted）由 Agent 侧判定，Runtime 不解释语义
    rt.cancel(&req).unwrap();
    rt.cancel(&req).unwrap();

    assert_eq!(handle.cancel_count(), 2);
    let received = handle.last_cancel().unwrap();
    assert_eq!(received, req, "重复 cancel 转发同一请求（结果一致）");

    // 不同 attempt 是不同 cancel 目标（attempt_id 不可复用，幂等按三元组判定）
    let other_req = CancelRequest::new(
        AttemptIdentity::new("s1", SessionEpoch::initial(), "t1", AttemptId::new()),
        CancelPolicy::Cascade,
    );
    rt.cancel(&other_req).unwrap();
    assert_eq!(handle.cancel_count(), 3);
    assert_ne!(
        handle.last_cancel().unwrap().identity.attempt_id,
        req.identity.attempt_id,
        "attempt_id 不可复用：不同 attempt 的 cancel 目标不同"
    );
}

// ─── clear_queue / policy 透传（§9 cancel 契约） ─────────────────────────────

#[tokio::test]
async fn cancel_passes_clear_queue_and_policy() {
    let handle = MockHandle::new(true, vec![]);
    let rt = Runtime::new();
    rt.register("s1", Arc::clone(&handle)).unwrap();

    let req = CancelRequest::new(
        AttemptIdentity::new("s1", SessionEpoch::initial(), "t1", AttemptId::new()),
        CancelPolicy::Independent,
    )
    .with_clear_queue(true);
    rt.cancel(&req).unwrap();

    let received = handle.last_cancel().unwrap();
    assert!(received.clear_queue, "clear_queue 标志透传");
    assert_eq!(received.policy, CancelPolicy::Independent, "policy 透传");
}

// ─── join（等待会话自然终止，带 deadline） ────────────────────────────────────

#[tokio::test]
async fn join_forwards_deadline_result() {
    let handle = MockHandle::new(true, vec![]); // join 在 deadline 内结束
    let rt = Runtime::new();
    rt.register("s1", Arc::clone(&handle)).unwrap();

    assert!(
        rt.join("s1", Duration::from_secs(5)).await.unwrap(),
        "deadline 内结束返回 true"
    );
    assert_eq!(handle.call_sequence(), vec!["join"]);

    // 超时分支：返回 false（不自动 abort；abort 由销毁路径编排）
    let slow = MockHandle::new(false, vec![]);
    rt.register("s2", Arc::clone(&slow)).unwrap();
    assert!(
        !rt.join("s2", Duration::from_millis(1)).await.unwrap(),
        "超时返回 false"
    );

    // 未注册 session：类型化错误
    let err = rt
        .join("missing", Duration::from_secs(1))
        .await
        .unwrap_err();
    assert!(matches!(err, RuntimeError::UnknownSession(_)));
}

// ─── submit_input（消息/工具注入面：运行时输入收口） ────────────────────────────

#[test]
fn submit_input_forwards_to_handle() {
    let handle = MockHandle::new(true, vec![]);
    let rt = Runtime::new();
    rt.register("s1", Arc::clone(&handle)).unwrap();

    rt.submit_input("s1", MessageContent::text("hi")).unwrap();
    assert_eq!(
        handle.last_input(),
        Some(MessageContent::text("hi")),
        "运行时输入透传到句柄"
    );
    assert_eq!(handle.call_sequence(), vec!["submit_input"]);

    // 未注册 session：定位失败
    let err = rt
        .submit_input("missing", MessageContent::text("hi"))
        .unwrap_err();
    assert!(matches!(err, RuntimeError::UnknownSession(_)));

    // 句柄注入失败：anyhow 穿透包 context 为 SubmitFailed
    handle.submit_ok.store(false, Ordering::SeqCst);
    let err = rt
        .submit_input("s1", MessageContent::text("hi"))
        .unwrap_err();
    assert!(matches!(err, RuntimeError::SubmitFailed(s, _) if s == "s1"));
}
