//! native 文件暂存适配器：目录布局、私有根解析与启动恢复编排（契约 §3.2、§5.2）。
//!
//! 磁盘布局（`<root>` 为私有应用数据根；`<route>` 为 [`RouteFingerprint`] 的十六进制）：
//!
//! ```text
//! <root>/peri/telemetry-spool/v1/<route>/
//!   domain.lock            跨进程 domain 锁：quota 账本与启动恢复的互斥点
//!   quota/state.json       配额账本 {reserved:{rid:bytes}, stored_bytes, objects}
//!   quota/locks/<rid>      单个预留的存活锁：锁被持有 = owner 仍存活
//!   objects/<b2>/<id>/     已发布对象，`meta` 最后发布，`meta` 存在即可见
//!       meta               描述信息 v1（JSON，≤4 KiB）
//!       body               落盘正文（Input 为冻结 JSON，Export 为 OTLP 正文）
//!       lock               独占领取锁；持有者即唯一 owner
//!   tmp/<id>.lock          writing 存活锁（先建锁再建正文，扫描据此避开活跃 writer）
//!   tmp/<id>.part          writing 正文；只有 publish 才 rename 进 objects/
//! ```
//!
//! 不变式：
//! 1. 可见 = `objects/<b2>/<id>/meta` 存在且校验通过；`tmp/` 中的对象永不发送。
//! 2. 任何对象生命周期变更（claim/checkpoint/delete/扫描清理）都必须先持有 `<id>/lock`；
//!    跨进程所有权只由 OS advisory lock 判定，不使用 PID 存活判断（契约 §6.1）。
//! 3. 配额账本的每次变更都在 domain 锁临界区内完成；进程崩溃后的残留由 `open_at`
//!    的启动恢复重建（先重建 quota，再接受新磁盘写入）。
//! 4. 句柄只接受内部 [`ObjectId`]；日志与错误不记录 route 摘要、凭据与宿主路径。
//!
//! 平台要求：文件系统必须支持 fsync 与 advisory lock，否则返回
//! [`SpoolError::Unavailable`] 且不宣称崩溃持久性。本模块只在 native cfg 下挂载。

use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use fs2::FileExt;

use crate::spool::error::SpoolError;
use crate::spool::sealed;
use crate::spool::types::{
    ObjectClaim, ObjectId, ObjectKind, ObjectMeta, PageCursor, QuotaReservation, RouteFingerprint,
    SpoolPage, SpoolStore, SpoolUsage, TemporaryObject,
};
use crate::spool::{CleanupRegistration, SpoolReader, SpoolResult};

#[path = "native_object.rs"]
pub(crate) mod native_object;
#[path = "native_quota.rs"]
pub(crate) mod native_quota;
#[path = "native_scan.rs"]
pub(crate) mod native_scan;

#[cfg(test)]
#[path = "native_test.rs"]
mod tests;

/// 固定布局前缀（契约 §3.2：`peri/telemetry-spool/v1/<route>`）。
const LAYOUT_PREFIX: &str = "peri/telemetry-spool/v1";
/// 描述信息格式版本；未知版本一律隔离，不做兼容猜测。
pub(crate) const DESCRIPTOR_VERSION: u16 = 1;
/// 描述信息字节上限（契约 §4：单对象最多 4 KiB）。
pub(crate) const MAX_DESCRIPTOR_BYTES: u64 = 4 * 1024;
/// 单页元数据条数上限（契约 §4：每页 64 条、页 buffer 256 KiB）。
pub(crate) const MAX_PAGE_LIMIT: usize = 64;
/// 对象目录分桶数（按 [`ObjectId`] 首字节分桶，避免一次 `read_dir` 全收集排序）。
pub(crate) const OBJECT_BUCKETS: u16 = 256;
/// 正文摘要/拷贝的块大小（契约 §4：读写各 64 KiB 工作区账单）。
pub(crate) const IO_CHUNK_BYTES: usize = 64 * 1024;

/// native 文件暂存：共享同一 [`NativeInner`] 的句柄可同时存在，语义等价。
pub struct NativeSpool {
    inner: Arc<NativeInner>,
}

/// 适配器的不可变身份：route 目录、隔离摘要与 quota 预算。
pub(crate) struct NativeInner {
    route_dir: PathBuf,
    route: RouteFingerprint,
    quota_bytes: u64,
}

