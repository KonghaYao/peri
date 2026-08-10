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
/// `current` 为 Session Doc `sessions` Map 的当前投影（**key = map 真实 key**，
/// 见 [`read_current`]）；`incoming` 为一次 `session_list` 响应条目。
///
/// **per-cwd 全量同步**（workspace 扩展，§6.3）：sessions 是 instance 级
/// 数据但按 cwd 分面——不同 workspace 目录的会话互不相交。upsert 按
/// (session_id, cwd) 对匹配；remove 按 cwd 隔离：仅删除「与 incoming 同 cwd
/// 但不在响应中」的条目，跨 cwd 条目不受影响；cwd 为空串的条目（历史遗留/
/// 孤儿数据）随任意一次轮询删除（§6.3 自愈），非空 cwd 但暂无轮询响应面的
/// 条目保留（无活跃对话即无响应面，硬约束；误删会使历史列表显示为空）。
pub fn diff(
    current: &HashMap<String, SessionSummaryProjection>,
    incoming: &[SessionSummaryProjection],
) -> SessionListDiff {
    let mut upsert = Vec::new();
    let mut remove = Vec::new();

    // incoming 按 cwd 分组（全量同步的响应面）。
    let mut by_cwd: std::collections::HashMap<&str, Vec<&SessionSummaryProjection>> =
        std::collections::HashMap::new();
    for e in incoming {
        by_cwd.entry(e.cwd.as_str()).or_default().push(e);
    }

    for (key, cur) in current {
        // 孤儿 key（map key ≠ 内部 session_id，历史遗留写入）：无条件删除——
        // 正常条目在 upsert 侧以 session_id 为 key 重建；孤儿 key 永存只会
        // 造成渲染层重复条目（§6.3 自愈）。不参与 per-cwd 匹配：其内部
        // session_id/cwd 可能恰好与响应条目相同而误判为「在响应中」。
        if key != &cur.session_id {
            remove.push(key.clone());
            continue;
        }
        match by_cwd.get(cur.cwd.as_str()) {
            Some(list) => {
                // 该 cwd 有轮询响应：全量同步——cur 不在响应中 → 删除。
                if !list.iter().any(|e| e.session_id == cur.session_id) {
                    remove.push(key.clone());
                }
            }
            None => {
                // cur 的 cwd 不在任何轮询响应面：
                // - cwd 为空串（历史遗留/孤儿数据）→ 过期，删除（§6.3
                //   自愈——正常条目必然带 cwd）；
                // - cwd 非空（其它 workspace 的会话，当前无活跃对话可
                //   轮询）→ 保留。轮询是硬约束（无 ACP 进程即无响应面），
                //   误删会让该 workspace 的历史列表在恢复活跃前显示为空。
                if cur.cwd.is_empty() {
                    remove.push(key.clone());
                }
            }
        }
    }

    // upsert：incoming 中与 current 同 (key, cwd) 且内容不同，或不存在。
    // （key 即 session_id 的正常条目按 session_id 匹配；孤儿 key 不匹配 →
    // 重建正常条目。）
    for e in incoming {
        match current.get(&e.session_id) {
            Some(existing) if existing == e => {
                // 无变化：no-op。
            }
            _ => upsert.push(e.clone()),
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

/// 读取 Session Doc `sessions` 当前投影（**key = map 真实 key**，非内部
/// session_id）。
///
/// 供聚合器在 `SessionListResponse` 判定/应用前收集当前状态；只读。
/// 以真实 key 为投影键：孤儿条目（key ≠ 内部 session_id，历史遗留写入）也
/// 进入投影，由 [`diff`] 的 remove 按真实 key 收集、全量同步时删除。
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
                cwd: str_or("cwd").unwrap_or_default(),
                bound_chat_id: None,
            };
            out.insert(key.to_string(), entry);
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
    m.insert(txn, "cwd", entry.cwd.clone());
}
