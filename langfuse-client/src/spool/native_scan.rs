//! 描述信息 v1、条目校验与分页枚举（契约 §3.2、§5.2、§6.1）。
//!
//! 本模块负责**格式层**：描述信息编解码、格式版本、隔离域（route）、长度与期限校验，以及
//! 磁盘桶分页索引与崩溃残留清扫。正文摘要的物理核对见
//! [`super::native_object::verify_digest`]，写入/发布/领取见 [`super::native_object`]。
//!
//! 校验失败规则（契约 §5.2「损坏/未知版本隔离诊断、清理，绝不发送」）：结构非法、隔离域不符
//! 或摘要不符的条目持对象锁清理后以显式错误返回，绝不作为可见条目返回；`meta` 未发布（writing
//! 残留）不可见；已被其他 owner 持锁则跳过，由其 owner 负责。

use std::fs;
use std::io::Read;
use std::path::Path;

use tracing::warn;

use crate::config::Compression;
use crate::spool::error::SpoolError;
use crate::spool::types::{ObjectId, ObjectKind, ObjectMeta, PageCursor, SpoolPage, SpoolState};
use crate::spool::{RouteFingerprint, SpoolResult};

use super::native_object;
use super::native_quota::accounted_bytes;
use super::{
    hex, id_hex, io_error, now_ms, open_read_only, parse_id, try_lock_file, unhex, LockAttempt,
    NativeInner, DESCRIPTOR_VERSION, MAX_DESCRIPTOR_BYTES, MAX_PAGE_LIMIT, OBJECT_BUCKETS,
};

/// 描述信息 v1；字段与 [`ObjectMeta`] 一一对应，未知字段视为不可识别格式并隔离。
#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Descriptor {
    version: u16,
    id: String,
    kind: String,
    route: String,
    event_count: u64,
    encoding: String,
    raw_len: u64,
    stored_len: u64,
    sha256: String,
    accepted_at_ms: i64,
    expires_at_ms: i64,
    attempts: u64,
    next_attempt_at_ms: Option<i64>,
    state: String,
}

fn kind_name(kind: ObjectKind) -> &'static str {
    match kind {
        ObjectKind::Input => "input",
        ObjectKind::Export => "export",
    }
}

fn parse_kind(text: &str) -> Option<ObjectKind> {
    match text {
        "input" => Some(ObjectKind::Input),
        "export" => Some(ObjectKind::Export),
        _ => None,
    }
}

fn state_name(state: SpoolState) -> &'static str {
    match state {
        SpoolState::Writing => "writing",
        SpoolState::Ready => "ready",
        SpoolState::Sending => "sending",
        SpoolState::RetryWait => "retry_wait",
        SpoolState::Delivered => "delivered",
        SpoolState::TerminalFailed => "terminal_failed",
    }
}

fn parse_state(text: &str) -> Option<SpoolState> {
    Some(match text {
        "writing" => SpoolState::Writing,
        "ready" => SpoolState::Ready,
        "sending" => SpoolState::Sending,
        "retry_wait" => SpoolState::RetryWait,
        "delivered" => SpoolState::Delivered,
        "terminal_failed" => SpoolState::TerminalFailed,
        _ => return None,
    })
}

fn encoding_name(encoding: Compression) -> &'static str {
    match encoding {
        Compression::Gzip => "gzip",
        Compression::None => "none",
    }
}

fn parse_encoding(text: &str) -> Option<Compression> {
    match text {
        "gzip" => Some(Compression::Gzip),
        "none" => Some(Compression::None),
        _ => None,
    }
}

fn route_from_hex(text: &str) -> Option<RouteFingerprint> {
    let bytes = unhex(text)?;
    if bytes.len() != 32 {
        return None;
    }
    let mut route = [0u8; 32];
    route.copy_from_slice(&bytes);
    Some(RouteFingerprint::from_bytes(route))
}

impl Descriptor {
    fn from_meta(meta: &ObjectMeta) -> Self {
        Self {
            version: meta.version,
            id: id_hex(meta.id),
            kind: kind_name(meta.kind).to_string(),
            route: hex(meta.route.as_bytes()),
            event_count: meta.event_count,
            encoding: encoding_name(meta.encoding).to_string(),
            raw_len: meta.raw_len,
            stored_len: meta.stored_len,
            sha256: hex(&meta.sha256),
            accepted_at_ms: meta.accepted_at_ms,
            expires_at_ms: meta.expires_at_ms,
            attempts: meta.attempts,
            next_attempt_at_ms: meta.next_attempt_at_ms,
            state: state_name(meta.state).to_string(),
        }
    }