impl NativeSpool {
    /// 在本机私有应用数据根下打开；根无法确定或缺失时显式 [`SpoolError::Unavailable`]，
    /// **不回退 cwd**（契约 §3.2）。
    pub fn open_default(route: RouteFingerprint, quota_bytes: u64) -> SpoolResult<Self> {
        let root = private_app_data_root().ok_or(SpoolError::Unavailable)?;
        Self::open_at(root, route, quota_bytes)
    }

    /// 在调用方给定的根下打开 `<root>/peri/telemetry-spool/v1/<route>`；测试只用临时目录。
    pub fn open_at(root: PathBuf, route: RouteFingerprint, quota_bytes: u64) -> SpoolResult<Self> {
        let route_dir = root.join(LAYOUT_PREFIX).join(hex(route.as_bytes()));
        let inner = Arc::new(NativeInner {
            route_dir,
            route,
            quota_bytes,
        });
        ensure_layout(&inner)?;
        probe_durable_io(&inner)?;
        // 启动恢复顺序固定：先清扫崩溃 writer 的临时产物，再重建 quota 账本。
        native_quota::sweep_startup_residue(&inner)?;
        native_quota::rebuild(&inner)?;
        Ok(Self { inner })
    }
}

impl sealed::Sealed for NativeSpool {}

impl SpoolStore for NativeSpool {
    fn reserve(&self, bytes: u64) -> SpoolResult<Box<dyn QuotaReservation>> {
        native_quota::reserve(&self.inner, bytes)
    }

    fn create(
        &self,
        quota: Box<dyn QuotaReservation>,
        kind: ObjectKind,
        cleanup: CleanupRegistration,
    ) -> SpoolResult<Box<dyn TemporaryObject>> {
        native_object::create(&self.inner, quota, kind, cleanup)
    }

    fn inspect(&self, id: ObjectId) -> SpoolResult<ObjectMeta> {
        native_object::inspect(&self.inner, id)
    }

    fn open(&self, id: ObjectId) -> SpoolResult<SpoolReader> {
        native_object::open_body(&self.inner, id)
    }

    fn list(&self, after: Option<PageCursor>, limit: usize) -> SpoolResult<SpoolPage> {
        native_scan::list(&self.inner, after, limit)
    }

    fn claim(&self, id: ObjectId) -> SpoolResult<Option<Box<dyn ObjectClaim>>> {
        native_object::claim(&self.inner, id)
    }

    fn delete(&self, id: ObjectId) -> SpoolResult<()> {
        native_object::delete_if_present(&self.inner, id)
    }

    fn usage(&self) -> SpoolResult<SpoolUsage> {
        native_quota::usage(&self.inner)
    }
}

impl NativeInner {
    pub(crate) fn route(&self) -> RouteFingerprint {
        self.route
    }

    pub(crate) fn quota_bytes(&self) -> u64 {
        self.quota_bytes
    }

    pub(crate) fn route_dir(&self) -> &Path {
        &self.route_dir
    }

    pub(crate) fn domain_lock_path(&self) -> PathBuf {
        self.route_dir.join("domain.lock")
    }

    pub(crate) fn quota_state_path(&self) -> PathBuf {
        self.route_dir.join("quota").join("state.json")
    }

    pub(crate) fn quota_lock_path(&self, reservation: &str) -> PathBuf {
        self.route_dir.join("quota").join("locks").join(reservation)
    }

    pub(crate) fn objects_dir(&self) -> PathBuf {
        self.route_dir.join("objects")
    }

    /// 分桶目录：`objects/<id 首字节 hex>`。
    pub(crate) fn bucket_dir(&self, bucket: u8) -> PathBuf {
        self.objects_dir().join(format!("{bucket:02x}"))
    }

    pub(crate) fn bucket_of(id: ObjectId) -> u8 {
        id.as_bytes()[0]
    }

    pub(crate) fn object_dir(&self, id: ObjectId) -> PathBuf {
        self.bucket_dir(Self::bucket_of(id)).join(id_hex(id))
    }

    pub(crate) fn object_meta_path(&self, id: ObjectId) -> PathBuf {
        self.object_dir(id).join("meta")
    }

    pub(crate) fn object_body_path(&self, id: ObjectId) -> PathBuf {
        self.object_dir(id).join("body")
    }

    pub(crate) fn object_lock_path(&self, id: ObjectId) -> PathBuf {
        self.object_dir(id).join("lock")
    }

    pub(crate) fn tmp_dir(&self) -> PathBuf {
        self.route_dir.join("tmp")
    }

    pub(crate) fn writer_lock_path(&self, id: ObjectId) -> PathBuf {
        self.tmp_dir().join(format!("{}.lock", id_hex(id)))
    }

