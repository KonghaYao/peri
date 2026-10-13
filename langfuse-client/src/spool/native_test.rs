//! native 适配器的真实文件系统测试（契约 §8 P1 用例）。
//!
//! 全部用例使用 [`tempfile`] 临时根 + `open_at`，不触碰用户私有目录；跨进程用例以
//! `current_exe()` 重新执行本测试二进制，在真实子进程里持锁/持预留（`claim`、`reserve`），
//! 不使用 mock、不凭 PID 文件判断存活。测试自带可控等待，不与真实墙钟做精确次数断言。

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};

use crate::config::Compression;
use crate::memory::MemoryBudget;
use crate::spool::cleanup::CleanupRegistry;
use crate::spool::types::{
    ObjectId, ObjectKind, ObjectMeta, QuotaReservation, SpoolState, SpoolStore, SpoolUsage,
};
use crate::spool::{CleanupRegistration, RouteFingerprint, SpoolError, SpoolObject, SpoolReader};

use super::{
    create_private_dir_all, hex, id_hex, native_scan, now_ms, parse_id, unhex, NativeInner,
    NativeSpool, DESCRIPTOR_VERSION,
};

/// 取锁语义（不可领取判定、并发删除降级、能力探针竞争）单独成文件，保持本文件在自设上限内。
/// 本文件由 `native.rs` 以 `#[path]` 挂载，子模块同样显式给路径。
#[path = "native_test/lock.rs"]
mod lock;

/// 契约 §8：配额 8 MiB。
const QUOTA_BYTES: u64 = 8 * 1024 * 1024;
/// 子进程等待父进程放行的上限；到点自行退出，测试不会挂死。
const CHILD_WAIT: Duration = Duration::from_secs(20);
/// 子进程实际监听的用例名（`--exact` 过滤器）。
const CHILD_TEST: &str = "spool::native::tests::cross_process_helper";

/// 隔离域摘要：同 seed 即同 route，不同 seed 即不同部署。
fn route_of(seed: u8) -> RouteFingerprint {
    RouteFingerprint::from_bytes([seed; 32])
}

/// 固定对象 ID（测试内可预测，不依赖随机数）。
fn id_of(seed: u8) -> ObjectId {
    ObjectId::from_bytes([seed; 16])
}

fn route_from_hex(text: &str) -> RouteFingerprint {
    let bytes = unhex(text).expect("route hex");
    let mut route = [0u8; 32];
    route.copy_from_slice(&bytes);
    RouteFingerprint::from_bytes(route)
}

/// 局部 fixture：临时根上的 native store、独立隔离域与清理注册表。
///
/// 根目录由调用方持有（[`tempfile::TempDir`]），因此同一用例可以先后打开两个实例，
/// 模拟进程重启前后的两个生命周期。
struct Fixture {
    root: PathBuf,
    route: RouteFingerprint,
    store: Arc<dyn SpoolStore>,
    registry: CleanupRegistry,
}

impl Fixture {
    fn open(root: PathBuf, route: RouteFingerprint) -> Self {
        let spool = NativeSpool::open_at(root.clone(), route, QUOTA_BYTES).expect("open spool");
        let store: Arc<dyn SpoolStore> = Arc::new(spool);
        let budget = MemoryBudget::new(1024 * 1024).expect("memory budget");
        let registry = CleanupRegistry::new(Arc::clone(&store), budget, 16);
        Self {
            root,
            route,
            store,
            registry,
        }
    }

    fn root(&self) -> &Path {
        &self.root
    }

    /// 与适配器一致的对象布局：`<root>/peri/telemetry-spool/v1/<route>/objects/<b2>/<id>`。
    fn route_dir(&self) -> PathBuf {
        self.root
            .join("peri/telemetry-spool/v1")
            .join(hex(self.route.as_bytes()))
    }

    fn object_dir(&self, id: ObjectId) -> PathBuf {
        self.route_dir()
            .join("objects")
            .join(format!("{:02x}", NativeInner::bucket_of(id)))
            .join(id_hex(id))
    }

    fn tmp_dir(&self) -> PathBuf {
        self.route_dir().join("tmp")
    }

