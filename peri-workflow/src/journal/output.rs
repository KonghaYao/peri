//! JSON output extraction commits a reference only after its backing file is written.
use super::WorkflowJournalStore;
use tracing::warn;

/// 递归遍历 JSON Value，将长度超过 threshold 的字符串写入 outputs/ 目录，
/// 仅写入成功后替换为 "${label}" 占位符；失败保留原文。返回成功提取的标签列表。
pub fn extract_long_texts(
    value: &mut serde_json::Value,
    run_id: &str,
    store: &WorkflowJournalStore,
    threshold: usize,
) -> Vec<String> {
    let mut extracted = Vec::new();
    extract_long_texts_inner(value, run_id, store, threshold, "", &mut extracted);
    extracted
}

fn extract_long_texts_inner(
    value: &mut serde_json::Value,
    run_id: &str,
    store: &WorkflowJournalStore,
    threshold: usize,
    key_hint: &str,
    extracted: &mut Vec<String>,
) {
    match value {
        serde_json::Value::Object(map) => {
            let keys: Vec<String> = map.keys().cloned().collect();
            for key in keys {
                let child_hint = if key_hint.is_empty() {
                    key.clone()
                } else {
                    format!("{}.{}", key_hint, key)
                };
                let child = map.get_mut(&key).unwrap();
                if let serde_json::Value::String(s) = child {
                    if s.len() > threshold {
                        let label = child_hint;
                        if let Err(e) = store.write_output(run_id, &label, s) {
                            warn!(target: "workflow", run_id = %run_id, label = %label, error = %e, "write_output failed");
                        } else {
                            extracted.push(label.clone());
                            *child = serde_json::Value::String(format!("${{{}}}", label));
                        }
                    }
                } else {
                    extract_long_texts_inner(
                        child,
                        run_id,
                        store,
                        threshold,
                        &child_hint,
                        extracted,
                    );
                }
            }
        }
        serde_json::Value::Array(arr) => {
            for (i, item) in arr.iter_mut().enumerate() {
                let child_hint = if key_hint.is_empty() {
                    format!("[{}]", i)
                } else {
                    format!("{}[{}]", key_hint, i)
                };
                extract_long_texts_inner(item, run_id, store, threshold, &child_hint, extracted);
            }
        }
        _ => {}
    }
}
