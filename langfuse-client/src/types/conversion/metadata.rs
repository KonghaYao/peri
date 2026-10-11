use super::OtelAttribute;

pub(super) fn append_metadata_attrs(
    attrs: &mut Vec<OtelAttribute>,
    namespace: &str,
    metadata: &serde_json::Value,
) {
    attrs.push(OtelAttribute::string(namespace, metadata.to_string()));
    if let Some(fields) = metadata.as_object() {
        for (key, value) in fields {
            let value = match value.as_str() {
                Some(text) => text.to_owned(),
                None => value.to_string(),
            };
            attrs.push(OtelAttribute::string(format!("{namespace}.{key}"), value));
        }
    }
}
