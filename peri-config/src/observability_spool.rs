//! 遥测 spool、内容编码与内存预算新键的解析与校验（契约 §6）。
//!
//! 新键逐键选择胜出来源：具名环境变量优先于 `langfuse.*` settings，两者都缺省时保留
//! [`LangfuseConfig`] 的默认值。胜出值非法即返回结构化 [`LangfuseConfigError`]，既不
//! 隐退回退到默认值，也不发布新配置快照；旧键的忽略/clamp 语义仍由
//! [`super::resolve`] 保持，不经过本模块。
//!
//! 跨键约束（阈值/块大小与预算的关系、并发上限）由 `langfuse-client` 的
//! `TelemetryConfig::validate` 在构造期一次校验，本模块只做逐键规则。

use std::collections::BTreeMap;

use serde_json::Value;

use super::{ConfigValueSource, LangfuseCompression, LangfuseConfig, LangfuseConfigError};

/// 新键在两类来源中的名字；错误只报告胜出来源的名字。
#[derive(Clone, Copy)]
pub(super) struct Key {
    pub(super) environment: &'static str,
    pub(super) settings: &'static str,
}

impl Key {
    const fn new(environment: &'static str, settings: &'static str) -> Self {
        Self {
            environment,
            settings,
        }
    }
}

/// `langfuse.spool_enabled`：磁盘暂存开关。
pub(super) const SPOOL_ENABLED: Key = Key::new("LANGFUSE_SPOOL_ENABLED", "spool_enabled");
/// `langfuse.compression`：上报正文内容编码。
pub(super) const COMPRESSION: Key = Key::new("LANGFUSE_COMPRESSION", "compression");
/// `langfuse.input_spool_threshold_bytes`：input 落盘阈值。
pub(super) const INPUT_SPOOL_THRESHOLD_BYTES: Key = Key::new(
    "LANGFUSE_INPUT_SPOOL_THRESHOLD_BYTES",
    "input_spool_threshold_bytes",
);
/// `langfuse.payload_memory_threshold_bytes`：导出正文落盘阈值。
pub(super) const PAYLOAD_MEMORY_THRESHOLD_BYTES: Key = Key::new(
    "LANGFUSE_PAYLOAD_MEMORY_THRESHOLD_BYTES",
    "payload_memory_threshold_bytes",
);
/// `langfuse.memory_budget_bytes`：遥测内存总预算。
pub(super) const MEMORY_BUDGET_BYTES: Key =
    Key::new("LANGFUSE_MEMORY_BUDGET_BYTES", "memory_budget_bytes");
/// `langfuse.prepare_max_in_flight`：同时准备的编码任务数。
pub(super) const PREPARE_MAX_IN_FLIGHT: Key =
    Key::new("LANGFUSE_PREPARE_MAX_IN_FLIGHT", "prepare_max_in_flight");
/// `langfuse.stream_chunk_bytes`：编码读写块大小。
pub(super) const STREAM_CHUNK_BYTES: Key =
    Key::new("LANGFUSE_STREAM_CHUNK_BYTES", "stream_chunk_bytes");
/// `langfuse.spool_quota_bytes`：暂存磁盘配额。
pub(super) const SPOOL_QUOTA_BYTES: Key =
    Key::new("LANGFUSE_SPOOL_QUOTA_BYTES", "spool_quota_bytes");
/// `langfuse.spool_ttl_secs`：暂存对象存活秒数。
pub(super) const SPOOL_TTL_SECS: Key = Key::new("LANGFUSE_SPOOL_TTL_SECS", "spool_ttl_secs");
/// `langfuse.max_wire_bytes`：压缩后 wire 正文字节上限。
pub(super) const MAX_WIRE_BYTES: Key = Key::new("LANGFUSE_MAX_WIRE_BYTES", "max_wire_bytes");
/// `langfuse.shutdown_grace_secs`：关闭宽限秒数。
pub(super) const SHUTDOWN_GRACE_SECS: Key =
    Key::new("LANGFUSE_SHUTDOWN_GRACE_SECS", "shutdown_grace_secs");

