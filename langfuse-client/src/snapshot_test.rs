//! 快照来源适配、事件准入与票据取消的契约测试（契约 §3.3、§8 用例 4/5）。
//!
//! 覆盖：`snapshot_value` 零拷贝借用且可重复写出、紧凑 JSON 字节与 `serde_json` 一致、
//! 双份内容冲突、每种来源的异预算拒收、失败路径按原 owner 释放（permit/claim/票据）、
//! 事件原文 permit 的计量、票据 Drop 置取消位并唤醒等待者。

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use serde_json::json;

use crate::error::LangfuseError;
use crate::memory::MemoryBudget;
use crate::payload::{BudgetedBytes, InputSnapshot};
use crate::snapshot::{
    snapshot_value, InputSource, SnapshotCell, SnapshotState, SnapshotTicket, TelemetryEvent,
};
use crate::spool::cleanup::CleanupRegistry;
use crate::spool::{
    CleanupRegistration, ObjectClaim, ObjectId, ObjectKind, ObjectMeta, PageCursor,
    QuotaReservation, RouteFingerprint, SpoolError, SpoolObject, SpoolPage, SpoolReader,
    SpoolResult, SpoolState, SpoolStore, SpoolUsage, TemporaryObject,
};
use crate::types::{EventBody, IngestionEvent};

fn budget(limit: usize) -> Arc<MemoryBudget> {
    MemoryBudget::new(limit).expect("budget")
}

fn event(input: Option<serde_json::Value>, output: Option<serde_json::Value>) -> IngestionEvent {
    IngestionEvent::EventCreate {
        id: "evt-1".into(),
        timestamp: "2026-01-01T00:00:00Z".into(),
        body: EventBody {
            id: None,
            trace_id: None,
            name: Some("test".into()),
            start_time: None,
            input,
            output,
            metadata: None,
            level: None,
            status_message: None,
            parent_observation_id: None,
            version: None,
            environment: None,
        },
        metadata: None,
    }
}

fn inline(budget: &Arc<MemoryBudget>, body: &'static [u8]) -> InputSnapshot {
    let permit = budget.try_reserve(body.len()).expect("permit");
    InputSnapshot::Inline(BudgetedBytes::new(Bytes::from_static(body), permit).expect("bytes"))
}

/// 准入拒绝：`TelemetryEvent` 不实现 `Debug`，不能用 `expect_err`。
fn reject(result: Result<TelemetryEvent, LangfuseError>) -> LangfuseError {
    match result {
        Ok(_) => panic!("event must be rejected"),
        Err(error) => error,
    }
}

fn cell(budget: &Arc<MemoryBudget>, prepare_seq: u64) -> Arc<SnapshotCell> {
    Arc::new(SnapshotCell {
        budget: Arc::clone(budget),
        prepare_seq,
        state: Mutex::new(SnapshotState::Preparing),
        cancelled: AtomicBool::new(false),
        changed: tokio::sync::Notify::new(),
    })
}

/// 只用于构造 `SpoolObject` 的最小 fake：对象可 claim 一次。
struct FakeStore {
    id: ObjectId,
    held: AtomicBool,
}

impl crate::spool::sealed::Sealed for FakeStore {}

impl FakeStore {
    fn new(id: ObjectId) -> Arc<Self> {
        Arc::new(Self {
            id,
            held: AtomicBool::new(false),
        })
    }

    fn meta(&self) -> ObjectMeta {
        ObjectMeta {
            version: 1,
            id: self.id,
            kind: ObjectKind::Input,
            route: RouteFingerprint::from_bytes([3; 32]),
            event_count: 1,
            encoding: crate::config::Compression::None,
            raw_len: 8,
            stored_len: 8,
            sha256: [0; 32],
            accepted_at_ms: 0,
            expires_at_ms: 60_000,
            attempts: 0,
            next_attempt_at_ms: None,
            state: SpoolState::Ready,
        }
    }
}

