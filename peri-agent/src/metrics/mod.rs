//! 指标事件出口。
//!
//! 指标只经宿主装配注入的 [`MetricsSink`] 上报（生产出口是 Langfuse event）；
//! 未注入出口（未配置或不可用 Langfuse）时事件直接丢弃——不落盘、不做本地缓冲、
//! 不建双写兼容层。
//!
//! [不变量] `emit` 不阻塞调用方，也不产生任何文件系统写入。

use std::sync::Arc;

use chrono::Utc;
use parking_lot::RwLock;

/// 字符串截断上限（字符级，CJK 安全）
const TRUNCATE_LIMIT: usize = 500;

/// 指标事件（出口契约）。
#[derive(Debug, Clone)]
pub struct MetricEvent {
    /// ISO 8601 毫秒时间戳
    pub ts: String,
    /// session_id
    pub sid: Option<String>,
    /// run_id（当前 ReAct 循环标识）
    pub rid: Option<String>,
    /// 事件名（点分层级）
    pub event: String,
    /// 事件附加数据
    pub data: serde_json::Value,
}

/// 指标出口。由宿主装配注入，实现必须立即返回（fire-and-forget）且不得 panic。
pub trait MetricsSink: Send + Sync {
    /// 接收一个事件；投递失败由实现自行警告，不回传调用方。
    fn record(&self, event: MetricEvent);
}

/// 进程级出口槽。宿主装配注入一次；`None` 表示未配置（事件丢弃）。
static SINK: RwLock<Option<Arc<dyn MetricsSink>>> = RwLock::new(None);

/// 安装/替换指标出口；`None` 清除出口。
///
/// 生产装配点在 ACP host（`peri-acp/src/host/assemble.rs`），且只在 Langfuse
/// 可用时安装；未配置 Langfuse 的部署保持未安装。
pub fn set_sink(sink: Option<Arc<dyn MetricsSink>>) {
    *SINK.write() = sink;
}

/// 发射一个指标事件。fire-and-forget，不阻塞调用方。
///
/// `data` 中所有字符串值会被截断到 500 字符。未安装出口时事件被丢弃。
pub fn emit(event: &str, data: serde_json::Value, sid: Option<&str>, rid: Option<&str>) {
    let Some(sink) = SINK.read().clone() else {
        return;
    };
    sink.record(MetricEvent {
        ts: Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        sid: sid.map(str::to_owned),
        rid: rid.map(str::to_owned),
        event: event.to_owned(),
        data: truncate_json_strings(data),
    });
}

/// 获取当前进程 RSS（MB），通过 sysinfo 获取实时值。
#[doc(hidden)]
pub fn current_rss_mb() -> Option<u64> {
    #[cfg(unix)]
    {
        use sysinfo::{ProcessesToUpdate, System};
        let mut sys = System::new();
        let pid = sysinfo::get_current_pid().ok()?;
        sys.refresh_processes(ProcessesToUpdate::Some(&[pid]), true);
        sys.process(pid).map(|p| p.memory() / 1024) // sysinfo 返回 KB → MB
    }
    #[cfg(not(unix))]
    {
        None
    }
}

/// 获取系统总物理内存（MB），跨平台
#[doc(hidden)]
pub fn total_system_memory_mb() -> Option<u64> {
    let sys = sysinfo::System::new_all();
    Some(sys.total_memory() / (1024 * 1024))
}

/// 递归截断 JSON 中所有字符串值到 TRUNCATE_LIMIT 字符
fn truncate_json_strings(val: serde_json::Value) -> serde_json::Value {
    match val {
        serde_json::Value::String(s) => {
            if s.chars().count() > TRUNCATE_LIMIT {
                serde_json::Value::String(s.chars().take(TRUNCATE_LIMIT).collect())
            } else {
                serde_json::Value::String(s)
            }
        }
        serde_json::Value::Object(map) => {
            let new_map: serde_json::Map<String, serde_json::Value> = map
                .into_iter()
                .map(|(k, v)| (k, truncate_json_strings(v)))
                .collect();
            serde_json::Value::Object(new_map)
        }
        serde_json::Value::Array(arr) => {
            serde_json::Value::Array(arr.into_iter().map(truncate_json_strings).collect())
        }
        other => other,
    }
}

#[cfg(test)]
#[path = "mod_test.rs"]
mod tests;
