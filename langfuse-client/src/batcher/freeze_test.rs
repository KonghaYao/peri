//! 冻结快照的阈值判定、真实磁盘落盘与降级语义回归（契约 §3.3、§4、§6）。
//!
//! 覆盖：阈值 -1 / 等于 / +1 的三点边界（`>=` 落盘）；真实 native 适配器 + 临时目录下的
//! **落盘正文逐字节保真**（不只内存 fake）；磁盘失败只允许一次非等待内存回退，额度不足即
//! 拒收；未开启暂存时不落盘、超 allowance 显式超限。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use super::*;
use crate::config::Compression;
use crate::memory::MemoryBudget;
use crate::spool::cleanup::CleanupRegistry;
use crate::spool::native::NativeSpool;
use crate::spool::{
    ObjectClaim, ObjectId, PageCursor, QuotaReservation, SpoolPage, SpoolReader, SpoolResult,
    SpoolUsage, TemporaryObject,
};

/// 写入固定字节的来源；`entered` 记录被重跑的次数（阈值跨越与降级路径都要重跑一次）。
struct ExactSource {
    bytes: Vec<u8>,
    entered: Arc<AtomicUsize>,
}

impl SnapshotSource for ExactSource {
    fn retained_bytes(&self) -> Result<usize, LangfuseError> {
        Ok(self.bytes.len())
    }

    fn write_json(&self, out: &mut dyn std::io::Write) -> Result<(), LangfuseError> {
        self.entered.fetch_add(1, Ordering::SeqCst);
        out.write_all(&self.bytes)
            .map_err(|error| LangfuseError::Config(format!("source write failed: {error}")))
    }
}

fn exact_source(bytes: Vec<u8>) -> (Arc<ExactSource>, Arc<AtomicUsize>) {
    let entered = Arc::new(AtomicUsize::new(0));
    let source = Arc::new(ExactSource {
        bytes,
        entered: Arc::clone(&entered),
    });
    (source, entered)
}

/// 显式指定被克隆的具体类型：返回位置再把它 unsize 成 trait object。
fn dyn_store(store: &Arc<NativeSpool>) -> Arc<dyn SpoolStore> {
    Arc::<NativeSpool>::clone(store)
}

fn budget(limit: usize) -> Arc<MemoryBudget> {
    MemoryBudget::new(limit).expect("memory budget")
}

fn context(
    store: Arc<dyn SpoolStore>,
    budget: &Arc<MemoryBudget>,
    threshold: usize,
    max_bytes: usize,
) -> InputSpool {
    InputSpool {
        store,
        budget: Arc::clone(budget),
        threshold,
        max_bytes,
        route: route(),
        accepted_at_ms: 1_000,
        expires_at_ms: 61_000,
    }
}

/// route 摘要只在运行时构造：`from_bytes` 不是常量函数。
fn route() -> RouteFingerprint {
    RouteFingerprint::from_bytes([7; 32])
}

const QUOTA_BYTES: u64 = 4 * 1024 * 1024;

/// 真实临时根上的 native 暂存：正文回读与元数据都必须与冻结内容一致。
fn native_spool(root: &std::path::Path) -> Arc<NativeSpool> {
    Arc::new(NativeSpool::open_at(root.to_path_buf(), route(), QUOTA_BYTES).expect("native spool"))
}

fn read_object(object: &SpoolObject) -> Vec<u8> {
    let claim: Box<dyn ObjectClaim> = object.take_claim().expect("frozen object claim");
    let mut reader = claim.open().expect("object body");
    let mut body = Vec::new();
    std::io::Read::read_to_end(&mut reader, &mut body).expect("read object body");
    drop(reader);
    object.return_claim(claim).expect("claim returned");
    body
}