    pub(crate) fn writer_part_path(&self, id: ObjectId) -> PathBuf {
        self.tmp_dir().join(format!("{}.part", id_hex(id)))
    }
}

/// 私有应用数据根；无法确定或不存在时返回 `None`（调用方转 [`SpoolError::Unavailable`]）。
fn private_app_data_root() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var_os("HOME")?;
        existing_dir(PathBuf::from(home).join("Library/Application Support"))
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        if let Some(xdg) = std::env::var_os("XDG_DATA_HOME") {
            let candidate = PathBuf::from(xdg);
            // XDG 规范要求绝对路径；相对值不可信，直接忽略而不是按 cwd 解释。
            if candidate.is_absolute() {
                if let Some(dir) = existing_dir(candidate) {
                    return Some(dir);
                }
            }
        }
        let home = std::env::var_os("HOME")?;
        existing_dir(PathBuf::from(home).join(".local/share"))
    }
    #[cfg(windows)]
    {
        let appdata = std::env::var_os("APPDATA")?;
        existing_dir(PathBuf::from(appdata))
    }
    #[cfg(not(any(unix, windows)))]
    {
        None
    }
}

fn existing_dir(path: PathBuf) -> Option<PathBuf> {
    path.is_dir().then_some(path)
}

/// 创建布局目录与锁文件；目录 0700、文件 0600（其他平台等价 ACL 由宿主提供）。
fn ensure_layout(inner: &NativeInner) -> SpoolResult<()> {
    for dir in [
        inner.route_dir().to_path_buf(),
        inner.tmp_dir(),
        inner.objects_dir(),
        inner.route_dir().join("quota"),
        inner.route_dir().join("quota").join("locks"),
    ] {
        create_private_dir_all(&dir)
            .map_err(|source| io_error("create spool directory", source))?;
    }
    Ok(())
}

/// fsync 与 advisory lock 能力探针：不支持即 [`SpoolError::Unavailable`]，不宣称崩溃持久性。
///
/// 探针不阻塞：对端持锁说明卷支持 advisory lock，只是此刻被同部署的其他 owner 持有
/// （契约 §5.2 的跨进程竞争，平台错误形态见 [`is_lock_contention`]），不能降级成「不可用」
/// ——紧随其后的启动重建会按 domain 锁排队。
fn probe_durable_io(inner: &NativeInner) -> SpoolResult<()> {
    let probe =
        create_private_file(&inner.domain_lock_path()).map_err(|_| SpoolError::Unavailable)?;
    match probe.try_lock_exclusive() {
        Ok(()) => {}
        Err(error) if is_lock_contention(&error) => {}
        Err(_) => return Err(SpoolError::Unavailable),
    }
    probe.sync_all().map_err(|_| SpoolError::Unavailable)?;
    let _ = probe.unlock();
    Ok(())
}

/// 目录 0700 递归创建。
pub(crate) fn create_private_dir_all(path: &Path) -> io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)
}