    fn into_meta(self) -> SpoolResult<ObjectMeta> {
        let digest = unhex(&self.sha256).ok_or(SpoolError::InvalidMetadata)?;
        if digest.len() != 32 {
            return Err(SpoolError::InvalidMetadata);
        }
        let mut sha256 = [0u8; 32];
        sha256.copy_from_slice(&digest);
        Ok(ObjectMeta {
            version: self.version,
            id: parse_id(&self.id).ok_or(SpoolError::InvalidMetadata)?,
            kind: parse_kind(&self.kind).ok_or(SpoolError::InvalidMetadata)?,
            route: route_from_hex(&self.route).ok_or(SpoolError::InvalidMetadata)?,
            event_count: self.event_count,
            encoding: parse_encoding(&self.encoding).ok_or(SpoolError::InvalidMetadata)?,
            raw_len: self.raw_len,
            stored_len: self.stored_len,
            sha256,
            accepted_at_ms: self.accepted_at_ms,
            expires_at_ms: self.expires_at_ms,
            attempts: self.attempts,
            next_attempt_at_ms: self.next_attempt_at_ms,
            state: parse_state(&self.state).ok_or(SpoolError::InvalidMetadata)?,
        })
    }
}

/// 描述信息与隔离域一致性校验；正文长度/摘要由调用方按需追加。
pub(crate) fn validate_meta(inner: &NativeInner, meta: &ObjectMeta) -> SpoolResult<()> {
    if meta.version != DESCRIPTOR_VERSION || meta.route != inner.route() {
        return Err(SpoolError::InvalidMetadata);
    }
    if meta.expires_at_ms <= meta.accepted_at_ms {
        return Err(SpoolError::InvalidMetadata);
    }
    // Input 只是快照：无压缩、无事件数（契约 §5.2）。
    if meta.kind == ObjectKind::Input
        && (meta.encoding != Compression::None || meta.event_count != 0)
    {
        return Err(SpoolError::InvalidMetadata);
    }
    if meta.encoding == Compression::None && meta.stored_len != meta.raw_len {
        return Err(SpoolError::InvalidMetadata);
    }
    Ok(())
}

/// 描述信息序列化；超过单对象 4 KiB 上限即拒绝（契约 §4）。
pub(crate) fn descriptor_bytes(meta: &ObjectMeta) -> SpoolResult<Vec<u8>> {
    let bytes = serde_json::to_vec(&Descriptor::from_meta(meta))
        .map_err(|_| SpoolError::InvalidMetadata)?;
    if bytes.len() as u64 > MAX_DESCRIPTOR_BYTES {
        return Err(SpoolError::InvalidMetadata);
    }
    Ok(bytes)
}

/// 读取并校验描述信息；没有 `meta` 表示尚未发布（[`SpoolError::NotFound`]）。
pub(crate) fn read_meta(inner: &NativeInner, id: ObjectId) -> SpoolResult<ObjectMeta> {
    let file = match open_read_only(&inner.object_meta_path(id)) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(SpoolError::NotFound)
        }
        Err(error) => return Err(io_error("read spool descriptor", error)),
    };
    let mut bytes = Vec::new();
    file.take(MAX_DESCRIPTOR_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| io_error("read spool descriptor", error))?;
    if bytes.len() as u64 > MAX_DESCRIPTOR_BYTES {
        return Err(SpoolError::InvalidMetadata);
    }
    let meta: ObjectMeta = serde_json::from_slice::<Descriptor>(&bytes)
        .map_err(|_| SpoolError::InvalidMetadata)?
        .into_meta()?;
    if meta.id != id {
        return Err(SpoolError::InvalidMetadata);
    }
    validate_meta(inner, &meta)?;
    Ok(meta)
}

/// 到期投影：TTL 已过的对象在返回的 metadata 上表现为终态（不落盘，由调用方 checkpoint 持久化）。
pub(crate) fn project_expiry(meta: &mut ObjectMeta) {
    let settled = matches!(
        meta.state,
        SpoolState::Delivered | SpoolState::TerminalFailed
    );
    if !settled && now_ms() >= meta.expires_at_ms {
        meta.state = SpoolState::TerminalFailed;
    }
}

