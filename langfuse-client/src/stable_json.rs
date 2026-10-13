//! 键序稳定化的 JSON 序列化（契约 §3.4）。
//!
//! `serde_json::Value` 的对象底层随构建期 feature 变化：启用 `preserve_order` 时是保插入序的
//! `IndexMap`，否则是按键排序的 `BTreeMap`。workspace 全量构建会经 `agent-client-protocol`
//! 启用该 feature，单独构建 `langfuse-client` 不会——同一份事件因此可能编出不同字节。冻结格式
//! 要求逐字节可复现，所以进入 OTLP 正文与暂存冻结正文的 JSON 一律经本模块序列化：对象键序由
//! 这里显式决定（字典序，与未启用该 feature 的 `serde_json::Value` 默认输出一致），不依赖
//! 编译图的 feature 并集。
//!
//! 事件的 map 字段是 `HashMap`，迭代序不受 `Ord` 约束（每次进程都不同），不能直接交给 serde；
//! [`StableMap`] 按键排序输出，map 的值再经 [`Stable`] 递归稳定化。

use std::collections::HashMap;

use serde::ser::{SerializeMap, SerializeSeq};
use serde::{Serialize, Serializer};
use serde_json::Value;

/// 键按字典序输出的 JSON 值视图；对象递归排序，数组保持原有顺序。
pub(crate) struct StableJson<'a>(pub(crate) &'a Value);

/// 键按字典序输出的 map 视图；值与 [`StableJson`] 共用同一递归口径。
pub(crate) struct StableMap<'a, V: Stable>(pub(crate) &'a HashMap<String, V>);

/// 可稳定序列化的 JSON 值：`Value` 递归排序；标量与字符串没有键序问题，直接透传。
pub(crate) trait Stable {
    fn stable<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error>;
}

impl Stable for Value {
    fn stable<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Value::Null => serializer.serialize_unit(),
            Value::Bool(value) => serializer.serialize_bool(*value),
            Value::Number(value) => value.serialize(serializer),
            Value::String(value) => serializer.serialize_str(value),
            Value::Array(items) => {
                let mut seq = serializer.serialize_seq(Some(items.len()))?;
                for item in items {
                    seq.serialize_element(&StableJson(item))?;
                }
                seq.end()
            }
            Value::Object(fields) => {
                let mut keys: Vec<&str> = fields.keys().map(String::as_str).collect();
                keys.sort_unstable();
                let mut map = serializer.serialize_map(Some(keys.len()))?;
                for key in keys {
                    if let Some(field) = fields.get(key) {
                        map.serialize_entry(key, &StableJson(field))?;
                    }
                }
                map.end()
            }
        }
    }
}

impl Stable for i32 {
    fn stable<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_i32(*self)
    }
}

impl Stable for f64 {
    fn stable<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_f64(*self)
    }
}

impl Serialize for StableJson<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.stable(serializer)
    }
}

impl<V: Stable> Serialize for StableMap<'_, V> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut keys: Vec<&str> = self.0.keys().map(String::as_str).collect();
        keys.sort_unstable();
        let mut map = serializer.serialize_map(Some(keys.len()))?;
        for key in keys {
            if let Some(value) = self.0.get(key) {
                map.serialize_entry(key, &StableEntry(value))?;
            }
        }
        map.end()
    }
}

/// `Stable` 值的 `Serialize` 适配器；map 条目只能传 `&T: Serialize`，这里补上桥接。
struct StableEntry<'a, V>(&'a V);

impl<V: Stable> Serialize for StableEntry<'_, V> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.stable(serializer)
    }
}

#[cfg(test)]
#[path = "stable_json_test.rs"]
mod tests;