const REASON_BOOLEAN: &str = "must be a boolean (true/false/1/0/on/off)";
const REASON_COMPRESSION: &str = "must be gzip or none";
const REASON_NONZERO_INTEGER: &str = "must be a nonzero decimal integer";
const REASON_RANGE: &str = "exceeds the supported integer range";

/// 胜出来源携带的原始值：环境是字符串，settings 是 JSON 值。
#[derive(Clone, Copy)]
enum Raw<'a> {
    Text(&'a str),
    Json(&'a Value),
}

/// 逐键选择胜出来源；环境存在该键时不再回看 settings。
fn winning<'a>(
    environment: &'a BTreeMap<String, String>,
    settings: Option<&'a Value>,
    key: Key,
) -> Option<(ConfigValueSource, &'static str, Raw<'a>)> {
    if let Some(value) = environment.get(key.environment) {
        return Some((
            ConfigValueSource::Environment,
            key.environment,
            Raw::Text(value),
        ));
    }
    settings
        .and_then(|settings| settings.get(key.settings))
        .map(|value| (ConfigValueSource::Settings, key.settings, Raw::Json(value)))
}

fn invalid(
    key: &'static str,
    origin: ConfigValueSource,
    reason: &'static str,
) -> LangfuseConfigError {
    LangfuseConfigError::Invalid {
        key,
        origin,
        reason,
    }
}

fn parse_bool(
    key: &'static str,
    origin: ConfigValueSource,
    raw: Raw<'_>,
) -> Result<bool, LangfuseConfigError> {
    match raw {
        Raw::Text(text) => match text.trim().to_ascii_lowercase().as_str() {
            "true" | "1" | "on" => Ok(true),
            "false" | "0" | "off" => Ok(false),
            _ => Err(invalid(key, origin, REASON_BOOLEAN)),
        },
        Raw::Json(value) => value
            .as_bool()
            .ok_or_else(|| invalid(key, origin, REASON_BOOLEAN)),
    }
}

fn parse_compression(
    key: &'static str,
    origin: ConfigValueSource,
    raw: Raw<'_>,
) -> Result<LangfuseCompression, LangfuseConfigError> {
    match raw {
        Raw::Text(text) => match text.trim().to_ascii_lowercase().as_str() {
            "gzip" => Ok(LangfuseCompression::Gzip),
            "none" => Ok(LangfuseCompression::None),
            _ => Err(invalid(key, origin, REASON_COMPRESSION)),
        },
        Raw::Json(value) => match value.as_str().map(str::trim) {
            Some(text) if text.eq_ignore_ascii_case("gzip") => Ok(LangfuseCompression::Gzip),
            Some(text) if text.eq_ignore_ascii_case("none") => Ok(LangfuseCompression::None),
            _ => Err(invalid(key, origin, REASON_COMPRESSION)),
        },
    }
}

fn parse_u64(
    key: &'static str,
    origin: ConfigValueSource,
    raw: Raw<'_>,
) -> Result<u64, LangfuseConfigError> {
    let value = match raw {
        // 只接受十进制数字：负号、正号、小数点与空白都会在此被拒。
        Raw::Text(text) => {
            let text = text.trim();
            if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
                return Err(invalid(key, origin, REASON_NONZERO_INTEGER));
            }
            text.parse::<u64>()
                .map_err(|_| invalid(key, origin, REASON_RANGE))?
        }
        Raw::Json(value) => value
            .as_u64()
            .ok_or_else(|| invalid(key, origin, REASON_NONZERO_INTEGER))?,
    };
    if value == 0 {
        return Err(invalid(key, origin, REASON_NONZERO_INTEGER));
    }
    Ok(value)
}

fn parse_usize(
    key: &'static str,
    origin: ConfigValueSource,
    raw: Raw<'_>,
) -> Result<usize, LangfuseConfigError> {
    let value = parse_u64(key, origin, raw)?;
    usize::try_from(value).map_err(|_| invalid(key, origin, REASON_RANGE))
}