#[test]
fn threshold_boundary_decides_between_inline_and_disk() {
    const THRESHOLD: usize = 64;
    const MAX_BYTES: usize = 4 * 1024;
    let root = tempfile::tempdir().expect("temporary root");
    let store = native_spool(root.path());
    let budget = budget(MAX_BYTES);
    let registry = CleanupRegistry::new(dyn_store(&store), Arc::clone(&budget), 4);

    // 阈值 -1：留在内存。
    let allowance = budget.try_reserve(THRESHOLD).expect("allowance");
    let (source, entered) = exact_source(vec![b'a'; THRESHOLD - 1]);
    let snapshot = freeze_snapshot(
        &(Arc::clone(&source) as Arc<dyn SnapshotSource>),
        allowance,
        Some((
            context(dyn_store(&store), &budget, THRESHOLD, MAX_BYTES),
            registry.try_register().expect("slot"),
        )),
    )
    .expect("inline snapshot");
    assert!(
        matches!(snapshot, InputSnapshot::Inline(_)),
        "must stay inline"
    );
    assert_eq!(
        entered.load(Ordering::SeqCst),
        1,
        "inline runs the source once"
    );
    drop(snapshot);
    assert_eq!(
        store.usage().expect("usage").objects,
        0,
        "no object written"
    );

    // 正好等于阈值与 +1：都落盘，正文摘要与长度按实测记录。
    for len in [THRESHOLD, THRESHOLD + 1] {
        let allowance = budget.try_reserve(THRESHOLD).expect("allowance");
        let payload: Vec<u8> = (0..len).map(|index| b'a' + (index % 26) as u8).collect();
        let (source, entered) = exact_source(payload.clone());
        let snapshot = freeze_snapshot(
            &(Arc::clone(&source) as Arc<dyn SnapshotSource>),
            allowance,
            Some((
                context(dyn_store(&store), &budget, THRESHOLD, MAX_BYTES),
                registry.try_register().expect("slot"),
            )),
        )
        .expect("spooled snapshot");
        let InputSnapshot::Spooled(object) = snapshot else {
            panic!("{len}-byte snapshot must be spooled");
        };
        // 落盘路径要先放弃前缀再重跑一次来源：内容必须逐字节等于冻结字节。
        assert_eq!(entered.load(Ordering::SeqCst), 2);
        assert_eq!(
            read_object(&object),
            payload,
            "disk body must be byte-identical"
        );
        let meta = object.meta();
        assert_eq!(meta.kind, ObjectKind::Input);
        assert_eq!(meta.encoding, Compression::None);
        assert_eq!(meta.event_count, 0);
        assert_eq!(meta.raw_len, len as u64);
        assert_eq!(meta.stored_len, len as u64);
        assert_eq!(meta.route, route());
        let digest: [u8; 32] = sha2::Sha256::digest(&payload).into();
        assert_eq!(meta.sha256, digest, "descriptor digest covers stored bytes");
        // 前缀额度在落盘后归还：预算里不应残留冻结 allowance。
        assert!(budget.used() < THRESHOLD, "prefix allowance returned");
        drop(object);
    }
}

#[test]
fn disabled_spool_never_touches_disk_and_rejects_over_allowance() {
    let budget = budget(1024);
    let allowance = budget.try_reserve(16).expect("allowance");
    let (source, _) = exact_source(vec![b'a'; 32]);
    let error = freeze_snapshot(
        &(Arc::clone(&source) as Arc<dyn SnapshotSource>),
        allowance,
        None,
    )
    .err()
    .expect("oversized frozen snapshot without spool");
    assert!(matches!(
        error,
        LangfuseError::PayloadTooLarge { limit_bytes: 16 }
    ));
}

/// 只实现「先失败」的那一步：reserve 或 create，其余端口方法不被本用例使用。
struct FailingStore {
    fail_create: bool,
    calls: AtomicUsize,
}

impl crate::spool::sealed::Sealed for FailingStore {}

impl SpoolStore for FailingStore {
    fn reserve(&self, bytes: u64) -> SpoolResult<Box<dyn QuotaReservation>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if !self.fail_create {
            struct Reserved(u64);
            impl QuotaReservation for Reserved {
                fn bytes(&self) -> u64 {
                    self.0
                }
            }
            return Ok(Box::new(Reserved(bytes)));
        }
        Err(SpoolError::Unavailable)
    }

    fn create(
        &self,
        _quota: Box<dyn QuotaReservation>,
        _kind: ObjectKind,
        _cleanup: CleanupRegistration,
    ) -> SpoolResult<Box<dyn TemporaryObject>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Err(SpoolError::Unavailable)
    }

    fn inspect(&self, _id: ObjectId) -> SpoolResult<ObjectMeta> {
        Err(SpoolError::Unavailable)
    }

    fn open(&self, _id: ObjectId) -> SpoolResult<SpoolReader> {
        Err(SpoolError::Unavailable)
    }

    fn list(&self, _after: Option<PageCursor>, _limit: usize) -> SpoolResult<SpoolPage> {
        Err(SpoolError::Unavailable)
    }

    fn claim(&self, _id: ObjectId) -> SpoolResult<Option<Box<dyn ObjectClaim>>> {
        Err(SpoolError::Unavailable)
    }

    fn delete(&self, _id: ObjectId) -> SpoolResult<()> {
        Err(SpoolError::Unavailable)
    }

    fn usage(&self) -> SpoolResult<SpoolUsage> {
        Err(SpoolError::Unavailable)
    }
}

