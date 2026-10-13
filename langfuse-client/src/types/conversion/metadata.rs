use super::{LangfuseError, OtlpSink};
use crate::stable_json::StableJson;

/// metadata 展开：先写命名空间属性（整份紧凑 JSON），再逐字段展开一层。
///
/// 与旧物化基线逐字节一致：字符串字段取原文本，其余字段取 `Value::to_string()` 的紧凑 JSON 文本；
/// 两者都写成 `stringValue`。大值同样边序列化边转义，不先构造中间串。
///
/// 键序由 [`StableJson`] 固定为字典序：整份 JSON 文本与展开属性都按排序键输出，两层不会互相
/// 矛盾，也不随构建期 `serde_json` feature 漂移。
pub(super) fn append_metadata_attrs(
    sink: &mut OtlpSink<'_>,
    namespace: &str,
    metadata: &serde_json::Value,
) -> Result<(), LangfuseError> {
    sink.attr_json(namespace, &StableJson(metadata))?;
    if let Some(fields) = metadata.as_object() {
        let mut keys: Vec<&String> = fields.keys().collect();
        keys.sort_unstable();
        for key in keys {
            if let Some(value) = fields.get(key) {
                let key = format!("{namespace}.{key}");
                match value.as_str() {
                    Some(text) => sink.attr_str(&key, text)?,
                    None => sink.attr_json(&key, &StableJson(value))?,
                }
            }
        }
    }
    Ok(())
}
