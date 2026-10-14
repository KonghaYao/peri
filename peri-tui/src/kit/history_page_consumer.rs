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

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::acp_client::AcpTuiClient;
use crate::kit::acp_types::{AcpEventData, AcpEventWithEpoch};
use crate::kit::atoms;
use crate::kit::history_paging::{
    self, HistoryPageResponse, InFlightPage, MAX_AUTO_CONTINUE_PAGES,
};

/// 一条历史页请求（触发侧冻结的浏览身份）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryPageRequest {
    pub session_id: String,
    pub snapshot_token: String,
    pub cursor: String,
    pub limit: usize,
}

impl HistoryPageRequest {
    /// 与本请求对应的在途身份（结算时用它判定响应是否仍属于当前浏览）。
    ///
    /// 身份只由页参数构成——触发侧认领时写的 `InFlightPage` 与这里构造的必须相等
    /// （同一页的判据就是这四个字段，见 [`crate::kit::history_paging::InFlightPage`]）。
    pub(crate) fn identity(&self) -> InFlightPage {
        InFlightPage {
            session_id: self.session_id.clone(),
            snapshot_token: self.snapshot_token.clone(),
            cursor: self.cursor.clone(),
            limit: self.limit,
        }
    }
}

/// 并发合并判定：同会话、同快照、同 cursor、同 limit = 同一页（只发一次）。
pub(crate) fn is_duplicate_of(request: &HistoryPageRequest, in_flight: &InFlightPage) -> bool {
    request.session_id == in_flight.session_id
        && request.snapshot_token == in_flight.snapshot_token
        && request.cursor == in_flight.cursor
        && request.limit == in_flight.limit
}

/// 取页失败。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FetchError {
    message: String,
    /// 服务端明确说这次浏览的范围/会话已经不是当前的了（-32011 / -32012）。
    invalidates: bool,
}

/// 单页 RPC 预算。
///
/// transport 明确「连接沉默没有内建超时」（`peri-acp/src/transport/mod.rs`），而
/// `in_flight` 认领只在结算时释放：没有这条预算，服务端一次静默就会让认领永久占用，
/// 该会话的翻页彻底停滞（不崩、不越界，但再也读不到更早历史）。
///
/// 超时按**普通失败**结算（`invalidates: false`）：释放认领、只记原因、保留游标——
/// 服务端沉默不代表浏览范围失效，退避（`REQUEST_BACKOFF_MS`）之后仍可重试。
/// 丢弃等待中的 future 会同时释放 transport 侧的 pending 注册，不留悬挂等待。
const PAGE_FETCH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// 拉取一页（必要时连续跳过空页，上限 [`MAX_AUTO_CONTINUE_PAGES`]）。
pub(crate) async fn fetch_page(
    client: &AcpTuiClient,
    request: &HistoryPageRequest,
) -> Result<HistoryPageResponse, FetchError> {
    let mut cursor = request.cursor.clone();
    let mut carried: Option<HistoryPageResponse> = None;
    for _ in 0..MAX_AUTO_CONTINUE_PAGES {
        let raw = peri_time::timeout(
            PAGE_FETCH_TIMEOUT,
            client.load_history_page(&request.session_id, &cursor, request.limit),
        )
        .await
        .map_err(|_| FetchError {
            message: format!("history page RPC timed out after {PAGE_FETCH_TIMEOUT:?}"),
            invalidates: false,
        })?
        .map_err(|error| FetchError {
            // -32011 跨会话 / -32012 范围失效：浏览身份不再有效，停止翻页。
            invalidates: matches!(error.code, -32011 | -32012),
            message: error.to_string(),
        })?;
        let response = history_paging::decode_page_response(&raw).ok_or_else(|| FetchError {
            message: "history page response has an unexpected shape".to_string(),
            invalidates: false,
        })?;
        if response.snapshot_token != request.snapshot_token {
            // 快照身份在游标链中途变了：这不是同一次浏览，丢弃。
            return Err(FetchError {
                message: "history page snapshot token changed mid-chain".to_string(),
                invalidates: true,
            });
        }
        let Some(next_cursor) = response.next_cursor.clone() else {
            return Ok(join_pages(carried, response, None));
        };
        let empty = response.items.is_empty();
        carried = Some(join_pages(carried, response, Some(next_cursor.clone())));
        if !empty {
            return Ok(carried.expect("carried page"));
        }
        // 空页 + 前进的游标是合法结果（一整段不可展示条目）：继续往前读。
        cursor = next_cursor;
    }
    Ok(carried.expect("at least one page fetched"))
}

