//! 历史页消费者——HISTORY_PAGES_TX channel → `session/load`（带 cursor 的历史页）。
//!
//! ## 设计
//!
//! - 触发侧（message_area 渲染路径）在同步边界先认领在途请求（`HISTORY_PAGING`），
//!   再入队；相同 cursor + 相同 limit 的并发触发在认领处即被合并（REQ-C01-2）。
//! - 单消费者顺序读取：一次只发一个页 RPC——历史页是只读浏览，与正在执行的 turn
//!   并行，但不需要并发抢答。每页 RPC 有 [`PAGE_FETCH_TIMEOUT`] 预算，服务端静默
//!   时按普通失败结算并释放在途认领（transport 自身没有内建超时）。
//! - 响应**注入桥接任务**（`LOCAL_EVENT_TX`）而不是在消费者里直接改 `committed`：
//!   窗口是桥接任务的独占状态，合并与发布必须在同一条串行链上。
//! - 翻页 loading 与执行状态分离：本消费者不碰 `ACP_STATE`、`LOADING_EPOCH`、
//!   submit/cancel 通道，也不建立或结束 turn（REQ-C01-4）。
//!
//! ## 丢弃规则（REQ-C01-5、A11）
//!
//! 响应回来时先按「会话 + 快照身份」核对当前浏览：
//!
//! - 切了会话/`/clear`/重新 load → 状态被清空 → 身份不匹配 → 丢弃，不入队；
//! - 同一会话 rewind 后旧范围 → 服务端回 -32012，客户端同时按快照身份自判丢弃；
//! - 桥接侧还有一层会话过滤（事件携带的 session_id 与 active 不符即丢），两层独立。

pub mod window;

#[cfg(test)]
#[path = "history_page_consumer_test.rs"]
mod tests;

#[cfg(test)]
#[path = "history_page_consumer/window_test.rs"]
mod window_tests;

#[cfg(test)]
use crate::acp_client::AcpTuiClient;
#[cfg(test)]
use crate::kit::acp_types::AcpEventData;
#[cfg(test)]
use crate::kit::atoms;
#[cfg(test)]
use crate::kit::history_paging;

mod consumer;
pub use consumer::*;

pub(crate) use window::*;
