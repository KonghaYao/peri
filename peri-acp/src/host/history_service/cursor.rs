//! 服务层 opaque cursor 与完整性保护（设计 §12.3.4）。
//!
//! wire 形状为 `v2|{mac_hex}|{window_id}|{before|after}|{raw_cursor}`：
//! - `mac` 是 `SHA-256(cursor_secret || "v2|{window_id}|{direction}|{raw}")` 的前
//!   16 字节小写 hex；
//! - `raw` 由后端解释，可以包含 `|`，因此只按前四个分隔符切分。
//!
//! 解码顺序固定为：格式 → MAC（常量时间）→ windowId → direction。服务层只把 raw
//! 交给后端；窗口身份与方向由服务层校验。cursor 只是位置提示，绝不作为授权依据。

use peri_acp_types::history_v2::HistoryDirection;
use sha2::{Digest, Sha256};

const PREFIX: &str = "v2";
const MAC_BYTES: usize = 16;
const MAC_HEX_LEN: usize = MAC_BYTES * 2;

pub(super) struct DecodedCursor {
    pub(super) direction: HistoryDirection,
    pub(super) raw: String,
}

#[derive(Debug)]
pub(super) enum CursorError {
    /// 形状错误、MAC 不符或方向不符。
    Invalid,
    /// cursor 属于另一个窗口；不能跨窗口复用。
    CrossWindow,
}

pub(super) fn encode(
    secret: &[u8; 32],
    window_id: &str,
    direction: HistoryDirection,
    raw: &str,
) -> String {
    let direction = direction_name(direction);
    let mac = mac_hex(secret, window_id, direction, raw);
    format!("{PREFIX}|{mac}|{window_id}|{direction}|{raw}")
}

/// 解析服务层 cursor；`raw` 中允许出现 `|`，只按前四个分隔符切分。
///
/// 校验顺序不可交换：格式与 MAC 失败一律 `Invalid`，只有 MAC 有效、身份自洽但
/// 窗口不匹配时才返回 `CrossWindow`。这样客户端不能通过改 windowId 把错误类型
/// 变成可利用的跨窗口提示。
pub(super) fn decode(
    secret: &[u8; 32],
    window_id: &str,
    value: &str,
) -> Result<DecodedCursor, CursorError> {
    let mut parts = value.splitn(5, '|');
    if parts.next() != Some(PREFIX) {
        return Err(CursorError::Invalid);
    }
    let mac = parts.next().ok_or(CursorError::Invalid)?;
    let owner = parts.next().ok_or(CursorError::Invalid)?;
    let direction = parts.next().ok_or(CursorError::Invalid)?;
    let raw = parts.next().ok_or(CursorError::Invalid)?;
    if mac.len() != MAC_HEX_LEN || !mac.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(CursorError::Invalid);
    }
    let direction = match direction {
        "before" => HistoryDirection::Before,
        "after" => HistoryDirection::After,
        _ => return Err(CursorError::Invalid),
    };
    let expected = mac_hex(secret, owner, direction_name(direction), raw);
    if !constant_time_eq(mac.as_bytes(), expected.as_bytes()) {
        return Err(CursorError::Invalid);
    }
    if owner != window_id {
        return Err(CursorError::CrossWindow);
    }
    Ok(DecodedCursor {
        direction,
        raw: raw.to_owned(),
    })
}

fn direction_name(direction: HistoryDirection) -> &'static str {
    match direction {
        HistoryDirection::Before => "before",
        HistoryDirection::After => "after",
    }
}

fn mac_hex(secret: &[u8; 32], window_id: &str, direction: &str, raw: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(secret);
    hasher.update(format!("{PREFIX}|{window_id}|{direction}|{raw}").as_bytes());
    let digest = hasher.finalize();
    hex(&digest[..MAC_BYTES])
}

fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

/// 定长比较；长度差异只泄露 MAC 编码长度，不泄露 secret 或逐字节匹配位置。
fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut diff = 0u8;
    for (a, b) in left.iter().zip(right) {
        diff |= a ^ b;
    }
    diff == 0
}