fn failing_store(fail_create: bool) -> Arc<FailingStore> {
    Arc::new(FailingStore {
        fail_create,
        calls: AtomicUsize::new(0),
    })
}

fn failing_context(
    store: &Arc<FailingStore>,
    budget: &Arc<MemoryBudget>,
) -> (InputSpool, CleanupRegistration) {
    let dyn_store: Arc<dyn SpoolStore> = Arc::clone(store) as Arc<dyn SpoolStore>;
    let registry = CleanupRegistry::new(Arc::clone(&dyn_store), Arc::clone(budget), 4);
    (
        context(dyn_store, budget, 32, 4096),
        registry.try_register().expect("slot"),
    )
}

/// 磁盘失败：只做一次非等待内存回退，回退成功后内容仍逐字节完整。
#[test]
fn disk_failure_falls_back_to_memory_once() {
    let budget = budget(64 * 1024);
    for fail_create in [true, false] {
        let store = failing_store(fail_create);
        let allowance = budget.try_reserve(32).expect("allowance");
        let payload: Vec<u8> = (0..512).map(|index| (index % 251) as u8).collect();
        let (source, entered) = exact_source(payload.clone());
        let snapshot = freeze_snapshot(
            &(Arc::clone(&source) as Arc<dyn SnapshotSource>),
            allowance,
            Some(failing_context(&store, &budget)),
        )
        .expect("memory fallback");
        let InputSnapshot::Inline(bytes) = snapshot else {
            panic!("fallback must stay in memory");
        };
        assert_eq!(bytes.bytes().as_ref(), payload.as_slice());
        // 一次磁盘尝试最多两次端口调用（reserve + create）；回退不得再试第二次。
        let attempts = if fail_create { 1 } else { 2 };
        assert_eq!(
            store.calls.load(Ordering::SeqCst),
            attempts,
            "fallback must not retry the disk"
        );
        assert_eq!(
            entered.load(Ordering::SeqCst),
            2,
            "prefix then resumed write"
        );
    }
}

/// 内存也预约不到时不得无限回退：本次准备显式终结，磁盘错误分类原样上报。
#[test]
fn disk_failure_without_headroom_is_rejected() {
    // 预算只够前缀 allowance：回退要整笔预约逻辑上界，必然不足。
    let budget = budget(64);
    let store = failing_store(true);
    let allowance = budget.try_reserve(32).expect("allowance");
    let (source, _) = exact_source(vec![b'a'; 512]);
    let error = freeze_snapshot(
        &(Arc::clone(&source) as Arc<dyn SnapshotSource>),
        allowance,
        Some(failing_context(&store, &budget)),
    )
    .err()
    .expect("fallback without headroom");
    assert!(
        matches!(error, LangfuseError::Spool(SpoolError::Unavailable)),
        "unexpected error: {error}"
    );
    assert_eq!(store.calls.load(Ordering::SeqCst), 1);
}

/// 超过单条事件逻辑上界：不发布任何对象，也不回退到无界内存。
#[test]
fn spooled_content_beyond_the_logical_limit_is_rejected() {
    const MAX_BYTES: usize = 128;
    let root = tempfile::tempdir().expect("temporary root");
    let store = native_spool(root.path());
    let budget = budget(64 * 1024);
    let registry = CleanupRegistry::new(dyn_store(&store), Arc::clone(&budget), 4);
    let allowance = budget.try_reserve(32).expect("allowance");
    let (source, _) = exact_source(vec![b'a'; MAX_BYTES + 1]);
    let error = freeze_snapshot(
        &(Arc::clone(&source) as Arc<dyn SnapshotSource>),
        allowance,
        Some((
            context(dyn_store(&store), &budget, 32, MAX_BYTES),
            registry.try_register().expect("slot"),
        )),
    )
    .err()
    .expect("oversized spooled snapshot");
    assert!(
        matches!(
            error,
            LangfuseError::PayloadTooLarge { limit_bytes } if limit_bytes == MAX_BYTES
        ),
        "unexpected error: {error}"
    );
    assert_eq!(
        store.usage().expect("usage").objects,
        0,
        "no published object"
    );
}
