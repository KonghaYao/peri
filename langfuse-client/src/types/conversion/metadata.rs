use super::{LangfuseError, OtlpSink};

/// metadata 展开：先写命名空间属性（整份紧凑 JSON），再逐字段展开一层。
///
/// 与旧物化基线逐字节一致：字符串字段取原文本，其余字段取 `Value::to_string()` 的紧凑 JSON 文本；
/// 两者都写成 `stringValue`。大值同样边序列化边转义，不先构造中间串。
pub(super) fn append_metadata_attrs(
    sink: &mut OtlpSink<'_>,
    namespace: &str,
    metadata: &serde_json::Value,
) -> Result<(), LangfuseError> {
    sink.attr_json(namespace, metadata)?;
    if let Some(fields) = metadata.as_object() {
        for (key, value) in fields {
            let key = format!("{namespace}.{key}");
            match value.as_str() {
                Some(text) => sink.attr_str(&key, text)?,
                None => sink.attr_json(&key, value)?,
            }
        }
    }
    Ok(())
}
