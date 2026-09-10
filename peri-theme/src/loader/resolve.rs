//! 不依赖文件系统的 JSON 展平、引用解析与颜色解析。

use std::collections::{HashMap, HashSet};

use ratatui::style::Color;

use super::ThemeLoadError;

pub(super) const MAX_DEPTH: usize = 10;

/// 展平嵌套对象和数组，保留键的原始大小写。
pub(super) fn flatten_json_obj(
    prefix: &str,
    value: &serde_json::Value,
    flat: &mut HashMap<String, String>,
) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, value) in map {
                let key = if prefix.is_empty() {
                    key.clone()
                } else {
                    format!("{prefix}.{key}")
                };
                flatten_json_obj(&key, value, flat);
            }
        }
        serde_json::Value::Array(array) => {
            for (index, value) in array.iter().enumerate() {
                flatten_json_obj(&format!("{prefix}.{index}"), value, flat);
            }
        }
        serde_json::Value::String(value) => {
            flat.insert(prefix.to_string(), value.clone());
        }
        serde_json::Value::Number(value) => {
            flat.insert(prefix.to_string(), value.to_string());
        }
        serde_json::Value::Bool(value) => {
            flat.insert(prefix.to_string(), value.to_string());
        }
        serde_json::Value::Null => {}
    }
}

/// 每个根键独立跟踪活动链；共享的依赖键不是循环。
pub(super) fn resolve_refs(
    flat: &HashMap<String, String>,
    depth: usize,
) -> Result<HashMap<String, String>, ThemeLoadError> {
    if depth > MAX_DEPTH {
        return Err(ThemeLoadError::CircularRef(
            "max reference depth exceeded".to_string(),
        ));
    }
    flat.iter()
        .map(|(key, value)| {
            let value = if let Some(path) = value.strip_prefix('$') {
                let mut active = HashSet::from([key.as_str()]);
                resolve_ref_value(path, flat, &mut active, depth + 1)?
            } else {
                value.clone()
            };
            Ok((key.clone(), value))
        })
        .collect()
}

fn resolve_ref_value<'a>(
    path: &str,
    flat: &'a HashMap<String, String>,
    active: &mut HashSet<&'a str>,
    depth: usize,
) -> Result<String, ThemeLoadError> {
    if depth > MAX_DEPTH {
        return Err(ThemeLoadError::CircularRef(format!(
            "max reference depth exceeded resolving: {path}"
        )));
    }
    let (key, value) = flat
        .get_key_value(path)
        .or_else(|| {
            let lower_path = path.to_lowercase();
            flat.iter()
                .find(|(key, _)| key.to_lowercase() == lower_path)
        })
        .ok_or_else(|| ThemeLoadError::UnresolvedRef(path.to_string()))?;
    // 用实际键而非引用拼写检测循环，兼容既有不区分大小写的查找。
    if !active.insert(key.as_str()) {
        return Err(ThemeLoadError::CircularRef(format!(
            "circular reference at key: {key}"
        )));
    }
    let resolved = match value.strip_prefix('$') {
        Some(next) => resolve_ref_value(next, flat, active, depth + 1),
        None => Ok(value.clone()),
    };
    active.remove(key.as_str());
    resolved
}

/// 只接受 ASCII `#RGB` 或 `#RRGGBB`，先校验字节再切片。
pub(super) fn parse_hex_color(value: &str) -> Result<Color, ThemeLoadError> {
    let value = value.trim();
    let bytes = value.as_bytes();
    if !matches!(bytes.len(), 4 | 7)
        || bytes.first() != Some(&b'#')
        || !bytes[1..].iter().all(u8::is_ascii_hexdigit)
    {
        return Err(ThemeLoadError::InvalidColor(if value.is_empty() {
            "empty color string".to_string()
        } else {
            value.to_string()
        }));
    }
    let width = if bytes.len() == 7 { 2 } else { 1 };
    let channel = |index| {
        u8::from_str_radix(&value[index..index + width], 16)
            .map(|value| if width == 1 { value * 17 } else { value })
            .map_err(|_| ThemeLoadError::InvalidColor(value.to_string()))
    };
    Ok(Color::Rgb(
        channel(1)?,
        channel(1 + width)?,
        channel(1 + width * 2)?,
    ))
}
