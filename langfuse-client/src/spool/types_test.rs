//! `SpoolObject` 受控构造与交付句柄的端口契约测试（契约 §3.2 构造协议、§8）。
//!
//! 覆盖：`from_claim` 的同 store/id/预算绑定校验与失败归槽、`take_claim`/`return_claim`
//! 排他性、Drop 只登记待清理而不做 IO、元数据只读视图与 `Send + Sync`。

use std::collections::HashMap;
use std::io;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

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

fn object_meta(id: ObjectId) -> ObjectMeta {
    ObjectMeta {
        version: 1,
        id,
        kind: ObjectKind::Input,
        route: RouteFingerprint::from_bytes([9; 32]),
        event_count: 2,
        encoding: crate::config::Compression::Gzip,
        raw_len: 32,
        stored_len: 48,
        sha256: [1; 32],
        accepted_at_ms: 10,
        expires_at_ms: 20,
        attempts: 0,
        next_attempt_at_ms: None,
        state: SpoolState::Ready,
    }
}

struct FakeStore {
    objects: Mutex<HashMap<ObjectId, ObjectMeta>>,
    held: Mutex<HashMap<ObjectId, ()>>,
    delete_calls: AtomicUsize,
}

impl crate::spool::sealed::Sealed for FakeStore {}

impl FakeStore {
    fn new(ids: &[ObjectId]) -> Arc<Self> {
        Arc::new(Self {
            objects: Mutex::new(ids.iter().map(|id| (*id, object_meta(*id))).collect()),
            held: Mutex::new(HashMap::new()),
            delete_calls: AtomicUsize::new(0),
        })
    }

    fn is_held(&self, id: ObjectId) -> bool {
        self.held.lock().unwrap().contains_key(&id)
    }

    fn delete_calls(&self) -> usize {
        self.delete_calls.load(Ordering::Relaxed)
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
        if held.contains_key(&id) {
            return Ok(None);
        }
        let meta = self
            .objects
            .lock()
            .unwrap()
            .get(&id)
            .cloned()
            .ok_or(SpoolError::NotFound)?;
        held.insert(id, ());
        Ok(Some(Box::new(FakeClaim {
            meta,
            delete_calls: Arc::new(AtomicUsize::new(0)),
        })))
    }

    fn delete(&self, id: ObjectId) -> SpoolResult<()> {
        self.delete_calls.fetch_add(1, Ordering::Relaxed);
        self.objects.lock().unwrap().remove(&id);
        self.held.lock().unwrap().remove(&id);
        Ok(())
    }

    fn usage(&self) -> SpoolResult<SpoolUsage> {
        Err(SpoolError::Unavailable)
    }
}

struct FakeClaim {
    meta: ObjectMeta,
    delete_calls: Arc<AtomicUsize>,
}

impl ObjectClaim for FakeClaim {
    fn meta(&self) -> &ObjectMeta {
        &self.meta
    }

    fn open(&self) -> SpoolResult<SpoolReader> {
        Ok(Box::new(io::empty()))
    }

    fn checkpoint(&mut self, meta: ObjectMeta) -> SpoolResult<()> {
        self.meta = meta;
        Ok(())
    }

    fn delete(&mut self) -> SpoolResult<()> {
        self.delete_calls.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
}

/// 显式 unsize：`Arc::clone` 的返回类型不能就地强转成 trait object。
fn dyn_store(store: &Arc<FakeStore>) -> Arc<dyn SpoolStore> {
    let clone = Arc::clone(store);
    clone
}

fn registry(store: &Arc<FakeStore>) -> CleanupRegistry {
    CleanupRegistry::new(
        dyn_store(store),
        MemoryBudget::new(64 * 1024).expect("budget"),
        4,
    )
}

fn take(id: ObjectId, store: &Arc<FakeStore>, slot: &CleanupRegistry) -> SpoolObject {
    let registration = slot.try_register().expect("slot");
    registration.slot.bind(id, 4096).expect("bind");
    let claim = store.claim(id).expect("claim call").expect("available");
    SpoolObject::from_claim(dyn_store(store), claim, registration).expect("from_claim")
}

#[test]
fn from_claim_rejects_mismatched_id_and_returns_claim_to_slot() {
    let store = FakeStore::new(&[oid(1), oid(2)]);
    let slot = registry(&store);
    let registration = slot.try_register().expect("slot");
    registration.slot.bind(oid(1), 1024).expect("bind");
    // claim 属于另一个对象：不得裸放字段构造，claim 必须回原槽等待清理。
    let claim = store.claim(oid(2)).expect("claim call").expect("available");
    let rejected = match SpoolObject::from_claim(dyn_store(&store), claim, registration) {
        Ok(_) => panic!("mismatched id must be rejected"),
        Err(error) => error,
    };
    assert!(matches!(rejected, SpoolError::InvalidMetadata));
    assert_eq!(slot.pending(), (1, 1024));
    assert!(store.is_held(oid(2)));
}

#[test]
fn from_claim_rejects_store_from_another_deployment() {
    let store = FakeStore::new(&[oid(3)]);
    let foreign = FakeStore::new(&[oid(3)]);
    let slot = registry(&store);
    let registration = slot.try_register().expect("slot");
    registration.slot.bind(oid(3), 2048).expect("bind");
    let claim = foreign
        .claim(oid(3))
        .expect("claim call")
        .expect("available");
    let rejected = match SpoolObject::from_claim(dyn_store(&foreign), claim, registration) {
        Ok(_) => panic!("foreign store must be rejected"),
        Err(error) => error,
    };
    assert!(matches!(rejected, SpoolError::InvalidMetadata));
    assert_eq!(slot.pending(), (1, 2048));
}

#[test]
fn take_claim_is_exclusive_and_return_is_checked() {
    let store = FakeStore::new(&[oid(4)]);
    let slot = registry(&store);
    let object = take(oid(4), &store, &slot);
    assert_eq!(object.meta().id, oid(4));
    assert_eq!(object.meta().stored_len, 48);

    let claim = object.take_claim().expect("claim in object");
    assert!(object.take_claim().is_none());
    object.return_claim(claim).expect("first return");
    let claim = object.take_claim().expect("claim in object");
    object.return_claim(claim).expect("second return");
}

#[test]
fn drop_registers_pending_cleanup_without_io() {
    let store = FakeStore::new(&[oid(5)]);
    let slot = registry(&store);
    let object = take(oid(5), &store, &slot);
    assert_eq!(store.delete_calls(), 0);

    drop(object);
    // 只登记待清理：不做 IO、不释放 OS 锁，另一 owner 仍看到 claim=None。
    assert_eq!(store.delete_calls(), 0);
    assert_eq!(slot.pending(), (1, 4096));
    assert!(store.is_held(oid(5)));
    assert!(store.claim(oid(5)).expect("claim call").is_none());
}

#[test]
fn handles_are_send_and_sync() {
    fn assert_shareable<T: Send + Sync>() {}
    assert_shareable::<SpoolObject>();
    assert_shareable::<CleanupRegistration>();
    assert_shareable::<ObjectMeta>();
}