    fn register(&self) -> CleanupRegistration {
        self.registry.try_register().expect("cleanup slot")
    }

    fn reservation(&self, bytes: u64) -> Box<dyn QuotaReservation> {
        self.store.reserve(bytes).expect("reserve quota")
    }

    /// 未压缩元数据：`stored_len == raw_len`，Input 无事件数（契约 §5.2）。
    fn meta_for(&self, id: ObjectId, kind: ObjectKind, body: &[u8]) -> ObjectMeta {
        let accepted = now_ms();
        ObjectMeta {
            version: DESCRIPTOR_VERSION,
            id,
            kind,
            route: self.route,
            event_count: 0,
            encoding: Compression::None,
            raw_len: body.len() as u64,
            stored_len: body.len() as u64,
            sha256: Sha256::digest(body).into(),
            accepted_at_ms: accepted,
            expires_at_ms: accepted + 60_000,
            attempts: 0,
            next_attempt_at_ms: None,
            state: SpoolState::Ready,
        }
    }

    /// 走真实写入/发布路径返回交付句柄；调用方决定何时归还或删除。
    fn publish_owned(&self, kind: ObjectKind, body: &[u8]) -> SpoolObject {
        let mut temp = self
            .store
            .create(self.reservation(body.len() as u64), kind, self.register())
            .expect("create temporary object");
        let id = temp.id();
        temp.write_all(body).expect("write spool body");
        let meta = self.meta_for(id, kind, body);
        temp.publish(meta).expect("publish spool object")
    }

    /// 对象目录实际占用的字节（正文 + 描述信息）。
    fn accounted(&self, id: ObjectId) -> u64 {
        fs::read_dir(self.object_dir(id))
            .expect("object directory")
            .map(|entry| entry.expect("entry").metadata().expect("metadata").len())
            .sum()
    }
}

/// 直接按适配器的磁盘格式铺一个「上个进程已发布并退出」的对象：正文 + 描述信息，不持任何锁。
///
/// 描述信息用适配器自己的编码器生成，因此这不是手写 JSON 夹带；恢复/领取/校验路径读到的
/// 就是真实格式。`meta` 允许携带与 fixture 不同的 route，用于隔离域用例。
fn write_entry(fixture: &Fixture, meta: &ObjectMeta, body: &[u8]) {
    let dir = fixture.object_dir(meta.id);
    create_private_dir_all(&dir).expect("object directory");
    fs::write(dir.join("body"), body).expect("object body");
    let bytes = native_scan::descriptor_bytes(meta).expect("descriptor bytes");
    fs::write(dir.join("meta"), bytes).expect("object descriptor");
}

fn read_all(reader: SpoolReader) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut reader = reader;
    reader.read_to_end(&mut bytes).expect("read spool body");
    bytes
}

/// 有界等待：等真实事件（子进程 marker）而不是固定 sleep。
/// 原子落盘标记内容：先写旁路文件再 rename，父进程不会读到写了一半的标记。
fn publish_marker(path: &Path, content: &[u8]) {
    let staging = path.with_extension("writing");
    fs::write(&staging, content).expect("marker content");
    fs::rename(&staging, path).expect("marker rename");
}

fn wait_for(path: &Path, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if path.exists() {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("child did not signal {} within {timeout:?}", path.display());
}

/// 真实子进程：以当前测试二进制重新执行自身，扮演 [`cross_process_helper`]。
struct ChildProcess {
    child: Option<Child>,
    release: PathBuf,
}

impl ChildProcess {
    fn finish(mut self) -> Output {
        let child = self.child.take().expect("child handle");
        let _ = fs::write(&self.release, b"release");
        child.wait_with_output().expect("wait child process")
    }
}

impl Drop for ChildProcess {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            // 用例 panic 也要放行子进程，避免留下悬挂的持锁进程。
            let _ = fs::write(&self.release, b"release");
            let _ = child.wait();
        }
    }
}

