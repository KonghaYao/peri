//! 时长字符串解析（`docs/plans/f2-auth-config.md` §3.1【决策】）。
//!
//! toml/CLI 中的 Duration 使用可读字符串（`"500ms"`/`"5s"`/`"16ms"`/`"24h"`/
//! `"90d"`，支持 `ns/us/ms/s/m/h/d` 后缀），经 serde 自定义反序列化落地——
//! §16 表格即此形态。非法字符串 → 启动错误，不静默取默认。

use std::time::Duration;

use serde::{de, Deserialize, Deserializer, Serializer};

/// 时长解析错误。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DurationParseError {
    /// 输入不是「数字 + 后缀」形态（含无后缀、空串、负号、非数字）。
    #[error("时长格式非法（需数字+后缀 ns/us/ms/s/m/h/d）: {0:?}")]
    Format(String),
    /// 数值乘后缀后超出 u64 纳秒范围。
    #[error("时长数值溢出: {0:?}")]
    Overflow(String),
}

/// 解析时长字符串（`"500ms"`/`"5s"`/`"24h"`/`"90d"` 等）。
///
/// 支持后缀：`ns`/`us`/`ms`/`s`/`m`/`h`/`d`。数字与后缀之间不允许空白；
/// 非法后缀/无后缀/溢出均报错（fail-fast，不静默取默认）。
pub fn parse_duration(input: &str) -> Result<Duration, DurationParseError> {
    let s = input.trim();
    let split = s
        .char_indices()
        .find(|(_, c)| !c.is_ascii_digit())
        .map(|(i, _)| i)
        .unwrap_or(s.len());
    if split == 0 {
        return Err(DurationParseError::Format(s.to_string()));
    }
    // 数字段已过滤非数字字符；parse 失败 = 超出 u64 范围 → Overflow。
    let num: u64 = s[..split]
        .parse()
        .map_err(|_| DurationParseError::Overflow(s.to_string()))?;
    let suffix = &s[split..];
    let nanos = match suffix {
        "ns" => num,
        "us" => num
            .checked_mul(1_000)
            .ok_or_else(|| DurationParseError::Overflow(s.to_string()))?,
        "ms" => num
            .checked_mul(1_000_000)
            .ok_or_else(|| DurationParseError::Overflow(s.to_string()))?,
        "s" => num
            .checked_mul(1_000_000_000)
            .ok_or_else(|| DurationParseError::Overflow(s.to_string()))?,
        "m" => num
            .checked_mul(60 * 1_000_000_000)
            .ok_or_else(|| DurationParseError::Overflow(s.to_string()))?,
        "h" => num
            .checked_mul(3600 * 1_000_000_000)
            .ok_or_else(|| DurationParseError::Overflow(s.to_string()))?,
        "d" => num
            .checked_mul(86400 * 1_000_000_000)
            .ok_or_else(|| DurationParseError::Overflow(s.to_string()))?,
        _ => return Err(DurationParseError::Format(s.to_string())),
    };
    Ok(Duration::from_nanos(nanos))
}

/// 把 [`Duration`] 格式化为可读字符串（`parse_duration` 的逆操作，供
/// round-trip 与调试输出）。取能整除的最大单位。
pub fn format_duration(d: Duration) -> String {
    let ns = d.as_nanos();
    const UNITS: &[(u128, &str)] = &[
        (86_400_000_000_000, "d"),
        (3_600_000_000_000, "h"),
        (60_000_000_000, "m"),
        (1_000_000_000, "s"),
        (1_000_000, "ms"),
        (1_000, "us"),
        (1, "ns"),
    ];
    for (unit, suffix) in UNITS {
        if ns.is_multiple_of(*unit) {
            return format!("{}{}", ns / unit, suffix);
        }
    }
    // `1` 恒整除，理论不可达。
    unreachable!("ns unit always divides")
}

/// serde 反序列化：toml 字段为 `Option<Duration>`，值为字符串。
pub fn deserialize_opt_duration<'de, D>(
    d: D,
) -> Result<Option<Duration>, D::Error>
where
    D: Deserializer<'de>,
{
    let s = Option::<String>::deserialize(d)?;
    s.map(|v| parse_duration(&v).map_err(de::Error::custom))
        .transpose()
}

/// serde 序列化：`Option<Duration>` → 字符串（或 null）。
pub fn serialize_opt_duration<S>(v: &Option<Duration>, s: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    match v {
        Some(d) => s.serialize_str(&format_duration(*d)),
        None => s.serialize_none(),
    }
}