/// 文件 0600 创建/截断。
pub(crate) fn create_private_file(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

/// 只读打开，不跟随可疑状态；调用方按错误分类。
pub(crate) fn open_read_only(path: &Path) -> io::Result<File> {
    File::open(path)
}

/// 目录 fsync（Unix）；其他平台不宣称目录项持久性。
pub(crate) fn fsync_dir(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        File::open(path)?.sync_all()
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

/// 同文件系统原子发布：临时文件 fsync 后 rename，再 fsync 父目录。
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    use std::io::Write;

    let tmp = path.with_extension("tmp");
    {
        let mut file = create_private_file(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    if let Some(parent) = path.parent() {
        fsync_dir(parent)?;
    }
    Ok(())
}

/// 锁文件尝试结果：把「条目已被并发删除」与真实 IO 故障分开（契约 §5.2）。
///
/// 并发删除下 `open(2)` 会报 `ENOENT`（Linux）或 `EINVAL`（macOS）：两者都只说明锁文件的
/// 父目录已经不在了，调用方应把该条目当作不存在，而不是把正常清理竞争升级为扫描失败。
#[derive(Debug)]
pub(crate) enum LockAttempt {
    /// 取得独占锁；句柄存活期间其他 owner 无法领取或删除该条目。
    Free(File),
    /// 其他 owner 已持锁。
    Held,
    /// 锁文件所在目录已不存在：条目不可领取，不是 IO 故障。
    Gone,
}

/// 同一文件的独占锁尝试（契约 §3.2）。
pub(crate) fn try_lock_file(path: &Path) -> io::Result<LockAttempt> {
    let mut options = OpenOptions::new();
    options.write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = match options.open(path) {
        Ok(file) => file,
        Err(error) => {
            // 只按「父目录是否仍在」判定竞争：父目录在而打开失败仍是真实故障，照常上抛。
            if !lock_parent_is_dir(path) {
                return Ok(LockAttempt::Gone);
            }
            return Err(error);
        }
    };
    match file.try_lock_exclusive() {
        Ok(()) => Ok(LockAttempt::Free(file)),
        Err(error) if is_lock_contention(&error) => Ok(LockAttempt::Held),
        Err(error) => Err(error),
    }
}

/// 对端持锁在平台上的错误形态。
///
/// Unix 是 `WouldBlock`（`EWOULDBLOCK`/`EAGAIN`）；Windows 的 `LockFileEx` 报
/// `ERROR_LOCK_VIOLATION`（33），当前 Rust 尚未把它映射成 `WouldBlock`。两者都只说明锁被
/// 另一个 owner 持有，属契约 §5.2 的跨进程竞争，不是 IO 故障。
fn is_lock_contention(error: &io::Error) -> bool {
    #[cfg(windows)]
    const CONTENTION_OS_ERROR: Option<i32> = Some(33);
    #[cfg(not(windows))]
    const CONTENTION_OS_ERROR: Option<i32> = None;

    error.kind() == io::ErrorKind::WouldBlock
        || CONTENTION_OS_ERROR.is_some_and(|code| error.raw_os_error() == Some(code))
}

/// 锁文件父目录是否仍是目录；无父目录按不可用处理（不回退到 cwd）。
fn lock_parent_is_dir(path: &Path) -> bool {
    path.parent().is_some_and(Path::is_dir)
}

/// 静态操作名 + 底层错误；不携带路径与凭据。
pub(crate) fn io_error(operation: &'static str, source: io::Error) -> SpoolError {
    SpoolError::Io { operation, source }
}

/// 当前墙上时钟毫秒；非负。
pub(crate) fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or(0)
}

/// 小写十六进制编码。
pub(crate) fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(char::from_digit((byte >> 4) as u32, 16).expect("nibble"));
        out.push(char::from_digit((byte & 0x0f) as u32, 16).expect("nibble"));
    }
    out
}

/// 十六进制解码；长度或字符非法返回 `None`。
pub(crate) fn unhex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    let mut out = Vec::with_capacity(text.len() / 2);
    let mut high: Option<u8> = None;
    for ch in text.chars() {
        let value = ch.to_digit(16)? as u8;
        match high.take() {
            None => high = Some(value),
            Some(previous) => out.push((previous << 4) | value),
        }
    }
    Some(out)
}

pub(crate) fn id_hex(id: ObjectId) -> String {
    hex(id.as_bytes())
}

/// 解析 32 位十六进制的对象 ID；其他长度或非法字符返回 `None`。
pub(crate) fn parse_id(text: &str) -> Option<ObjectId> {
    if text.len() != 32 {
        return None;
    }
    let bytes = unhex(text)?;
    let mut id = [0u8; 16];
    id.copy_from_slice(&bytes);
    Some(ObjectId::from_bytes(id))
}

/// 新对象 ID：进程盐 + 单调序号 + 时间，避免依赖 cwd 上的随机文件与额外依赖。
pub(crate) fn random_object_id() -> ObjectId {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::OnceLock;

    static SALT: OnceLock<[u64; 2]> = OnceLock::new();
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);

    let salt = *SALT.get_or_init(|| {
        let mut first = RandomState::new().build_hasher();
        first.write_u64(u64::from(std::process::id()));
        first.write_u64(now_ms() as u64);
        let a = first.finish();
        let mut second = RandomState::new().build_hasher();
        second.write_u64(a);
        second.write_u64(now_ms() as u64);
        let b = second.finish();
        [a, b]
    });
    let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let mut head = RandomState::new().build_hasher();
    head.write_u64(salt[0]);
    head.write_u64(sequence);
    head.write_u64(now_ms() as u64);
    let high = head.finish();
    let mut tail = RandomState::new().build_hasher();
    tail.write_u64(salt[1]);
    tail.write_u64(high);
    let low = tail.finish();

    let mut bytes = [0u8; 16];
    bytes[..8].copy_from_slice(&high.to_le_bytes());
    bytes[8..].copy_from_slice(&low.to_le_bytes());
    ObjectId::from_bytes(bytes)
}