fn spawn_child(
    fixture: &Fixture,
    mode: &str,
    marker: &Path,
    release: &Path,
    extra: &[(&str, String)],
) -> ChildProcess {
    let exe = std::env::current_exe().expect("current test binary");
    let mut command = Command::new(exe);
    command
        .arg(CHILD_TEST)
        .args(["--exact", "--ignored", "--nocapture", "--test-threads=1"])
        .env("PERI_SPOOL_TEST_ROOT", fixture.root())
        .env("PERI_SPOOL_TEST_ROUTE", hex(fixture.route.as_bytes()))
        .env("PERI_SPOOL_TEST_QUOTA", QUOTA_BYTES.to_string())
        .env("PERI_SPOOL_TEST_MODE", mode)
        .env("PERI_SPOOL_TEST_MARKER", marker)
        .env("PERI_SPOOL_TEST_RELEASE", release)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in extra {
        command.env(key, value);
    }
    let child = command.spawn().expect("spawn child test process");
    ChildProcess {
        child: Some(child),
        release: release.to_path_buf(),
    }
}

/// 跨进程 helper 的角色：持有对象锁或配额预留，等父进程放行后退出。
///
/// 只在父进程设置了环境变量时工作；被测试框架单独执行时直接返回。真实 OS advisory lock
/// 只有进程退出才释放，这正是「跨进程排他」要证明的部分。
#[test]
#[ignore = "cross-process helper：由本文件的跨进程用例以子进程重新执行"]
fn cross_process_helper() {
    let Some(root) = std::env::var_os("PERI_SPOOL_TEST_ROOT") else {
        return;
    };
    let route = route_from_hex(&std::env::var("PERI_SPOOL_TEST_ROUTE").expect("route hex"));
    let quota: u64 = std::env::var("PERI_SPOOL_TEST_QUOTA")
        .expect("quota")
        .parse()
        .expect("quota number");
    let marker = PathBuf::from(std::env::var_os("PERI_SPOOL_TEST_MARKER").expect("marker"));
    let release = PathBuf::from(std::env::var_os("PERI_SPOOL_TEST_RELEASE").expect("release"));
    let mode = std::env::var("PERI_SPOOL_TEST_MODE").expect("mode");
    let spool = NativeSpool::open_at(PathBuf::from(root), route, quota).expect("child open spool");
    match mode.as_str() {
        "claim" => {
            let id = parse_id(&std::env::var("PERI_SPOOL_TEST_OBJECT").expect("object id"))
                .expect("object id hex");
            let claim = spool
                .claim(id)
                .expect("child claim")
                .expect("object claimable");
            assert_eq!(claim.meta().id, id);
            publish_marker(&marker, b"claimed");
            wait_for_exit(&release);
            drop(claim);
        }
        "reserve" => {
            let bytes: u64 = std::env::var("PERI_SPOOL_TEST_BYTES")
                .expect("bytes")
                .parse()
                .expect("bytes number");
            let reservation = spool.reserve(bytes).expect("child reserve");
            assert_eq!(reservation.bytes(), bytes);
            publish_marker(&marker, b"reserved");
            wait_for_exit(&release);
            drop(reservation);
        }
        other => panic!("unknown child mode {other}"),
    }
}