/// 分页枚举可见完整对象；页大小上限 64（契约 §4）。
///
/// 游标只承载「上一个已返回对象的 ID」，桶号由其首字节推出，因此不依赖一次性全收集排序；
/// 每次调用都从游标处重新扫描，新条目不会被永久漏过（允许重复，由调用方按 ObjectId 去重）。
pub(crate) fn list(
    inner: &NativeInner,
    after: Option<PageCursor>,
    limit: usize,
) -> SpoolResult<SpoolPage> {
    let limit = limit.clamp(1, MAX_PAGE_LIMIT);
    let (start, mut bound) = match after {
        Some(cursor) => decode_cursor(cursor.as_bytes())?,
        None => (0u8, None),
    };
    let mut entries: Vec<ObjectMeta> = Vec::new();
    // 桶号是 u16 计数：u8 的 `saturating_add` 在 255 处原地打转（永不出循环）。
    let mut bucket = u16::from(start);
    while bucket < OBJECT_BUCKETS {
        let bucket_dir = inner.bucket_dir(bucket as u8);
        for name in bucket_names(&bucket_dir)? {
            if bound.as_deref().is_some_and(|bound| name.as_str() <= bound) {
                continue;
            }
            let Some(id) = parse_id(&name) else {
                continue;
            };
            let dir = bucket_dir.join(&name);
            match native_object::load_validated(inner, id).and_then(|meta| {
                native_object::verify_digest(inner, id, &meta.sha256).map(|()| meta)
            }) {
                Ok(mut meta) => {
                    project_expiry(&mut meta);
                    entries.push(meta);
                    if entries.len() == limit {
                        return Ok(SpoolPage {
                            entries,
                            next: Some(encode_cursor(id)?),
                        });
                    }
                }
                // 尚未发布（writing 残留）：不可见。
                Err(SpoolError::NotFound) => continue,
                // 损坏或隔离域不符：持锁清理后显式失败；被其他 owner 持锁或条目已被并发
                // 删除则跳过，正常清理竞争不升级为整页扫描失败。
                Err(error) => {
                    match try_lock_file(&inner.object_lock_path(id))
                        .map_err(|error| io_error("lock spool object", error))?
                    {
                        LockAttempt::Held | LockAttempt::Gone => continue,
                        LockAttempt::Free(_lock) => {
                            native_object::remove_object(inner, id, &dir)?;
                            warn!(
                                object = %id_hex(id),
                                reason = %error,
                                "telemetry spool discarded unreadable entry"
                            );
                            return Err(error);
                        }
                    }
                }
            }
        }
        bucket += 1;
        bound = None;
    }
    Ok(SpoolPage {
        entries,
        next: None,
    })
}

/// 重建期统计：累计已发布对象的实际占用与个数，并持锁清理结构非法的条目。
///
/// 只在 domain 锁临界区内调用（[`super::native_quota::rebuild`]），因此不能经
/// `native_object::remove_object` 删除（那会再次取 domain 锁互等）；账本由本次重建以磁盘为准
/// 统一结算。摘要在启动期不重算，由 `claim`/`list` 读取时核对。
pub(crate) fn scan_totals(inner: &NativeInner) -> SpoolResult<(u64, u64)> {
    let mut stored = 0u64;
    let mut objects = 0u64;
    // 桶号以 u16 计数：`OBJECT_BUCKETS as u8` 会把 256 截断成 0，导致统计与清理被跳过。
    for bucket in 0..OBJECT_BUCKETS {
        let bucket_dir = inner.bucket_dir(bucket as u8);
        for name in bucket_names(&bucket_dir)? {
            let dir = bucket_dir.join(&name);
            let Some(id) = parse_id(&name) else {
                continue;
            };
            if !dir.is_dir() {
                continue;
            }
            match native_object::load_validated(inner, id) {
                Ok(_) => {
                    stored = stored.saturating_add(accounted_bytes(&dir).unwrap_or(0));
                    objects = objects.saturating_add(1);
                }
                // 尚未发布（writing 残留）：不可见也不计账，由发布路径或启动清扫处理。
                Err(SpoolError::NotFound) => continue,
                Err(error) => {
                    discard_entry(inner, id, &dir)?;
                    warn!(
                        object = %id_hex(id),
                        reason = %error,
                        "telemetry spool discarded unreadable entry at startup"
                    );
                }
            }
        }
    }
    Ok((stored, objects))
}

/// 重建期持锁删除条目；账本由本次重建统一结算，不做扣减。
fn discard_entry(inner: &NativeInner, id: ObjectId, dir: &Path) -> SpoolResult<()> {
    let _lock = match try_lock_file(&inner.object_lock_path(id))
        .map_err(|error| io_error("lock spool object", error))?
    {
        // 其他 owner 持锁：交由其处理，不抢删；条目已被并发删除：没有残留可清。
        LockAttempt::Held | LockAttempt::Gone => return Ok(()),
        LockAttempt::Free(lock) => lock,
    };
    match fs::remove_dir_all(dir) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(io_error("remove spool residue", error)),
    }
}

/// 桶内对象目录名（十六进制 ID）升序；单桶枚举有界，不做全树收集。
pub(crate) fn bucket_names(bucket_dir: &Path) -> SpoolResult<Vec<String>> {
    let mut names = Vec::new();
    let entries = match fs::read_dir(bucket_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(names),
        Err(error) => return Err(io_error("scan spool bucket", error)),
    };
    for entry in entries {
        let entry = entry.map_err(|error| io_error("scan spool bucket", error))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if parse_id(&name).is_some() {
            names.push(name);
        }
    }
    names.sort_unstable();
    Ok(names)
}

fn encode_cursor(id: ObjectId) -> SpoolResult<PageCursor> {
    PageCursor::try_from_bytes(id_hex(id).into_bytes())
}

fn decode_cursor(bytes: &[u8]) -> SpoolResult<(u8, Option<String>)> {
    let text = std::str::from_utf8(bytes).map_err(|_| SpoolError::InvalidMetadata)?;
    let id = parse_id(text).ok_or(SpoolError::InvalidMetadata)?;
    Ok((NativeInner::bucket_of(id), Some(text.to_string())))
}
