//! JSON output extraction commits a reference only after its backing file is written.
use super::WorkflowJournalStore;
use std::collections::HashSet;
use tracing::warn;

/// label → `outputs/<label>.txt` 文件名的**单一权威**。
///
/// 规则：路径分隔符与控制字符换成 `_`；连续点折叠为 `._`（消除 `..` 形态）；
/// 空 label 回落 `unnamed`；其余字符（含中文、`[`/`]`）原样保留。
///
/// 为什么不允许在写文件时静默改名：占位符 `${label}` 与文件名必须同源。曾经由
/// `WorkflowJournalStore::write_output` 把非法 label 静默改写成 `unnamed`，导致
/// state.json 的 `${label}` 指向不存在的文件（引用断裂），多个非法 label 还会
/// 互相覆盖（2026-10-11 issue）。
fn sanitize_label(json_path: &str) -> String {
    let mut sanitized = String::with_capacity(json_path.len());
    for character in json_path.chars() {
        // 两类替换效果相同但理由不同：分隔符/控制字符不能出现在文件名里；
        // 连续点是路径遍历形态（`..`），折叠成 `._`。
        let illegal = character == '/'
            || character == '\\'
            || character.is_control()
            || (character == '.' && sanitized.ends_with('.'));
        if illegal {
            sanitized.push('_');
        } else {
            sanitized.push(character);
        }
    }
    if sanitized.is_empty() {
        sanitized.push_str("unnamed");
    }
    sanitized
}

/// 分配本次提取内唯一的输出 label：`sanitize_label` + 重名追加 `-2`、`-3`…。
///
/// 返回值同时用作占位符 `${label}` 与文件名 `outputs/<label>.txt`，因此「引用可
/// 解析」由构造保证，读侧不需要猜或兜底。
pub fn output_label(json_path: &str, used: &mut HashSet<String>) -> String {
    let base = sanitize_label(json_path);
    if used.insert(base.clone()) {
        return base;
    }
    let mut index = 2;
    loop {
        let candidate = format!("{base}-{index}");
        if used.insert(candidate.clone()) {
            return candidate;
        }
        index += 1;
    }
}

/// 递归遍历 JSON Value，将长度超过 threshold 的字符串写入 outputs/ 目录，
/// 仅写入成功后替换为 "${label}" 占位符；失败保留原文。返回成功提取的标签列表。
pub fn extract_long_texts(
    value: &mut serde_json::Value,
    run_id: &str,
    store: &WorkflowJournalStore,
    threshold: usize,
) -> Vec<String> {
    let mut extracted = Vec::new();
    let mut used = HashSet::new();
    extract_long_texts_inner(
        value,
        run_id,
        store,
        threshold,
        "",
        &mut used,
        &mut extracted,
    );
    extracted
}

#[allow(clippy::too_many_arguments)]
fn extract_long_texts_inner(
    value: &mut serde_json::Value,
    run_id: &str,
    store: &WorkflowJournalStore,
    threshold: usize,
    key_hint: &str,
    used: &mut HashSet<String>,
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
                        // 占位符与文件名取同一 label（去重后），两者不可能再漂移。
                        let label = output_label(&child_hint, used);
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
                        used,
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
                extract_long_texts_inner(
                    item,
                    run_id,
                    store,
                    threshold,
                    &child_hint,
                    used,
                    extracted,
                );
            }
        }
        _ => {}
    }
}
