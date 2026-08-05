//! 固定 UTF-8 canonical transcript 与上下文构造（r2-encrypted-transfer v1）。
//!
//! 所有签名 bytes、握手 prologue 与 AEAD 上下文都经由这里以完全确定的
//! `peri-sync/v1|op|field...|unix_seconds` 形式构造；二进制字段必须先经
//! [`b64url_nopad`] 编码。字段顺序即契约，Slice 2 的 TS Worker 必须逐字节复刻。
//!
//! 本模块是纯构造层，不依赖任何密码学原语；非法输入（空 op、空字段、含 `|`
//! 的片段）一律以错误返回而非 panic——构造层不 panic 是 API 契约。

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;

use anyhow::Result;

/// 协议版本标签，固定为 `peri-sync/v1`。
pub const PROTOCOL_TAG: &str = "peri-sync/v1";

/// 二进制字段编码：base64url，无 padding。
pub fn b64url_nopad(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

/// 组装 `peri-sync/v1|op[|field...][|tail]`。
///
/// 校验（错误返回，不 panic）：
/// - `op` 非空且不含 `|`；
/// - 每个字段非空且不含 `|`（base64url / Crockford / 十进制字符集均不含 `|`；
///   空字段会与“无字段”产生歧义，一律拒绝）；
/// - `tail` 不含 `|`（可为空，表示无 tail）。
///
/// arity 约束：每个 op 的字段数量是契约的一部分（Slice 2 TS Worker 逐字节
/// 复刻），调用方必须为同一 op 始终传相同数量的字段——末尾 tail（时间戳）与
/// 最后一个字段在文本上不可区分，arity 固定是消除歧义的唯一保证。本层只
/// 校验片段合法性，不校验具体 op 的 arity（那是各调用点的静态契约）。
fn join(op: &str, fields: &[&str], tail: &str) -> Result<String> {
    if op.is_empty() || op.contains('|') {
        anyhow::bail!("op must be non-empty and must not contain '|': {op:?}");
    }
    for field in fields {
        if field.is_empty() {
            anyhow::bail!("transcript field must not be empty");
        }
        if field.contains('|') {
            anyhow::bail!("transcript field must not contain '|': {field:?}");
        }
    }
    if tail.contains('|') {
        anyhow::bail!("transcript tail must not contain '|': {tail:?}");
    }
    // `|` 仅作为非空片段间的分隔符：无字段时 `peri-sync/v1|op`，无 tail 时
    // 也不产生尾随 `|`。格式因此始终是 `peri-sync/v1|op[|field...][|tail]`。
    let mut out = String::with_capacity(PROTOCOL_TAG.len() + op.len() + 16);
    out.push_str(PROTOCOL_TAG);
    out.push('|');
    out.push_str(op);
    if !fields.is_empty() {
        out.push('|');
        out.push_str(&fields.join("|"));
    }
    if !tail.is_empty() {
        out.push('|');
        out.push_str(tail);
    }
    Ok(out)
}

/// 带时间戳的签名 transcript：`peri-sync/v1|op|field...|unix_seconds`。
///
/// 时间戳为十进制 unix 秒，由调用方提供（一般为当前墙钟）。arity 固定：
/// 每个 op 的字段数量是契约的一部分（Slice 2 TS Worker 逐字节复刻），调用方
/// 必须为同一 op 始终传相同数量的字段；字段必须非空且不含 `|`。
pub fn transcript(op: &str, fields: &[&str], unix_secs: u64) -> Result<String> {
    join(op, fields, &unix_secs.to_string())
}

/// 无时间戳的上下文：`peri-sync/v1|op|field...`。
///
/// 用于握手 prologue、AEAD 上下文等不需要时钟的绑定场景。字段约束与
/// [`transcript`] 相同。
pub fn context(op: &str, fields: &[&str]) -> Result<String> {
    join(op, fields, "")
}

/// 检查签名时间戳是否在 `±skew_secs` 内（服务端接受 ±300 秒）。
pub fn timestamp_within_skew(ts: u64, now_secs: u64, skew_secs: u64) -> bool {
    now_secs.abs_diff(ts) <= skew_secs
}