/// 新键的默认值由 [`LangfuseConfig::default`] 提供；仅在胜出来源存在时改写。
fn resolve_bool(
    environment: &BTreeMap<String, String>,
    settings: Option<&Value>,
    key: Key,
    default: bool,
) -> Result<bool, LangfuseConfigError> {
    match winning(environment, settings, key) {
        Some((origin, name, raw)) => parse_bool(name, origin, raw),
        None => Ok(default),
    }
}

fn resolve_compression(
    environment: &BTreeMap<String, String>,
    settings: Option<&Value>,
    key: Key,
    default: LangfuseCompression,
) -> Result<LangfuseCompression, LangfuseConfigError> {
    match winning(environment, settings, key) {
        Some((origin, name, raw)) => parse_compression(name, origin, raw),
        None => Ok(default),
    }
}

fn resolve_usize(
    environment: &BTreeMap<String, String>,
    settings: Option<&Value>,
    key: Key,
    default: usize,
) -> Result<usize, LangfuseConfigError> {
    match winning(environment, settings, key) {
        Some((origin, name, raw)) => parse_usize(name, origin, raw),
        None => Ok(default),
    }
}

fn resolve_u64(
    environment: &BTreeMap<String, String>,
    settings: Option<&Value>,
    key: Key,
    default: u64,
) -> Result<u64, LangfuseConfigError> {
    match winning(environment, settings, key) {
        Some((origin, name, raw)) => parse_u64(name, origin, raw),
        None => Ok(default),
    }
}

/// 解析 §6 的 11 个新键并写入 `config`。
///
/// `settings` 是 `langfuse` 子对象（缺省或不是 JSON 对象时按缺省处理）。任一胜出值
/// 非法时整个解析失败，调用方按来源拒绝该配置快照，不保留部分结果。
pub(super) fn resolve_into(
    config: &mut LangfuseConfig,
    settings: Option<&Value>,
    environment: &BTreeMap<String, String>,
) -> Result<(), LangfuseConfigError> {
    config.spool_enabled =
        resolve_bool(environment, settings, SPOOL_ENABLED, config.spool_enabled)?;
    config.compression =
        resolve_compression(environment, settings, COMPRESSION, config.compression)?;
    config.input_spool_threshold_bytes = resolve_usize(
        environment,
        settings,
        INPUT_SPOOL_THRESHOLD_BYTES,
        config.input_spool_threshold_bytes,
    )?;
    config.payload_memory_threshold_bytes = resolve_usize(
        environment,
        settings,
        PAYLOAD_MEMORY_THRESHOLD_BYTES,
        config.payload_memory_threshold_bytes,
    )?;
    config.memory_budget_bytes = resolve_usize(
        environment,
        settings,
        MEMORY_BUDGET_BYTES,
        config.memory_budget_bytes,
    )?;
    config.prepare_max_in_flight = resolve_usize(
        environment,
        settings,
        PREPARE_MAX_IN_FLIGHT,
        config.prepare_max_in_flight,
    )?;
    config.stream_chunk_bytes = resolve_usize(
        environment,
        settings,
        STREAM_CHUNK_BYTES,
        config.stream_chunk_bytes,
    )?;
    config.spool_quota_bytes = resolve_u64(
        environment,
        settings,
        SPOOL_QUOTA_BYTES,
        config.spool_quota_bytes,
    )?;
    config.spool_ttl_secs =
        resolve_u64(environment, settings, SPOOL_TTL_SECS, config.spool_ttl_secs)?;
    config.max_wire_bytes =
        resolve_usize(environment, settings, MAX_WIRE_BYTES, config.max_wire_bytes)?;
    config.shutdown_grace_secs = resolve_u64(
        environment,
        settings,
        SHUTDOWN_GRACE_SECS,
        config.shutdown_grace_secs,
    )?;
    Ok(())
}

#[cfg(test)]
#[path = "observability_spool_test.rs"]
mod tests;