impl SpoolStore for FakeStore {
    fn reserve(&self, bytes: u64) -> SpoolResult<Box<dyn QuotaReservation>> {
        struct Reserved(u64);
        impl QuotaReservation for Reserved {
            fn bytes(&self) -> u64 {
                self.0
            }
        }
        Ok(Box::new(Reserved(bytes)))
    }

    fn create(
        &self,
        _quota: Box<dyn QuotaReservation>,
        _kind: ObjectKind,
        _cleanup: CleanupRegistration,
    ) -> SpoolResult<Box<dyn TemporaryObject>> {
        Err(SpoolError::Unavailable)
    }

    fn inspect(&self, id: ObjectId) -> SpoolResult<ObjectMeta> {
        (id == self.id)
            .then(|| self.meta())
            .ok_or(SpoolError::NotFound)
    }

    fn open(&self, _id: ObjectId) -> SpoolResult<SpoolReader> {
        Err(SpoolError::Unavailable)
    }

    fn list(&self, _after: Option<PageCursor>, _limit: usize) -> SpoolResult<SpoolPage> {
        Err(SpoolError::Unavailable)
    }

    fn claim(&self, id: ObjectId) -> SpoolResult<Option<Box<dyn ObjectClaim>>> {
        if id != self.id {
            return Err(SpoolError::NotFound);
        }
        if self.held.swap(true, Ordering::AcqRel) {
            return Ok(None);
        }
        Ok(Some(Box::new(FakeClaim { meta: self.meta() })))
    }

    fn delete(&self, id: ObjectId) -> SpoolResult<()> {
        (id == self.id).then_some(()).ok_or(SpoolError::NotFound)
    }

    fn usage(&self) -> SpoolResult<SpoolUsage> {
        Err(SpoolError::Unavailable)
    }
}

struct FakeClaim {
    meta: ObjectMeta,
}

impl ObjectClaim for FakeClaim {
    fn meta(&self) -> &ObjectMeta {
        &self.meta
    }

    fn open(&self) -> SpoolResult<SpoolReader> {
        Ok(Box::new(io::empty()))
    }

    fn checkpoint(&mut self, _meta: ObjectMeta) -> SpoolResult<()> {
        Ok(())
    }

    fn delete(&mut self) -> SpoolResult<()> {
        Ok(())
    }
}

/// 显式指定被克隆的具体类型：返回位置再把它 unsize 成 trait object。
fn dyn_store(store: &Arc<FakeStore>) -> Arc<dyn SpoolStore> {
    Arc::<FakeStore>::clone(store)
}

/// 走完注册→绑定→claim，返回绑定同一预算与 store 的暂存对象。
fn spooled(store: &Arc<FakeStore>, id: ObjectId, budget: &Arc<MemoryBudget>) -> SpoolObject {
    let registry = CleanupRegistry::new(dyn_store(store), Arc::clone(budget), 4);
    let registration = registry.try_register().expect("slot");
    registration.slot.bind(id, 512).expect("bind");
    let claim = store.claim(id).expect("claim call").expect("available");
    SpoolObject::from_claim(dyn_store(store), claim, registration).expect("from_claim")
}

#[test]
fn snapshot_value_borrows_arc_and_writes_compact_json() {
    let value = Arc::new(json!({"key": "value", "list": [1, 2, 3]}));
    let source = snapshot_value(&value);
    assert_eq!(Arc::strong_count(&value), 2, "只克隆 Arc，不重建 JSON 树");

    let mut first = Vec::new();
    source.write_json(&mut first).expect("write json");
    assert_eq!(first, serde_json::to_vec(&*value).expect("reference bytes"));
    let mut second = Vec::new();
    source.write_json(&mut second).expect("second write");
    assert_eq!(first, second, "允许被多次调用");

    let big = Arc::new(json!({ "blob": "x".repeat(4096) }));
    let source = snapshot_value(&big);
    assert!(source.retained_bytes().expect("accounting") >= 4096);
}

