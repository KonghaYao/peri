//! S5.4 窗口请求/结算状态机：open/page/seek、单在途 + 单待合并、迟到页丢弃。
//!
//! 只有 `HISTORY_WINDOW_V1` 开关与连接能力同时为真时启用；否则旧 `HISTORY_PAGING`
//! 路径保持不变。桥接任务拥有 `HistoryWindow`，本模块只维护客户端侧请求身份、预留账目
//! 与在途/待合并状态。

//! 触发侧（滚动/gap 恢复）在 S5.4/S5.6 接线前由单元测试覆盖；保留未使用告警豁免。
#[path = "window_triggers.rs"]
mod triggers;

use crate::kit::atoms;
use crate::kit::history_paging::{
    self, WINDOW_DEFAULT_BYTE_BUDGET, WINDOW_PAGE_LIMIT, WindowDirection, WindowPageCursor,
};

mod boundary;
mod window_consumer;
pub use window_consumer::*;

pub use triggers::*;