/// 把连续拉取的多页并成一个响应（游标取最后一页的下一页游标）。
fn join_pages(
    carried: Option<HistoryPageResponse>,
    page: HistoryPageResponse,
    next_cursor: Option<String>,
) -> HistoryPageResponse {
    match carried {
        None => HistoryPageResponse {
            session_id: page.session_id,
            snapshot_token: page.snapshot_token,
            next_cursor,
            items: page.items,
        },
        Some(mut previous) => {
            previous.items.extend(page.items);
            previous.next_cursor = next_cursor;
            previous
        }
    }
}

/// 启动历史页消费者。
pub fn spawn_history_page_consumer(
    client: AcpTuiClient,
    mut rx: mpsc::UnboundedReceiver<HistoryPageRequest>,
    shutdown: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        info!("kit history_page_consumer: started");
        loop {
            let request = tokio::select! {
                _ = shutdown.cancelled() => {
                    info!("kit history_page_consumer: shutdown signal received, exiting");
                    break;
                }
                request = rx.recv() => match request {
                    None => {
                        info!("kit history_page_consumer: HISTORY_PAGES_TX dropped, exiting");
                        break;
                    }
                    Some(request) => request,
                },
            };
            let identity = request.identity();
            handle_request(&client, &request, identity).await;
        }
    })
}

async fn handle_request(
    client: &AcpTuiClient,
    request: &HistoryPageRequest,
    identity: InFlightPage,
) {
    // 触发侧已按身份合并过并发请求；这里再判一次是为了防止两个触发在
    // 「认领—派发」窗口里各派发一次（相同的页只拉一次，不多发）。
    let current = atoms::HISTORY_PAGING.state().read().clone();
    match &current.in_flight {
        Some(in_flight) if !is_duplicate_of(request, in_flight) => {
            debug!(
                session_id = %request.session_id,
                "history_page_consumer: request superseded by a newer browsing state, skipped"
            );
            return;
        }
        None => return,
        Some(_) => {}
    }
    if !current.accepts(&request.session_id, &request.snapshot_token) {
        debug!(
            session_id = %request.session_id,
            "history_page_consumer: browsing identity is stale, page dropped"
        );
        return;
    }

    match fetch_page(client, request).await {
        Ok(response) => {
            let next_cursor = response.next_cursor.clone();
            // 注入前再核对一次身份：RPC 期间可能已经切会话 / 重新 load。
            let current = atoms::HISTORY_PAGING.state().read().clone();
            if !current.accepts(&request.session_id, &request.snapshot_token) {
                debug!(
                    session_id = %request.session_id,
                    "history_page_consumer: browsing identity changed while fetching, page dropped"
                );
                return;
            }
            if let Some(tx) = atoms::LOCAL_EVENT_TX.get() {
                let _ = tx.send(AcpEventWithEpoch {
                    event: AcpEventData::HistoryPageLoaded {
                        response: Box::new(response),
                    },
                    active_session_id: request.session_id.clone(),
                });
            } else {
                warn!("history_page_consumer: LOCAL_EVENT_TX not initialized, page dropped");
                return;
            }
            // 注入失败（通道关闭）时不推游标，保留重试机会。
            if history_paging::settle_request(&identity, Ok(next_cursor)).is_none() {
                debug!(
                    session_id = %request.session_id,
                    "history_page_consumer: response no longer belongs to the current browsing"
                );
            }
        }
        Err(error) => {
            if error.invalidates {
                history_paging::invalidate_browsing(&request.session_id, &error.message);
            } else if history_paging::settle_request(&identity, Err(error.message.clone()))
                .is_none()
            {
                return;
            }
            // 翻页失败保留当前画面、输入与运行状态，只记录原因供重试。
            warn!(
                session_id = %request.session_id,
                cursor = %request.cursor,
                error = %error.message,
                "history_page_consumer: history page fetch failed"
            );
        }
    }
}

#[cfg(test)]
#[path = "history_page_consumer_test.rs"]
mod tests;
