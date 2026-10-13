//! 取锁语义：跨 owner 的「不可领取」判定与并发删除下的降级边界（契约 §5.2）。
//!
//! 锁文件所在目录已不存在只表示条目不可领取，不是 IO 故障；扫描、领取、清理三条路径都必须
//! 按 [`LockAttempt`] 判定，禁止把正常清理竞争升级为整页扫描失败。这里同时钉住能力探针：
//! 对端**持锁**是并发竞争，不是「卷不支持 advisory lock」。

use std::fs;
use std::fs::OpenOptions;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use fs2::FileExt;
use sha2::{Digest, Sha256};

use super::super::{
    create_private_dir_all, id_hex, native_scan, now_ms, probe_durable_io, random_object_id,
    write_atomic, LockAttempt, NativeInner, DESCRIPTOR_VERSION,
};
use super::{id_of, route_of, Fixture, QUOTA_BYTES};
use crate::config::Compression;
use crate::spool::types::{ObjectId, ObjectKind, ObjectMeta, SpoolState};
use crate::spool::SpoolError;

/// 并发删除竞争：锁文件父目录消失必须报 [`LockAttempt::Gone`]，不得升级为 IO 故障。
///
/// 契约 §5.2：读锁/领取路径把「条目已被并发删除」与真实 IO 故障分开，正常清理竞争不能让
/// 整页扫描失败（macOS 的 `open(O_CREAT)` 在父目录被删除时报 `EINVAL`，Linux 报 `ENOENT`）。
#[test]
fn lock_attempt_reports_gone_instead_of_io_error_when_parent_is_deleted() {
    let dir = tempfile::tempdir().expect("temporary root");
    let fixture = Fixture::open(dir.path().to_path_buf(), route_of(13));
    // 对象目录不存在（已被并发删除或从未发布）：不可领取，但不是 IO 故障。
    let gone_path = fixture.object_dir(id_of(0x71)).join("lock");
    assert!(matches!(
        super::super::try_lock_file(&gone_path).expect("lock attempt"),
        LockAttempt::Gone
    ));
    // 父目录存在时照常建锁：第一次 Free、第二次 Held，释放后可再次取得。
    let held_path = fixture.object_dir(id_of(0x72)).join("lock");
    create_private_dir_all(held_path.parent().expect("parent")).expect("object directory");
    let held = match super::super::try_lock_file(&held_path).expect("first lock") {
        LockAttempt::Free(lock) => lock,
        other => panic!("expected Free, got {other:?}"),
    };
    assert!(matches!(
        super::super::try_lock_file(&held_path).expect("second lock"),
        LockAttempt::Held
    ));
    drop(held);
    assert!(matches!(
        super::super::try_lock_file(&held_path).expect("third lock"),
        LockAttempt::Free(_)
    ));
}

/// 「取锁」步骤上的 IO 故障：本次修复禁止的唯一结果（其余瞬时不可读由契约 §5.2 显式返回）。
fn is_lock_fault(error: &SpoolError) -> bool {
    matches!(error, SpoolError::Io { operation, .. } if operation.starts_with("lock spool"))
}

/// 写一个摘要与正文不符的条目（扫描读到即走持锁清理分支）；成功创建返回 true。
fn write_corrupt_entry(
    objects: &std::path::Path,
    route: crate::spool::RouteFingerprint,
    id: ObjectId,
    body: &[u8],
) -> bool {
    let path = objects
        .join(format!("{:02x}", NativeInner::bucket_of(id)))
        .join(id_hex(id));
    let accepted = now_ms();
    let meta = ObjectMeta {
        version: DESCRIPTOR_VERSION,
        id,
        kind: ObjectKind::Export,
        route,
        event_count: 0,
        encoding: Compression::None,
        raw_len: body.len() as u64,
        stored_len: body.len() as u64,
        // 摘要与正文不符：扫描读到即走持锁清理分支。
        sha256: Sha256::digest(b"other bytes").into(),
        accepted_at_ms: accepted,
        expires_at_ms: accepted + 60_000,
        attempts: 0,
        next_attempt_at_ms: None,
        state: SpoolState::Ready,
    };
    let descriptor = native_scan::descriptor_bytes(&meta).expect("descriptor bytes");
    create_private_dir_all(&path).is_ok()
        && fs::write(path.join("body"), body).is_ok()
        && write_atomic(&path.join("meta"), &descriptor).is_ok()
}

