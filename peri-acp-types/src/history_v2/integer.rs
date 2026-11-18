//! History V2 的大整数 wire 约定（设计 I08）。
//!
//! 所有 `u64` 身份/序号在 JSON 中一律写成十进制字符串，避免跨语言整数精度损失。
//! 只接受规范形式：`"0"` 或无前导零的正整数；空串、前导零、正负号、小数点、
//! 非 ASCII 数字与溢出都显式失败。

use std::fmt;
use std::str::FromStr;

use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::error::HistoryContractError;

/// 规范十进制字符串编码的 `u64`。
///
/// 序列化固定为 JSON string；反序列化拒绝 JSON number 与非规范字符串。构造后即保证
/// 文本形式唯一，可直接用于 wire 比较与去重。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DecimalU64(u64);

impl DecimalU64 {
    /// 零值。
    pub const ZERO: Self = Self(0);

    /// 由原生整数构造。
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// 取回原生整数。
    pub const fn get(self) -> u64 {
        self.0
    }

    /// 是否为零。
    pub const fn is_zero(self) -> bool {
        self.0 == 0
    }
}

impl From<u64> for DecimalU64 {
    fn from(value: u64) -> Self {
        Self::new(value)
    }
}

impl fmt::Display for DecimalU64 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.0)
    }
}

impl FromStr for DecimalU64 {
    type Err = HistoryContractError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let bytes = value.as_bytes();
        if bytes.is_empty() {
            return Err(HistoryContractError::invalid_request(
                "decimal u64 must not be empty",
            ));
        }
        if bytes.len() > 1 && bytes[0] == b'0' {
            return Err(HistoryContractError::invalid_request(
                "decimal u64 must not have leading zeros",
            ));
        }
        if !bytes.iter().all(u8::is_ascii_digit) {
            return Err(HistoryContractError::invalid_request(
                "decimal u64 must contain only ASCII digits",
            ));
        }
        value
            .parse::<u64>()
            .map(Self)
            .map_err(|_| HistoryContractError::invalid_request("decimal u64 is out of range"))
    }
}

impl TryFrom<&str> for DecimalU64 {
    type Error = HistoryContractError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl Serialize for DecimalU64 {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.0.to_string())
    }
}

impl<'de> Deserialize<'de> for DecimalU64 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_str(DecimalU64Visitor)
    }
}

struct DecimalU64Visitor;

impl Visitor<'_> for DecimalU64Visitor {
    type Value = DecimalU64;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a canonical decimal u64 string")
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        value.parse().map_err(E::custom)
    }
}
