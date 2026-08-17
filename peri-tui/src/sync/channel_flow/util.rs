use std::fmt::Write as _;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;

use crate::sync::device::DeviceId;
use crate::sync::http_client;
use crate::sync::keystore::SecretStore;

// ─── 工具 ──────────────────────────────────────────────────────────────────

pub(super) fn random_channel_id() -> Result<String> {
    Ok(DeviceId::random()?.to_b64())
}

pub(super) fn b64url(bytes: &[u8]) -> String {
    http_client::b64url(bytes)
}

pub(super) fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub(super) fn sha256_bytes(data: &[u8]) -> [u8; 32] {
    let digest = ring::digest::digest(&ring::digest::SHA256, data);
    let mut out = [0u8; 32];
    out.copy_from_slice(digest.as_ref());
    out
}

/// channel 的日志表示：SHA-256 前 8 hex（不泄露完整 channel ID）。
pub(super) fn channel_hash(channel_id: &str) -> String {
    let h = sha256_bytes(channel_id.as_bytes());
    let mut s = String::with_capacity(8);
    for b in &h[..4] {
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// 构造 `Authorization: PeriSig ...` 头；`now` 为当前墙钟（unix 秒）。
///
/// H1 复审修复：重发/轮询每次请求前以当前时间重新构造（刷新签名时间戳，
/// 服务端 ±300s 偏差窗口内永不因等待而过期）。
pub(super) fn auth_header_at(
    now: u64,
    store: &dyn SecretStore,
    device_id: &DeviceId,
    op: &str,
    fields: &[&str],
) -> Result<String> {
    http_client::peri_sig_header(store, device_id, op, fields, now)
}