#[test]
fn try_new_rejects_body_field_conflicts() {
    let budget = budget(1024 * 1024);
    let error = reject(TelemetryEvent::try_new(
        event(Some(json!({"inline": true})), None),
        Some(InputSource::Ready(inline(&budget, b"{}"))),
        None,
        &budget,
    ));
    assert!(matches!(error, LangfuseError::Config(_)));

    let error = reject(TelemetryEvent::try_new(
        event(None, Some(json!({"inline": true}))),
        None,
        Some(InputSource::Ready(inline(&budget, b"{}"))),
        &budget,
    ));
    assert!(matches!(error, LangfuseError::Config(_)));
}

#[test]
fn try_new_rejects_every_foreign_budget_source() {
    let owner = budget(1024 * 1024);
    let foreign = budget(1024 * 1024);
    let store = FakeStore::new(ObjectId::from_bytes([5; 16]));
    let id = store.id;

    // 同金额不同 Arc 不算同一预算。
    let error = reject(TelemetryEvent::try_new(
        event(None, None),
        Some(InputSource::Ready(inline(&owner, b"{\"a\":1}"))),
        None,
        &foreign,
    ));
    assert!(matches!(error, LangfuseError::BudgetMismatch));

    let error = reject(TelemetryEvent::try_new(
        event(None, None),
        Some(InputSource::Ready(InputSnapshot::Spooled(spooled(
            &store, id, &owner,
        )))),
        None,
        &foreign,
    ));
    assert!(matches!(error, LangfuseError::BudgetMismatch));

    let error = reject(TelemetryEvent::try_new(
        event(None, None),
        Some(InputSource::Preparing(SnapshotTicket {
            cell: cell(&owner, 1),
        })),
        None,
        &foreign,
    ));
    assert!(matches!(error, LangfuseError::BudgetMismatch));
}

#[test]
fn try_new_reserves_event_permit_and_releases_sources_on_failure() {
    let budget = budget(1024 * 1024);
    let admitted = event(None, None);
    let expected = serde_json::to_vec(&admitted).expect("reference size").len();

    let accepted = TelemetryEvent::try_new(admitted, None, None, &budget).expect("admitted");
    assert_eq!(budget.used(), expected);
    drop(accepted);
    assert_eq!(budget.used(), 0);

    // 字段冲突：Inline permit 已预约，失败后必须回到原 owner 并归还。
    let error = reject(TelemetryEvent::try_new(
        event(Some(json!({"inline": true})), None),
        Some(InputSource::Ready(inline(&budget, b"{\"frozen\":true}"))),
        None,
        &budget,
    ));
    assert!(matches!(error, LangfuseError::Config(_)));
    assert_eq!(budget.used(), 0);
}

#[test]
fn try_new_rejects_insufficient_budget() {
    let budget = budget(8);
    let error = reject(TelemetryEvent::try_new(
        event(None, None),
        None,
        None,
        &budget,
    ));
    match error {
        LangfuseError::MemoryBudgetExceeded { requested, .. } => assert!(requested > 0),
        other => panic!("unexpected error: {other}"),
    }
}

#[test]
fn accepted_sources_stay_owned_by_the_event() {
    let budget = budget(1024 * 1024);
    let frozen = b"{\"frozen\":true}";
    let event = TelemetryEvent::try_new(
        event(None, None),
        Some(InputSource::Ready(inline(&budget, frozen))),
        None,
        &budget,
    )
    .expect("admitted");
    assert_eq!(
        budget.used(),
        serde_json::to_vec(&event.event).expect("size").len() + frozen.len()
    );
    drop(event);
    assert_eq!(budget.used(), 0);
}

#[tokio::test]
async fn ticket_drop_cancels_and_wakes_waiter() {
    let budget = budget(4096);
    let cell = cell(&budget, 7);
    let waiter = tokio::spawn({
        let cell = Arc::clone(&cell);
        async move { cell.changed.notified().await }
    });
    tokio::task::yield_now().await;

    drop(SnapshotTicket {
        cell: Arc::clone(&cell),
    });
    tokio::time::timeout(std::time::Duration::from_secs(1), waiter)
        .await
        .expect("ticket drop wakes the worker")
        .expect("waiter joined");
    assert!(cell.cancelled.load(Ordering::Acquire));
    assert_eq!(budget.used(), 0);
}
