//! 客户端侧限额与 TTL 常量（r2-encrypted-transfer contract）。
//!
//! 本模块是客户端预算检查的事实源；Slice 2 的 TS Worker 必须镜像同一组数值
//! （channel TTL、限流、签名偏差与 payload 限额均已在 plan 中冻结）。

use anyhow::Result;

/// 单个 payload part 上限（64 KiB，**密文**口径；TS `maxPartBytes` 同值）。
pub const MAX_PART_BYTES: usize = 64 * 1024;

/// 单个明文分片上限：`MAX_PART_BYTES - ENVELOPE_HEADER_LEN - AEAD_TAG_LEN`
/// （= 65507）。seal 后每片密文恰为 `MAX_PART_BYTES`，与 TS channel-do 的
/// `ct.length > maxPartBytes → 413` 逐字节对齐（C1 复审修复）。
pub const MAX_PLAINTEXT_PART_BYTES: usize =
    MAX_PART_BYTES - crate::sync::crypto::ENVELOPE_HEADER_LEN - crate::sync::crypto::AEAD_TAG_LEN;

/// 每 channel 最大 part 数。
pub const MAX_PARTS_PER_CHANNEL: usize = 512;

/// 每 channel payload 总预算（32 MiB = 64 KiB × 512，L2 修订；与 TS
/// `side-projects/peri-sync/server/src/limits.ts` 的 `MAX_PAYLOAD_BYTES` 一致）。
pub const MAX_PAYLOAD_BYTES: usize = 32 * 1024 * 1024;

/// manifest 上限（256 KiB）。
pub const MAX_MANIFEST_BYTES: usize = 256 * 1024;

/// 设备名最大字符数。
pub const MAX_DEVICE_NAME_CHARS: usize = 64;

/// 签名时间戳可接受偏差（±300 秒，服务端侧）。
pub const SIGNATURE_SKEW_SECS: u64 = 300;

/// channel TTL：created 起 +10 分钟。
pub const TTL_CREATED_SECS: u64 = 600;
/// channel TTL：join 成功/握手进行中 +5 分钟。
pub const TTL_JOINED_SECS: u64 = 300;
/// channel TTL：ready 起 +1 小时。
pub const TTL_READY_SECS: u64 = 3600;
/// 终态 tombstone +1 小时。
pub const TTL_TOMBSTONE_SECS: u64 = 3600;

/// 正常 sender 码注册频率上限（次/分钟）。
pub const CODE_REGISTER_MAX_PER_MIN: u32 = 2;
/// 撞码最多一次额外重试。
pub const CODE_COLLISION_MAX_RETRIES: u32 = 1;
/// code lookup 限流（次/分钟）；避免成功路径被错误 429。
pub const CODE_LOOKUP_RATE_LIMIT_PER_MIN: u32 = 15;

/// 客户端指数退避基准（毫秒）。
pub const BASE_BACKOFF_MS: u64 = 500;
/// 客户端指数退避上限（毫秒）。
pub const MAX_BACKOFF_MS: u64 = 30_000;

/// 校验 manifest 预算（part 数与**密文**总字节），在 R2 PUT 前检查。
///
/// `total_bytes` 为各 part envelope 长度之和，与 TS channel-do 的
/// `total_bytes + ct.length > maxPayloadBytes → 413` 累计口径一致（C1）。
pub fn validate_manifest(parts: usize, total_bytes: usize) -> Result<()> {
    if parts > MAX_PARTS_PER_CHANNEL {
        anyhow::bail!("manifest exceeds part limit: {parts} > {MAX_PARTS_PER_CHANNEL}");
    }
    if total_bytes > MAX_PAYLOAD_BYTES {
        anyhow::bail!("manifest exceeds payload budget: {total_bytes} > {MAX_PAYLOAD_BYTES}");
    }
    Ok(())
}

/// 校验单个 part 大小。
pub fn validate_part_size(part_bytes: usize) -> Result<()> {
    if part_bytes > MAX_PART_BYTES {
        anyhow::bail!("part exceeds size limit: {part_bytes} > {MAX_PART_BYTES}");
    }
    Ok(())
}

/// 校验设备名（非空、不超过 [`MAX_DEVICE_NAME_CHARS`] 字符）。
pub fn validate_device_name(name: &str) -> Result<()> {
    if name.is_empty() {
        anyhow::bail!("device name must not be empty");
    }
    let chars = name.chars().count();
    if chars > MAX_DEVICE_NAME_CHARS {
        anyhow::bail!("device name too long: {chars} chars > {MAX_DEVICE_NAME_CHARS}");
    }
    Ok(())
}
