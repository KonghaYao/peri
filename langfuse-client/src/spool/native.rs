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

use fs4::FileExt;

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

#[path = "native/native_spool.rs"]
mod native_spool;

pub(crate) use native_spool::NativeInner;
pub use native_spool::NativeSpool;
pub(crate) use native_spool::DESCRIPTOR_VERSION;
pub(crate) use native_spool::IO_CHUNK_BYTES;
pub(crate) use native_spool::MAX_DESCRIPTOR_BYTES;
pub(crate) use native_spool::MAX_PAGE_LIMIT;
pub(crate) use native_spool::OBJECT_BUCKETS;

pub(crate) use native_spool::create_private_dir_all;
pub(crate) use native_spool::create_private_file;
pub(crate) use native_spool::fsync_dir;
pub(crate) use native_spool::open_read_only;
#[cfg(test)]
use native_spool::probe_durable_io;
pub(crate) use native_spool::try_lock_file;
pub(crate) use native_spool::write_atomic;
pub(crate) use native_spool::LockAttempt;

pub(crate) use native_spool::hex;
pub(crate) use native_spool::id_hex;
pub(crate) use native_spool::io_error;
pub(crate) use native_spool::now_ms;
pub(crate) use native_spool::parse_id;
pub(crate) use native_spool::random_object_id;
pub(crate) use native_spool::unhex;
