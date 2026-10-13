//! 键序稳定化的回归用例：默认构建下 `serde_json::Value` 已按键排序，所以这里既覆盖实现本身，
//! 也作为 `preserve_order` 构建（workspace 全量构建）下的真实判据——那时 Value 保插入序、
//! `HashMap` 迭代序随机，只有显式排序才会得到这些期望字节。

use std::collections::HashMap;

use serde_json::json;

use super::{StableJson, StableMap};

#[test]
fn stable_map_sorts_keys_despite_hash_order() {
    let mut cost = HashMap::new();
    cost.insert("zebra".to_string(), 3.5f64);
    cost.insert("alpha".to_string(), 0.0001f64);
    cost.insert("middle".to_string(), 1.0f64);
    let text = serde_json::to_string(&StableMap(&cost)).expect("stable map");
    assert_eq!(text, r#"{"alpha":0.0001,"middle":1.0,"zebra":3.5}"#);
}

#[test]
fn stable_map_recursively_sorts_value_fields() {
    let mut params = HashMap::new();
    params.insert(
        "response_format".to_string(),
        json!({"type": "json_object", "schema": {"b": 2, "a": 1}}),
    );
    params.insert("temperature".to_string(), json!(0.7));
    let text = serde_json::to_string(&StableMap(&params)).expect("stable map");
    assert_eq!(
        text,
        r#"{"response_format":{"schema":{"a":1,"b":2},"type":"json_object"},"temperature":0.7}"#
    );
}

#[test]
fn stable_json_sorts_nested_objects_and_keeps_array_order() {
    let value = json!({"z": {"b": 2, "a": 1}, "a": [{"d": 4, "c": 3}, 7, "文本"]});
    let text = serde_json::to_string(&StableJson(&value)).expect("stable json");
    assert_eq!(text, r#"{"a":[{"c":3,"d":4},7,"文本"],"z":{"a":1,"b":2}}"#);
}

#[test]
fn stable_json_matches_scalar_baseline() {
    let value = json!([null, true, false, 3, -1.5, "a\"b", {"k": 1}]);
    let text = serde_json::to_string(&StableJson(&value)).expect("stable json");
    assert_eq!(text, r#"[null,true,false,3,-1.5,"a\"b",{"k":1}]"#);
}

#[test]
fn stable_json_handles_empty_containers() {
    assert_eq!(
        serde_json::to_string(&StableJson(&json!({}))).expect("empty object"),
        "{}"
    );
    assert_eq!(
        serde_json::to_string(&StableJson(&json!([]))).expect("empty array"),
        "[]"
    );
    let empty: HashMap<String, i32> = HashMap::new();
    assert_eq!(
        serde_json::to_string(&StableMap(&empty)).expect("empty map"),
        "{}"
    );
}
