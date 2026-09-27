//! 远程 adapter 的关闭语义（离线装配，不连网）。
//!
//! 门面在确认未结清事实之前不会再调用数据面的 `close`，但确认之后的收尾仍可能失败。
//! 关闭**失败或取消**都不是确认关闭：连接被保留在 adapter 的关闭句柄里（业务读不复活、
//! 不重连），重试关闭的是同一条真实连接。
//!
//! 本文件只覆盖**从来没有过连接**的装配（关闭态装配，与「关闭过一次但没成功」不同：
//! 那种情况下连接仍在关闭句柄里，关闭可以重试）：这里没有可关闭的真实资源，因此
//! **不能**把「没有连接」当成「已经干净关闭」，重复关闭也不谎报成功。

use peri_acp_types::session_resources::SessionResourceErrorKind;

use super::schema::StoreId;
use super::session_data::RemoteSessionData;
use crate::sessions::data::SessionDataPort;

#[tokio::test]
async fn close_without_a_connection_is_not_reported_as_success() {
    // 关闭态装配：连接从来没有过（没有任何真实资源可关，也没有关闭句柄可用）。
    let adapter = RemoteSessionData::closed_for_test(StoreId::mint());

    let error = adapter.close().await.unwrap_err();
    assert!(matches!(
        error.kind(),
        SessionResourceErrorKind::Internal { .. }
    ));
    // 幂等成功只属于确认关闭；这里既没有连接也没有关闭进度，重复关闭不谎报成功。
    assert!(adapter.close().await.is_err());
}
