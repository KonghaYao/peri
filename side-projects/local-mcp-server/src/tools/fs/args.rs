//! 工具参数解析（逐字复刻源实现的校验与错误文案）。
//!
//! 事实源：
//! - `peri-middlewares/src/tools/filesystem/read.rs:35`（`parse_line_number`）
//! - `peri-middlewares/src/tools/mod.rs:24`（`parse_optional_u64`）
//!
//! 两者都刻意**不接受**浮点与负值：`as_u64()` 会把 `1.5` 静默变成 `None`（退回默认），
//! 让模型读到错误位置；这里保留源实现的显式报错。

use serde_json::Value;

/// 解析 1-based 行号/行数参数。
///
/// 缺失（`null`）返回 `default`；显式传入非正整数（0、负数、小数、非数字）报错，文案与源
/// 实现逐字一致。
pub fn parse_line_number(value: &Value, name: &str, default: usize) -> Result<usize, String> {
    if value.is_null() {
        return Ok(default);
    }
    let number = value
        .as_f64()
        .ok_or_else(|| format!("Error: '{name}' must be a positive integer, got {value}"))?;
    if number.fract() != 0.0 || number < 1.0 {
        return Err(format!(
            "Error: '{name}' must be a positive integer (1-based line number), got {number}"
        ));
    }
    Ok(number as usize)
}

/// 解析可选的非负整数（`folder_operations.max_depth`）。
pub fn parse_optional_u64(value: &Value, name: &str) -> Result<Option<u64>, String> {
    if value.is_null() {
        return Ok(None);
    }
    let number = value
        .as_f64()
        .ok_or_else(|| format!("Error: '{name}' must be a non-negative integer, got {value}"))?;
    if number.fract() != 0.0 || number < 0.0 {
        return Err(format!(
            "Error: '{name}' must be a non-negative integer, got {number}"
        ));
    }
    Ok(Some(number as u64))
}

/// 取字符串参数（源实现 `<value>.as_str()`）。
pub fn required_str<'a>(
    arguments: &'a Value,
    field: &str,
    message: &str,
) -> Result<&'a str, String> {
    arguments[field].as_str().ok_or_else(|| message.to_string())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn test_line_number_defaults_and_errors_match_source() {
        assert_eq!(parse_line_number(&json!(null), "offset", 1).unwrap(), 1);
        assert_eq!(parse_line_number(&json!(7), "offset", 1).unwrap(), 7);
        assert_eq!(
            parse_line_number(&json!(0), "limit", 2000).unwrap_err(),
            "Error: 'limit' must be a positive integer (1-based line number), got 0"
        );
        assert_eq!(
            parse_line_number(&json!(1.5), "offset", 1).unwrap_err(),
            "Error: 'offset' must be a positive integer (1-based line number), got 1.5"
        );
        assert_eq!(
            parse_line_number(&json!("3"), "offset", 1).unwrap_err(),
            "Error: 'offset' must be a positive integer, got \"3\""
        );
    }

    #[test]
    fn test_optional_u64_matches_source() {
        assert_eq!(parse_optional_u64(&json!(null), "max_depth").unwrap(), None);
        assert_eq!(
            parse_optional_u64(&json!(11), "max_depth").unwrap(),
            Some(11)
        );
        assert_eq!(
            parse_optional_u64(&json!(-1), "max_depth").unwrap_err(),
            "Error: 'max_depth' must be a non-negative integer, got -1"
        );
        assert_eq!(
            parse_optional_u64(&json!(2.5), "max_depth").unwrap_err(),
            "Error: 'max_depth' must be a non-negative integer, got 2.5"
        );
    }
}
