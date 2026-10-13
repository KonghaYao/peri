//! 清理注册表的端口契约测试（契约 §3.2 删除协议 R1、§8）。
//!
//! 覆盖：delete 失败保留 claim/slot/quota 且原 owner 用同一 claim 重试成功、claim 排他
//! （同进程模拟另一 owner 看到 `None`）、容量归还、正常移交不删除、串行尝试与非计数归还。

use std::collections::{HashMap, HashSet};
use std::io;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};

use crate::memory::MemoryBudget;
use crate::spool::cleanup::CleanupRegistry;
use crate::spool::{
    CleanupRegistration, ObjectClaim, ObjectId, ObjectKind, ObjectMeta, PageCursor,
    QuotaReservation, RouteFingerprint, SpoolError, SpoolObject, SpoolPage, SpoolReader,
    SpoolResult, SpoolState, SpoolStore, SpoolUsage, TemporaryObject,
};

fn oid(seed: u8) -> ObjectId {
    ObjectId::from_bytes([seed; 16])
}

fn meta(id: ObjectId) -> ObjectMeta {
    ObjectMeta {
        version: 1,
        id,
        kind: ObjectKind::Input,
        route: RouteFingerprint::from_bytes([7; 32]),
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

/// 进程内 fake：记录对象、claim 排他、删除调用与可注入失败。
struct FakeStore {
    me: OnceLock<Weak<FakeStore>>,
    objects: Mutex<HashMap<ObjectId, ObjectMeta>>,
    held: Mutex<HashSet<ObjectId>>,
    issued: AtomicUsize,
    delete_calls: AtomicUsize,
    fail_delete: AtomicBool,
}

impl crate::spool::sealed::Sealed for FakeStore {}

impl FakeStore {
    fn new(ids: &[ObjectId]) -> Arc<Self> {
        let store = Arc::new(Self {
            me: OnceLock::new(),
            objects: Mutex::new(ids.iter().map(|id| (*id, meta(*id))).collect()),
            held: Mutex::new(HashSet::new()),
            issued: AtomicUsize::new(0),
            delete_calls: AtomicUsize::new(0),
            fail_delete: AtomicBool::new(false),
        });
        store.me.set(Arc::downgrade(&store)).ok();
        store
    }

    fn is_held(&self, id: ObjectId) -> bool {
        self.held.lock().unwrap().contains(&id)
    }

    fn exists(&self, id: ObjectId) -> bool {
        self.objects.lock().unwrap().contains_key(&id)
    }

    /// 已发出的 claim 个数：重试必须复用同一个 claim，不得重新领取。
    fn claims_issued(&self) -> usize {
        self.issued.load(Ordering::Relaxed)
    }

    fn delete_calls(&self) -> usize {
        self.delete_calls.load(Ordering::Relaxed)
    }

    fn fail_delete(&self, fail: bool) {
        self.fail_delete.store(fail, Ordering::Relaxed);
    }

    /// 幂等删除：注入失败时不改动任何状态（含 claim 持锁）。
    fn delete_object(&self, id: ObjectId) -> SpoolResult<()> {
        self.delete_calls.fetch_add(1, Ordering::Relaxed);
        if self.fail_delete.load(Ordering::Relaxed) {
            return Err(SpoolError::Io {
                operation: "delete",
                source: io::Error::other("injected delete failure"),
            });
        }
        self.objects.lock().unwrap().remove(&id);
        self.held.lock().unwrap().remove(&id);
        Ok(())
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
        self.objects
            .lock()
            .unwrap()
            .get(&id)
            .cloned()
            .ok_or(SpoolError::NotFound)
    }

    fn open(&self, _id: ObjectId) -> SpoolResult<SpoolReader> {
        Err(SpoolError::Unavailable)
    }

    fn list(&self, _after: Option<PageCursor>, _limit: usize) -> SpoolResult<SpoolPage> {
        Err(SpoolError::Unavailable)
    }

    fn claim(&self, id: ObjectId) -> SpoolResult<Option<Box<dyn ObjectClaim>>> {
        let mut held = self.held.lock().unwrap();
        if held.contains(&id) {
            return Ok(None);
        }
        if !self.objects.lock().unwrap().contains_key(&id) {
            return Err(SpoolError::NotFound);
        }
        held.insert(id);
        self.issued.fetch_add(1, Ordering::Relaxed);
        Ok(Some(Box::new(FakeClaim {
            store: self.me.get().expect("store initialized").clone(),
            meta: meta(id),
        })))
    }

    fn delete(&self, id: ObjectId) -> SpoolResult<()> {
        self.delete_object(id)
    }

    fn usage(&self) -> SpoolResult<SpoolUsage> {
        Err(SpoolError::Unavailable)
    }
}

struct FakeClaim {
    store: Weak<FakeStore>,
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
        self.store
            .upgrade()
            .expect("store alive")
            .delete_object(self.meta.id)
    }
}

/// 走完注册→绑定→claim→构造对象，返回仍持 claim 的对象句柄。
fn publish(
    store: &Arc<FakeStore>,
    registry: &CleanupRegistry,
    id: ObjectId,
    quota: u64,
) -> SpoolObject {
    let registration = registry.try_register().expect("slot available");
    registration.slot.bind(id, quota).expect("bind");
    let claim = store
        .claim(id)
        .expect("claim call")
        .expect("object available");
    SpoolObject::from_claim(dyn_store(store), claim, registration).expect("from_claim")
}

/// 显式 unsize：`Arc::clone` 的返回类型不能就地强转成 trait object。
fn dyn_store(store: &Arc<FakeStore>) -> Arc<dyn SpoolStore> {
    let clone = Arc::clone(store);
    clone
}

fn registry(store: &Arc<FakeStore>, slots: usize) -> CleanupRegistry {
    CleanupRegistry::new(
        dyn_store(store),
        MemoryBudget::new(64 * 1024).expect("budget"),
        slots,
    )
}

#[test]
fn other_owner_sees_no_claim_while_cleanup_slot_holds_it() {
    let store = FakeStore::new(&[oid(1)]);
    let registry = registry(&store, 4);
    let object = publish(&store, &registry, oid(1), 4096);
    drop(object);

    // 槽持有同一个 claim：另一 owner 只能看到 None，且配额/待清理登记保留。
    assert!(store.claim(oid(1)).expect("claim call").is_none());
    assert!(store.is_held(oid(1)));
    assert_eq!(store.claims_issued(), 1);
    assert_eq!(registry.pending(), (1, 4096));
    assert_eq!(registry.disarmed(), (0, 0));
}

#[tokio::test]
async fn failed_delete_keeps_claim_and_slot_until_retry_succeeds() {
    let store = FakeStore::new(&[oid(2)]);
    let mut registry = registry(&store, 4);
    drop(publish(&store, &registry, oid(2), 8192));
    assert_eq!(registry.pending(), (1, 8192));

    store.fail_delete(true);
    let error = registry.drain().await.expect_err("delete fails");
    assert!(matches!(error, SpoolError::Io { .. }));
    // 失败不解锁：另一 owner 仍 claim=None，槽/配额/登记全部保留。
    assert_eq!(registry.pending(), (1, 8192));
    assert_eq!(registry.disarmed(), (0, 0));
    assert!(store.claim(oid(2)).expect("claim call").is_none());
    assert!(store.exists(oid(2)));
    assert_eq!(store.claims_issued(), 1);

    // 原 owner 用同一 claim 重试：不重新领取，成功后恰好回收一次。
    store.fail_delete(false);
    registry.drain().await.expect("retry succeeds");
    assert_eq!(store.delete_calls(), 2);
    assert_eq!(store.claims_issued(), 1);
    assert!(!store.exists(oid(2)));
    assert!(!store.is_held(oid(2)));
    assert_eq!(registry.pending(), (0, 0));
    assert_eq!(registry.disarmed(), (1, 0));
}

#[tokio::test]
async fn drain_attempts_every_pending_slot_and_reports_first_error() {
    let store = FakeStore::new(&[oid(3), oid(4)]);
    let mut registry = registry(&store, 4);
    drop(publish(&store, &registry, oid(3), 1024));
    drop(publish(&store, &registry, oid(4), 2048));
    assert_eq!(registry.pending(), (2, 3072));

    // 注入失败：本轮仍逐个尝试全部待清理槽，失败的登记与 claim 全部保留。
    store.fail_delete(true);
    assert!(registry.drain().await.is_err());
    assert_eq!(store.delete_calls(), 2);
    assert_eq!(registry.pending(), (2, 3072));
    assert_eq!(registry.disarmed(), (0, 0));
    assert!(store.is_held(oid(3)) && store.is_held(oid(4)));

    store.fail_delete(false);
    registry.drain().await.expect("retry succeeds");
    assert_eq!(store.delete_calls(), 4);
    assert_eq!(registry.pending(), (0, 0));
    assert_eq!(registry.disarmed(), (2, 0));
    assert!(!store.exists(oid(3)) && !store.exists(oid(4)));
}

#[tokio::test]
async fn confirmed_delete_returns_slot_capacity() {
    let store = FakeStore::new(&[oid(5), oid(6)]);
    let mut registry = registry(&store, 1);
    drop(publish(&store, &registry, oid(5), 512));
    assert!(matches!(registry.try_register(), Err(SpoolError::Busy)));

    registry.drain().await.expect("delete succeeds");
    let registration = registry.try_register().expect("capacity returned");
    registration.slot.bind(oid(6), 256).expect("bind");
    drop(registration);
    assert_eq!(registry.pending(), (0, 0));
}

#[tokio::test]
async fn handoff_disarms_once_without_deleting() {
    let store = FakeStore::new(&[oid(7)]);
    let mut registry = registry(&store, 2);
    let registration = registry.try_register().expect("slot available");
    registration.slot.bind(oid(7), 777).expect("bind");
    registration.disarm_after_handoff();

    assert_eq!(registry.pending(), (0, 0));
    assert_eq!(registry.disarmed(), (0, 1));
    registry.drain().await.expect("nothing pending");
    assert_eq!(store.delete_calls(), 0);
}

#[test]
fn never_bound_registration_releases_capacity_without_counting() {
    let store = FakeStore::new(&[]);
    let registry = registry(&store, 1);
    let registration = registry.try_register().expect("slot available");
    assert!(matches!(registry.try_register(), Err(SpoolError::Busy)));

    drop(registration);
    assert_eq!(registry.pending(), (0, 0));
    // 没有可删产物，不算「确认删除后回收」。
    assert_eq!(registry.disarmed(), (0, 0));
    assert!(registry.try_register().is_ok());
}
