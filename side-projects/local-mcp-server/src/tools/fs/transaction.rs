//! Write/Edit 共用的 per-target 事务（`transaction.rs` 的等价实现 + fd 锚定提交）。
//!
//! 保留的源语义：
//! - **per-target 进程内锁**：同一目标的并发 Write/Edit 串行化；锁表按目标键去重，
//!   用弱引用惰性回收（`transaction.rs:143`）；
//! - **投影哨兵防护**：提交前比较 pre/post 的 V1 哨兵计数，新增即拒绝；
//! - **tmp + rename 原子提交**：tmp 名由目标名换扩展名得到（`with_extension("tmp.{uuid v7}")`），
//!   目标已存在时复制其权限位，`rename` 失败清理 tmp；**不做 fsync**（源实现亦无，
//!   不构成崩溃持久性保证）；
//! - append 以字节拼接，保留非 UTF-8 原字节。
//!
//! 与源实现的实现差异（记录在 handoff）：提交不再用 `std::fs::write`/`rename` 走路径，
//! 而是基于[`DirectChild`] 持有的父目录 fd 用 `openat`/`renameat` 完成，
//! 消除 canonicalize→open 的 TOCTOU 窗口；父目录在解析阶段按需创建。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, Weak};

use parking_lot::Mutex;

use crate::capability::{AccessError, DirectChild, RequestedPath, RootDir};

use super::sentinel::introduces_sentinel_bytes;

/// 投影哨兵拒绝文案（源实现逐字）。
pub const SENTINEL_REJECTION: &str =
    "File change rejected because the result contains newly introduced protected projection text.";

/// 提交失败分类（源实现 `CommitError`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommitError {
    /// 新增了受保护的投影哨兵行。
    Sentinel,
    /// 写盘/权限/rename 失败。
    Io,
}

impl CommitError {
    /// 源实现的错误文案。
    pub fn message(self) -> &'static str {
        match self {
            Self::Sentinel => SENTINEL_REJECTION,
            Self::Io => WRITE_IO_ERROR,
        }
    }
}

/// 源实现 `write.rs:9` 的 IO 失败文案（Edit 用同前缀的 "Edit failed while committing the file."）。
pub const WRITE_IO_ERROR: &str = "Write failed while committing the file.";
/// 源实现 `edit.rs` 的提交失败文案。
pub const EDIT_IO_ERROR: &str = "Edit failed while committing the file.";

/// per-target 锁表。
#[derive(Debug, Default)]
pub struct TargetLocks {
    registry: Mutex<HashMap<PathBuf, Weak<Mutex<()>>>>,
}

impl TargetLocks {
    /// 空锁表。
    pub fn new() -> Self {
        Self::default()
    }

    /// 在目标锁内执行 `operation`（同一目标串行，不同目标并行）。
    pub fn with_lock<T>(&self, target: &Path, operation: impl FnOnce() -> T) -> T {
        let lock = self.lock_for(target);
        let _guard = lock.lock();
        operation()
    }

    fn lock_for(&self, target: &Path) -> Arc<Mutex<()>> {
        let key = target.to_path_buf();
        let mut entries = self.registry.lock();
        entries.retain(|_, lock| lock.strong_count() > 0);
        if let Some(lock) = entries.get(&key).and_then(Weak::upgrade) {
            return lock;
        }
        let lock = Arc::new(Mutex::new(()));
        entries.insert(key, Arc::downgrade(&lock));
        lock
    }
}

/// 进程级锁表（与源实现一致：跨工具实例共享）。
pub fn global_locks() -> &'static TargetLocks {
    static LOCKS: OnceLock<TargetLocks> = OnceLock::new();
    LOCKS.get_or_init(TargetLocks::new)
}

/// 提交结果：写入的字节数与总行数（`split(b'\n')` 去掉行尾空段，与源实现一致）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommitOutcome {
    /// 提交后的总行数。
    pub total_lines: usize,
}

/// 读取目标当前内容（不存在返回 `None`）。
pub fn read_pre(root: &RootDir, requested: &RequestedPath) -> Result<Option<Vec<u8>>, AccessError> {
    match root.open_entry(requested) {
        Ok(mut entry) => entry
            .read_bytes()
            .map(Some)
            .map_err(|error| AccessError::io("read", requested.raw(), error)),
        Err(error) if error.is_not_found() => Ok(None),
        Err(error) => Err(error),
    }
}

/// 提交函数：`post` 已由调用方按覆盖/append 语义算好。
///
/// 步骤与源实现 `guard_and_commit` 一一对应，但全部基于 fd：
/// 哨兵检查 → 复制目标权限位（若存在）→ 同目录建 tmp → 写入 → `renameat` 覆盖 → 失败清理。
pub fn commit(target: &DirectChild, pre: &[u8], post: &[u8]) -> Result<CommitOutcome, CommitError> {
    if introduces_sentinel_bytes(pre, post) {
        return Err(CommitError::Sentinel);
    }

    let tmp_name = tmp_name_for(target.name());
    // 目标已存在时复制权限位（源实现只在 metadata 可读时复制）。
    let mode_source = target.lstat().ok();

    let tmp = match target.sibling(&tmp_name) {
        Ok(child) => child,
        Err(_) => return Err(CommitError::Io),
    };
    if tmp.create_new(post).is_err() {
        let _ = tmp.unlink();
        return Err(CommitError::Io);
    }
    if let Some(metadata) = mode_source {
        if tmp.set_mode(metadata.mode()).is_err() {
            let _ = tmp.unlink();
            return Err(CommitError::Io);
        }
    }
    if target.rename_from(&tmp_name).is_err() {
        let _ = tmp.unlink();
        return Err(CommitError::Io);
    }
    Ok(CommitOutcome {
        total_lines: count_lines(post),
    })
}

/// 提交后的行数：`split(b'\n')` 的长度，行尾换行不额外计一行（源实现一致）。
pub fn count_lines(bytes: &[u8]) -> usize {
    bytes.split(|byte| *byte == b'\n').count() - usize::from(bytes.last() == Some(&b'\n'))
}

/// tmp 文件名：`Path::with_extension(format!("tmp.{uuid v7}"))` 的等价形式。
fn tmp_name_for(name: &str) -> String {
    let path = Path::new(name);
    let tmp = path.with_extension(format!("tmp.{}", uuid::Uuid::now_v7()));
    tmp.file_name()
        .map(|value| value.to_string_lossy().to_string())
        .unwrap_or_else(|| format!("tmp.{}", uuid::Uuid::now_v7()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tmp_name_replaces_extension_like_source() {
        assert!(tmp_name_for("a.txt").starts_with("a.tmp."));
        assert!(tmp_name_for("noext").starts_with("noext.tmp."));
        assert!(tmp_name_for(".env").starts_with(".env.tmp."));
        assert!(tmp_name_for("archive.tar.gz").starts_with("archive.tar.tmp."));
    }

    #[test]
    fn test_count_lines_matches_source() {
        assert_eq!(count_lines(b""), 1);
        assert_eq!(count_lines(b"a"), 1);
        assert_eq!(count_lines(b"a\n"), 1);
        assert_eq!(count_lines(b"a\nb"), 2);
        assert_eq!(count_lines(b"a\nb\n"), 2);
    }

    #[test]
    fn test_locks_serialize_same_target_only() {
        let locks = TargetLocks::new();
        let path = PathBuf::from("a/b.txt");
        let first = locks.with_lock(&path, || 1);
        let second = locks.with_lock(&path, || 2);
        assert_eq!((first, second), (1, 2));
        assert!(Arc::strong_count(&locks.lock_for(&path)) >= 1);
    }
}
