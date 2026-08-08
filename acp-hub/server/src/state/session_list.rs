//! `session_list` 响应全量同步投影（§6.3：幂等，10s 轮询；响应中不存在的旧
//! 条目删除——自愈）。

use std::collections::HashMap;

use yrs::{Map, ReadTxn};

use acp_hub_proto::schema::SessionSummaryProjection;

use crate::state::view_store::TransactionCtx;

/// `session_list` diff 结果。
#[derive(Debug, Clone, PartialEq)]
pub struct SessionListDiff {
    /// 需 upsert 的条目（新条目或字段变化；与既有完全相同的条目不进 diff——
    /// 无变化 no-op，§6.3 幂等）。
    pub upsert: Vec<SessionSummaryProjection>,
    /// 现存 key 中不在响应内的（§6.3 旧条目删除）。
    pub remove: Vec<String>,
}

/// 纯函数 diff（不触碰 doc；可单测）。
///
/// `current` 为 Session Doc `sessions` Map 的当前投影（key = session_id）；
/// `incoming` 为 `session_list` 响应条目。
pub fn diff(
    current: &HashMap<String, SessionSummaryProjection>,
    incoming: &[SessionSummaryProjection],
) -> SessionListDiff {
    let mut upsert = Vec::new();
    let mut remove = Vec::new();

    for entry in incoming {
        match current.get(&entry.session_id) {
            Some(existing) if existing == entry => {
                // 无变化：no-op。
            }
            _ => upsert.push(entry.clone()),
        }
    }
    let incoming_ids: std::collections::HashSet<&str> =
        incoming.iter().map(|e| e.session_id.as_str()).collect();
    for id in current.keys() {
        if !incoming_ids.contains(id.as_str()) {
            remove.push(id.clone());
        }
    }
    SessionListDiff { upsert, remove }
}

/// 应用 diff 到 Session Doc `sessions`（Y.Map 写；upsert 覆盖、remove 删键）。
///
/// 由聚合器在收到 `SessionListResponse` 时经本原语调用（session doc 事务内）。
pub fn apply_diff(txn: &mut TransactionCtx<'_>, root: &yrs::MapRef, diff: &SessionListDiff) {
    let sessions = root.get_or_init::<_, yrs::MapRef>(txn, "sessions");
    for entry in &diff.upsert {
        write_summary(txn, &sessions, entry);
    }
    for id in &diff.remove {
        sessions.remove(txn, id);
    }
}

/// 读取 Session Doc `sessions` 当前投影（key = session_id）。
///
/// 供聚合器在 `SessionListResponse` 判定/应用前收集当前状态；只读。
pub fn read_current<T: ReadTxn>(
    txn: &T,
    root: &yrs::MapRef,
) -> HashMap<String, SessionSummaryProjection> {
    let mut out = HashMap::new();
    let Some(sessions) = root
        .get(txn, "sessions")
        .and_then(|v| v.cast::<yrs::MapRef>().ok())
    else {
        return out;
    };
    for (key, v) in sessions.iter(txn) {
        if let Ok(m) = v.cast::<yrs::MapRef>() {
            let str_or = |k: &str| -> Option<String> {
                m.get(txn, k).and_then(|x| x.cast::<String>().ok())
            };
            let entry = SessionSummaryProjection {
                session_id: str_or("session_id").unwrap_or_else(|| key.to_string()),
                title: str_or("title").unwrap_or_default(),
                status: str_or("status").unwrap_or_default(),
                updated_at: str_or("updated_at").unwrap_or_default(),
            };
            out.insert(entry.session_id.clone(), entry);
        }
    }
    out
}

fn write_summary(
    txn: &mut TransactionCtx<'_>,
    sessions: &yrs::MapRef,
    entry: &SessionSummaryProjection,
) {
    let m = sessions.get_or_init::<_, yrs::MapRef>(txn, entry.session_id.as_str());
    m.insert(txn, "session_id", entry.session_id.clone());
    m.insert(txn, "title", entry.title.clone());
    m.insert(txn, "status", entry.status.clone());
    m.insert(txn, "updated_at", entry.updated_at.clone());
}