/// 子进程侧等待放行文件；到点即退出，父进程不会永久等待。
fn wait_for_exit(release: &Path) {
    let deadline = Instant::now() + CHILD_WAIT;
    while Instant::now() < deadline {
        if release.exists() {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// 契约 §8：先预留再保留，且已写额度只回收一次。
#[tokio::test]
async fn reserve_before_retain_and_release_once() {
    let dir = tempfile::tempdir().expect("temporary root");
    let mut fixture = Fixture::open(dir.path().to_path_buf(), route_of(1));
    let body = vec![0x5au8; 4096];
    // 预留先于保留：create 之前额度已计入 reserved，磁盘上还没有对象。
    let reservation = fixture.reservation(body.len() as u64);
    assert_eq!(reservation.bytes(), body.len() as u64);
    let usage = fixture.store.usage().expect("usage");
    assert_eq!(usage.reserved_bytes, body.len() as u64);
    assert_eq!(usage.stored_bytes, 0);
    assert_eq!(usage.objects, 0);
    let mut temp = fixture
        .store
        .create(reservation, ObjectKind::Export, fixture.register())
        .expect("create temporary object");
    let id = temp.id();
    temp.write_all(&body).expect("write spool body");
    assert_eq!(temp.written_len(), body.len() as u64);
    // 写入中的正文占用来自预留本身：不额外记账，也不能静默超支。
    assert_eq!(
        fixture.store.usage().expect("usage").reserved_bytes,
        body.len() as u64
    );
    let object = temp
        .publish(fixture.meta_for(id, ObjectKind::Export, &body))
        .expect("publish spool object");
    // publish 把预留转成实际存储：同一批字节不双计。
    let usage = fixture.store.usage().expect("usage");
    assert_eq!(usage.reserved_bytes, 0);
    assert_eq!(usage.objects, 1);
    assert_eq!(usage.stored_bytes, fixture.accounted(id));
    assert!(usage.stored_bytes >= body.len() as u64);
    drop(object);
    fixture.registry.drain().await.expect("cleanup drain");
    // 确认删除后恰好回收一次：第二次 drain 不再回收，占用归零。
    assert_eq!(fixture.registry.disarmed(), (1, 0));
    fixture
        .registry
        .drain()
        .await
        .expect("second cleanup drain");
    assert_eq!(fixture.registry.disarmed(), (1, 0));
    assert_eq!(
        fixture.store.usage().expect("usage"),
        SpoolUsage {
            reserved_bytes: 0,
            stored_bytes: 0,
            objects: 0,
        }
    );
}

/// 契约 §8：写入期间任何读路径都不得看到半个正文。
#[test]
fn atomic_publish_never_exposes_partial_body() {
    let dir = tempfile::tempdir().expect("temporary root");
    let fixture = Fixture::open(dir.path().to_path_buf(), route_of(2));
    let body = vec![b'a'; 64 * 1024];
    let mut temp = fixture
        .store
        .create(
            fixture.reservation(body.len() as u64),
            ObjectKind::Export,
            fixture.register(),
        )
        .expect("create temporary object");
    let id = temp.id();
    temp.write_all(&body[..1024]).expect("partial write");
    // writing 阶段：临时正文在 tmp/ 下，对象目录与描述信息都不存在。
    assert!(fixture
        .tmp_dir()
        .join(format!("{}.part", id_hex(id)))
        .exists());
    assert!(!fixture.object_dir(id).exists());
    assert!(fixture
        .store
        .list(None, 64)
        .expect("list")
        .entries
        .is_empty());
    assert!(matches!(
        fixture.store.inspect(id),
        Err(SpoolError::NotFound)
    ));
    assert!(matches!(fixture.store.open(id), Err(SpoolError::NotFound)));
    assert!(matches!(fixture.store.claim(id), Err(SpoolError::NotFound)));
    // 完成写入后原子发布：可见对象一次出现，正文完整。
    temp.write_all(&body[1024..]).expect("rest of body");
    let object = temp
        .publish(fixture.meta_for(id, ObjectKind::Export, &body))
        .expect("publish spool object");
    assert_eq!(object.meta().stored_len, body.len() as u64);
    assert!(!fixture
        .tmp_dir()
        .join(format!("{}.part", id_hex(id)))
        .exists());
    let page = fixture.store.list(None, 64).expect("list after publish");
    assert_eq!(page.entries.len(), 1);
    assert_eq!(page.entries[0].id, id);
    assert!(page.next.is_none());
    assert_eq!(read_all(fixture.store.open(id).expect("open body")), body);
}

/// 契约 §8：配额预算必须含写入中与删除失败的对象，且在确认清理后归零。
#[tokio::test]
async fn quota_includes_writing_and_failed_delete() {
    let dir = tempfile::tempdir().expect("temporary root");
    let mut fixture = Fixture::open(dir.path().to_path_buf(), route_of(3));
    // 满额预留后再要一字节：可见的 QuotaExceeded，而不是静默超支。
    let reservation = fixture.reservation(QUOTA_BYTES);
    let error = match fixture.store.reserve(1024) {
        Ok(extra) => panic!(
            "quota exhausted but reserve returned {} bytes",
            extra.bytes()
        ),
        Err(error) => error,
    };
    match error {
        SpoolError::QuotaExceeded {
            requested,
            available,
        } => {
            assert_eq!(requested, 1024);
            assert_eq!(available, 0);
        }
        other => panic!("expected QuotaExceeded, got {other}"),
    }
    drop(reservation);
    // 写入中的临时文件计入预留：取消后恰好归还。
    let body = vec![b'w'; 2048];
    let mut temp = fixture
        .store
        .create(
            fixture.reservation(4096),
            ObjectKind::Export,
            fixture.register(),
        )
        .expect("create temporary object");
    let temp_id = temp.id();
    temp.write_all(&body).expect("write spool body");
    assert_eq!(fixture.store.usage().expect("usage").reserved_bytes, 4096);
    assert!(fixture
        .tmp_dir()
        .join(format!("{}.part", id_hex(temp_id)))
        .exists());
    temp.abort().expect("abort writing object");
    assert!(!fixture
        .tmp_dir()
        .join(format!("{}.part", id_hex(temp_id)))
        .exists());
    assert_eq!(fixture.store.usage().expect("usage").reserved_bytes, 0);
    // 删除失败：对象占用必须仍然可见，不能提前释放额度。
    let published = fixture.publish_owned(ObjectKind::Export, b"delete fails while claimed");
    let id = published.meta().id;
    let accounted = fixture.accounted(id);
    match fixture.store.delete(id).expect_err("busy object") {
        SpoolError::Busy => {}
        other => panic!("expected Busy, got {other}"),
    }
    let usage = fixture.store.usage().expect("usage");
    assert_eq!(usage.stored_bytes, accounted);
    assert_eq!(usage.objects, 1);
    // 交回锁后删除成功：占用与对象数归零，回收只发生一次。
    let claim = published.take_claim().expect("delivery claim");
    drop(claim);
    fixture.store.delete(id).expect("delete released object");
    drop(published);
    fixture.registry.drain().await.expect("cleanup drain");
    let usage = fixture.store.usage().expect("usage");
    assert_eq!(usage.reserved_bytes, 0);
    assert_eq!(usage.stored_bytes, 0);
    assert_eq!(usage.objects, 0);
    assert_eq!(fixture.registry.disarmed(), (2, 0));
    fixture
        .registry
        .drain()
        .await
        .expect("second cleanup drain");
    assert_eq!(fixture.registry.disarmed(), (2, 0));
}

/// 契约 §8：跨进程领取必须靠真实 OS advisory lock 排他，不能靠 PID 存活判断。
#[test]
fn claim_is_exclusive_across_processes() {
    let dir = tempfile::tempdir().expect("temporary root");
    let fixture = Fixture::open(dir.path().to_path_buf(), route_of(4));
    let body = b"cross-process delivery body";
    let meta = fixture.meta_for(id_of(0x31), ObjectKind::Export, body);
    write_entry(&fixture, &meta, body);
    let marker = fixture.root().join("child.claim.ready");
    let release = fixture.root().join("child.claim.release");
    let child = spawn_child(
        &fixture,
        "claim",
        &marker,
        &release,
        &[("PERI_SPOOL_TEST_OBJECT", id_hex(meta.id))],
    );
    wait_for(&marker, CHILD_WAIT);
    // marker 由子进程在**取到锁之后**写入：它的内容证明锁真的在另一个进程手里。
    assert_eq!(fs::read(&marker).expect("marker"), b"claimed");
    // 另一个进程持锁：本进程领取必须返回 None，删除必须报 Busy。
    assert!(fixture.store.claim(meta.id).expect("claim").is_none());
    assert!(matches!(
        fixture.store.delete(meta.id),
        Err(SpoolError::Busy)
    ));
    let output = child.finish();
    assert!(
        output.status.success(),
        "child failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    // 锁随进程退出释放：此时才允许领取，并且读到完整正文。
    let claim = fixture
        .store
        .claim(meta.id)
        .expect("claim after child exit")
        .expect("claim free");
    assert_eq!(claim.meta().id, meta.id);
    assert_eq!(read_all(claim.open().expect("open claimed body")), body);
}

/// 契约 §8：预留额度跨进程可见，另一个进程的存活预留必须真实占额。
#[test]
fn reserve_is_exclusive_across_processes() {
    let dir = tempfile::tempdir().expect("temporary root");
    let fixture = Fixture::open(dir.path().to_path_buf(), route_of(5));
    let held = QUOTA_BYTES - 4096;
    let marker = fixture.root().join("child.reserve.ready");
    let release = fixture.root().join("child.reserve.release");
    let child = spawn_child(
        &fixture,
        "reserve",
        &marker,
        &release,
        &[("PERI_SPOOL_TEST_BYTES", held.to_string())],
    );
    wait_for(&marker, CHILD_WAIT);
    assert_eq!(fs::read(&marker).expect("marker"), b"reserved");
    assert!(fixture.store.usage().expect("usage").reserved_bytes >= held);
    let error = match fixture.store.reserve(2 * 4096) {
        Ok(extra) => panic!(
            "other process holds {} bytes but reserve returned {} bytes",
            held,
            extra.bytes()
        ),
        Err(error) => error,
    };
    match error {
        SpoolError::QuotaExceeded {
            requested,
            available,
        } => {
            assert_eq!(requested, 2 * 4096);
            assert_eq!(available, 4096);
        }
        other => panic!("expected QuotaExceeded, got {other}"),
    }
    let output = child.finish();
    assert!(
        output.status.success(),
        "child failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    // 子进程退出：存活锁消失，额度回归本进程。
    let reservation = fixture.reservation(2 * 4096);
    assert_eq!(reservation.bytes(), 2 * 4096);
    assert_eq!(
        fixture.store.usage().expect("usage").reserved_bytes,
        2 * 4096
    );
}

/// 写入中取消（显式 abort 与丢弃句柄）都不得留下可见对象或占用额度。
#[test]
fn abort_during_writing_leaves_no_visible_object() {
    let dir = tempfile::tempdir().expect("temporary root");
    let fixture = Fixture::open(dir.path().to_path_buf(), route_of(6));
    let body = vec![b'c'; 1024];
    let mut aborted = fixture
        .store
        .create(
            fixture.reservation(body.len() as u64),
            ObjectKind::Export,
            fixture.register(),
        )
        .expect("create temporary object");
    let aborted_id = aborted.id();
    aborted.write_all(&body[..512]).expect("partial write");
    aborted.abort().expect("abort writing object");
    assert!(!fixture
        .tmp_dir()
        .join(format!("{}.part", id_hex(aborted_id)))
        .exists());
    assert!(matches!(
        fixture.store.claim(aborted_id),
        Err(SpoolError::NotFound)
    ));
    // 丢弃句柄（写入中取消的另一种形态）走 Drop：同样清理临时产物并归还预留。
    let mut dropped = fixture
        .store
        .create(
            fixture.reservation(body.len() as u64),
            ObjectKind::Export,
            fixture.register(),
        )
        .expect("create temporary object");
    let dropped_id = dropped.id();
    dropped.write_all(&body).expect("write spool body");
    drop(dropped);
    assert!(!fixture
        .tmp_dir()
        .join(format!("{}.part", id_hex(dropped_id)))
        .exists());
    assert!(fixture
        .store
        .list(None, 64)
        .expect("list")
        .entries
        .is_empty());
    assert_eq!(
        fixture.store.usage().expect("usage"),
        SpoolUsage {
            reserved_bytes: 0,
            stored_bytes: 0,
            objects: 0,
        }
    );
    // 额度确实回到池子里：整额预留成功。
    let reservation = fixture.reservation(QUOTA_BYTES);
    assert_eq!(reservation.bytes(), QUOTA_BYTES);
}

/// 写失败/满盘类错误的可见分类与归还：写超预留报 Io，配额耗尽报 QuotaExceeded。
///
/// 临时目录无法稳定复现真实 ENOSPC，因此这里覆盖两条可复现路径：writer 拒绝写超预留
/// （底层写失败一律归类 [`SpoolError::Io`]）与额度耗尽（[`SpoolError::QuotaExceeded`]），
/// 两者都必须显式可见且清理后归零，不静默超支。
#[test]
fn write_beyond_reservation_is_visible_and_releases_on_abort() {
    let dir = tempfile::tempdir().expect("temporary root");
    let fixture = Fixture::open(dir.path().to_path_buf(), route_of(7));
    let mut temp = fixture
        .store
        .create(
            fixture.reservation(64),
            ObjectKind::Export,
            fixture.register(),
        )
        .expect("create temporary object");
    let error = temp
        .write_all(&[0u8; 128])
        .expect_err("write beyond reservation");
    assert_eq!(error.kind(), std::io::ErrorKind::Other);
    assert!(error.to_string().contains("reserved quota"));
    assert_eq!(temp.written_len(), 0);
    assert_eq!(fixture.store.usage().expect("usage").reserved_bytes, 64);
    temp.abort().expect("abort after failed write");
    assert_eq!(
        fixture.store.usage().expect("usage"),
        SpoolUsage {
            reserved_bytes: 0,
            stored_bytes: 0,
            objects: 0,
        }
    );
    // 归还后可以重新按需预留整额：失败路径没有泄漏额度。
    let reservation = fixture.reservation(QUOTA_BYTES);
    drop(reservation);
}

/// 重启后恢复枚举：新实例从磁盘恢复可见对象、清理无锁残留，并以磁盘为准重建账本。
#[test]
fn restart_resumes_enumerable_objects_and_sweeps_residue() {
    let dir = tempfile::tempdir().expect("temporary root");
    let route = route_of(8);
    let body = b"published before restart";
    let published_id;
    {
        let fixture = Fixture::open(dir.path().to_path_buf(), route);
        let published = fixture.publish_owned(ObjectKind::Export, body);
        published_id = published.meta().id;
        // 崩溃 writer 的残留：正文与存活锁都在，但没有任何进程持锁。
        let stale = id_of(0x77);
        fs::write(
            fixture.tmp_dir().join(format!("{}.part", id_hex(stale))),
            b"half written body",
        )
        .expect("stale part");
        fs::write(
            fixture.tmp_dir().join(format!("{}.lock", id_hex(stale))),
            b"",
        )
        .expect("stale lock");
        // 发布中途崩溃：对象目录存在但没有描述信息，因此不可见。
        let orphan = id_of(0x78);
        let orphan_dir = fixture.object_dir(orphan);
        create_private_dir_all(&orphan_dir).expect("orphan directory");
        fs::write(orphan_dir.join("body"), b"never published").expect("orphan body");
        // 上一轮进程退出（句柄 drop）不删除任何文件，等价于没有显式清理的停机。
    }
    let reopened = Fixture::open(dir.path().to_path_buf(), route);
    let page = reopened.store.list(None, 64).expect("list after restart");
    assert_eq!(page.entries.len(), 1);
    assert_eq!(page.entries[0].id, published_id);
    assert_eq!(
        read_all(reopened.store.open(published_id).expect("open body")),
        body
    );
    // 无锁 writing 残留与未发布对象目录都被启动清扫回收。
    assert!(!reopened
        .tmp_dir()
        .join(format!("{}.part", id_hex(id_of(0x77))))
        .exists());
    assert!(!reopened
        .tmp_dir()
        .join(format!("{}.lock", id_hex(id_of(0x77))))
        .exists());
    assert!(!reopened.object_dir(id_of(0x78)).exists());
    // 账本以磁盘为准重建：对象数 1，占用等于该对象目录的实际字节数。
    let usage = reopened.store.usage().expect("usage");
    assert_eq!(usage.objects, 1);
    assert_eq!(usage.reserved_bytes, 0);
    assert_eq!(usage.stored_bytes, reopened.accounted(published_id));
    // 恢复出来的对象可被新实例独占领取。
    let claim = reopened
        .store
        .claim(published_id)
        .expect("claim")
        .expect("claim free");
    assert_eq!(claim.meta().id, published_id);
}

/// 隔离域不匹配：别的 route 的对象永远不得作为可见条目返回，只能被清理。
#[test]
fn foreign_route_entry_is_never_delivered() {
    let dir = tempfile::tempdir().expect("temporary root");
    let route_a = route_of(9);
    let route_b = route_of(10);
    let fixture = Fixture::open(dir.path().to_path_buf(), route_b);
    let body = b"object from another deployment route";
    let mut foreign = fixture.meta_for(id_of(0x5a), ObjectKind::Export, body);
    foreign.route = route_a;
    write_entry(&fixture, &foreign, body);
    // 隔离域不符：不返回条目、不发送，持锁清理后显式报错。
    match fixture
        .store
        .list(None, 64)
        .expect_err("foreign route must not be listed")
    {
        SpoolError::InvalidMetadata => {}
        other => panic!("expected InvalidMetadata, got {other}"),
    }
    assert!(!fixture.object_dir(foreign.id).exists());
    assert!(matches!(
        fixture.store.claim(foreign.id),
        Err(SpoolError::NotFound)
    ));
    // 另一个隔离域的实例在同一 root 下同样看不到本 domain 的目录。
    let other = Fixture::open(dir.path().to_path_buf(), route_a);
    assert!(other.store.list(None, 64).expect("list").entries.is_empty());
}

/// 损坏条目（长度或摘要不符）必须失败并持锁清理，绝不发送。
#[test]
fn corrupt_entry_is_removed_and_reported() {
    let dir = tempfile::tempdir().expect("temporary root");
    let fixture = Fixture::open(dir.path().to_path_buf(), route_of(11));
    let body = b"trusted body bytes";
    let truncated = fixture.meta_for(id_of(0x41), ObjectKind::Export, body);
    write_entry(&fixture, &truncated, body);
    fs::write(fixture.object_dir(truncated.id).join("body"), b"short").expect("tamper length");
    match fixture.store.list(None, 64).expect_err("corrupt length") {
        SpoolError::Corrupt { id } => assert_eq!(id, truncated.id),
        other => panic!("expected Corrupt, got {other}"),
    }
    assert!(!fixture.object_dir(truncated.id).exists());
    // 同长度不同内容：摘要校验失败，同样清理。
    let tampered = fixture.meta_for(id_of(0x42), ObjectKind::Export, body);
    write_entry(&fixture, &tampered, body);
    fs::write(
        fixture.object_dir(tampered.id).join("body"),
        b"tampered body byte",
    )
    .expect("tamper digest");
    match fixture.store.list(None, 64).expect_err("corrupt digest") {
        SpoolError::Corrupt { id } => assert_eq!(id, tampered.id),
        other => panic!("expected Corrupt, got {other}"),
    }
    assert!(!fixture.object_dir(tampered.id).exists());
    assert_eq!(
        fixture.store.usage().expect("usage"),
        SpoolUsage {
            reserved_bytes: 0,
            stored_bytes: 0,
            objects: 0,
        }
    );
}

/// checkpoint 持久化尝试/状态，但正文身份不可被改写。
#[test]
fn checkpoint_persists_attempt_state_without_rewriting_body() {
    let dir = tempfile::tempdir().expect("temporary root");
    let fixture = Fixture::open(dir.path().to_path_buf(), route_of(12));
    let body = b"checkpoint body";
    let meta = fixture.meta_for(id_of(0x61), ObjectKind::Export, body);
    write_entry(&fixture, &meta, body);
    let mut claim = fixture
        .store
        .claim(meta.id)
        .expect("claim")
        .expect("claim free");
    let mut sending = claim.meta().clone();
    sending.attempts = 1;
    sending.state = SpoolState::Sending;
    sending.next_attempt_at_ms = Some(sending.accepted_at_ms + 30_000);
    claim.checkpoint(sending.clone()).expect("checkpoint");
    let stored = fixture.store.inspect(meta.id).expect("inspect");
    assert_eq!(stored.attempts, 1);
    assert_eq!(stored.state, SpoolState::Sending);
    assert_eq!(stored.next_attempt_at_ms, sending.next_attempt_at_ms);
    // 正文身份字段不可经 checkpoint 改写。
    let mut rewritten = sending;
    rewritten.stored_len += 1;
    assert!(matches!(
        claim.checkpoint(rewritten),
        Err(SpoolError::InvalidMetadata)
    ));
    assert_eq!(
        fixture.store.inspect(meta.id).expect("inspect").stored_len,
        body.len() as u64
    );
}