/// 并发删除不得把取锁升级为 IO 故障（契约 §5.2；`tests/spool_recovery` 竞态回归）。
///
/// 复现面：扫描遇到不可读条目（摘要不符）后要持锁清理，此时另一个 owner 正在删除同一对象
/// 目录。macOS 的 `open(O_CREAT)` 在父目录消失时报 `EINVAL`、Linux 报 `ENOENT`；修复前该
/// 瞬态错误被当作硬故障 `?` 上抛，正常清理竞争升级为整页扫描失败。
///
/// 竞争线程每轮用**新 ID** 铺一个摘要不符的条目再整目录删除：ID 不复用，因此「条目消失后
/// 又被重建」不会出现；`remove_dir_all` 逐个文件删除期间仍可能读到瞬时不可读的条目，那属于
/// 契约显式返回的范畴，本用例只禁止取锁本身报错。
///
/// 「持锁清理分支」的覆盖由循环前的确定性条目证明：并发删除的产物依赖 OS 删除语义（Windows
/// 上被占用句柄会让整目录删除失败，条目可能根本不留到扫描可见），不能拿它当分支覆盖证据。
#[test]
fn concurrent_delete_never_turns_lock_attempts_into_io_faults() {
    const BODY: &[u8] = b"corrupt entry body";
    let dir = tempfile::tempdir().expect("temporary root");
    let fixture = Fixture::open(dir.path().to_path_buf(), route_of(14));
    let objects = fixture.route_dir().join("objects");
    let route = fixture.route;
    // 确定性覆盖：单个摘要不符条目在没有并发修改时必须走到持锁清理并显式报损坏。
    assert!(
        write_corrupt_entry(&objects, route, id_of(0x5a), BODY),
        "先写入一个摘要不符条目"
    );
    let planted = fixture.store.list(None, 64);
    assert!(
        matches!(planted, Err(SpoolError::Corrupt { .. })),
        "扫描必须先走持锁清理分支: {planted:?}"
    );
    let stop = Arc::new(AtomicBool::new(false));
    let live: Arc<Mutex<Vec<ObjectId>>> = Arc::new(Mutex::new(Vec::new()));
    let churn = {
        let stop = Arc::clone(&stop);
        let live = Arc::clone(&live);
        std::thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                let id = random_object_id();
                let path = objects
                    .join(format!("{:02x}", NativeInner::bucket_of(id)))
                    .join(id_hex(id));
                if write_corrupt_entry(&objects, route, id, BODY) {
                    let mut live = live.lock().expect("live ids");
                    if live.len() >= 32 {
                        live.remove(0);
                    }
                    live.push(id);
                }
                // 等价于并发 owner 的 `remove_object`：整目录删除，不做协作握手。
                let _ = fs::remove_dir_all(&path);
            }
        })
    };
    let deadline = Instant::now() + Duration::from_millis(1500);
    let mut pages = 0usize;
    while Instant::now() < deadline {
        match fixture.store.list(None, 64) {
            Ok(_) => pages += 1,
            Err(SpoolError::Corrupt { .. }) => pages += 1,
            Err(error) => {
                assert!(!is_lock_fault(&error), "list 取锁不得报 IO 故障: {error}");
                pages += 1;
            }
        }
        let targets: Vec<ObjectId> = {
            let mut live = live.lock().expect("live ids");
            let take = live.len().min(4);
            live.drain(..take).collect()
        };
        for id in targets {
            match fixture.store.claim(id) {
                Ok(Some(claim)) => drop(claim),
                Ok(None) => {}
                Err(error) => assert!(!is_lock_fault(&error), "claim 取锁不得报 IO 故障: {error}"),
            }
        }
    }
    stop.store(true, Ordering::Relaxed);
    churn.join().expect("churn thread");
    assert!(pages > 0, "扫描必须实际执行过");
}

/// 对端持有 domain 锁是并发竞争，不是「卷不支持 advisory lock」。
///
/// 能力探针只做 `try_lock`（不许阻塞），所以这里直接钉住探针判定：同进程自持 domain 锁时
/// 整库 `open_at` 会在锁上排队，属于另一条路径。误判的代价是**整次装配降级**——
/// `peri-acp/src/host/telemetry.rs::native_spool` 收到 `Unavailable` 就放弃 spool，而紧随其后的
/// 启动重建本会按 domain 锁互斥等下去。
#[test]
fn peer_holding_the_domain_lock_is_contention_not_unavailability() {
    let dir = tempfile::tempdir().expect("temporary root");
    let fixture = Fixture::open(dir.path().to_path_buf(), route_of(15));
    let inner = NativeInner {
        route_dir: fixture.route_dir(),
        route: fixture.route,
        quota_bytes: QUOTA_BYTES,
    };
    assert!(probe_durable_io(&inner).is_ok(), "无竞争时探针必须通过");

    // 只借对端锁文件持锁：允许不存在时新建，已存在时保持原内容。
    let peer = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(inner.domain_lock_path())
        .expect("peer opens domain lock");
    peer.lock_exclusive().expect("peer holds domain lock");
    assert!(
        probe_durable_io(&inner).is_ok(),
        "对端持锁只是竞争，不得判定为卷不支持持久化"
    );
    drop(peer);
    assert!(probe_durable_io(&inner).is_ok(), "释放后照常通过");
}
