//! 远端会话数据的连接生命周期：服务中/关闭中槽位、失效代际重建与真实关闭。

use std::sync::Arc;

#[cfg(test)]
use async_trait::async_trait;
use peri_acp_types::session_resources::{
    SessionResourceError, SessionResourceErrorKind, SessionResourceResult,
};
use tokio::sync::RwLockReadGuard;

#[cfg(test)]
use super::super::generation::ConnectionFactory;
use super::super::mutation::{RemoteStore, StoreAccess};
use super::super::schema::StoreIdentityRead;
use super::super::session_codec;
use super::{unsupported_behavior, RemoteSessionData};

impl RemoteSessionData {
    /// 取当前连接（已就绪的借用）。
    ///
    /// 只有「槽里还有一条**服务中**的连接、但这一代已被记为失效」才重建，而且**只重建一次**：
    /// 失败如实上报，下一次调用再试。槽里没有可服务的连接（关闭已经开始、关闭已确认，或
    /// 关闭态装配）一律返回关闭错误——一次重连绝不能把「已经开始的关闭」变回来。
    ///
    /// 重建判定与取守卫之间还有一个空窗：另一条在途调用被放弃（超时、取消）会把**当前**这一代
    /// 记为失效。拿到读锁之后再复核一次，就不会把已知失效的连接交出去——本调用什么都没发，
    /// 只如实失败（[`Unavailable`](SessionResourceErrorKind::Unavailable)），下一次访问按它重建。
    /// 不做自动重试：重建是下一次访问的事，本次绝不把请求发到一条已经不可证明的连接上。
    pub(in crate::sessions::remote) async fn store(
        &self,
    ) -> SessionResourceResult<RwLockReadGuard<'_, RemoteStore>> {
        self.reconnect_if_retired().await?;
        let slot = self.slot.read().await;
        let store = RwLockReadGuard::try_map(slot, ConnectionSlot::serving_store)
            .map_err(|_| connection_closed())?;
        if self.gate.is_invalid(store.generation()) {
            return Err(retired_connection());
        }
        Ok(store)
    }

    /// 只读打开的访问意图下，先于任何「读前置事实」拒绝写入。
    ///
    /// mutation 层自己也会拒绝只读写入，但那发生在**发起写入**时：读前置事实之后提前
    /// 返回成功的路径（如「未知边界 = 无变更」）会绕过它，让只读请求以成功收场。
    /// 本机 adapter 的同类路径先 `writable()` 再读边界，这里保持同一顺序。
    pub(in crate::sessions::remote) async fn ensure_writable(&self) -> SessionResourceResult<()> {
        self.store().await?.access().ensure_writable()
    }

    /// 失效代际的重建：建立新连接、重核实身份、替换槽位、关闭退场连接。
    async fn reconnect_if_retired(&self) -> SessionResourceResult<()> {
        let Some(retired) = self.retired_generation().await else {
            return Ok(());
        };
        let replacement = self.factory.connect().await?;
        if let Err(error) = self.verify_reconnect(&replacement).await {
            // 没能证明「还是这个库」的新连接不装回槽里：关闭它，按原错误上报。
            retire(replacement).await;
            return Err(error);
        }
        let replaced = {
            let mut slot = self.slot.write().await;
            match slot.serving_store().map(RemoteStore::generation) {
                // 槽在重建期间失去服务中的连接（关闭开始把连接移进关闭句柄）：不复活，不装回去。
                None => {
                    drop(slot);
                    retire(replacement).await;
                    return Err(connection_closed());
                }
                // 别的调用已经重建好了：自己这条一次请求都没发过，丢弃即可。
                Some(current) if current != retired => {
                    drop(slot);
                    retire(replacement).await;
                    return Ok(());
                }
                Some(_) => slot.replace_serving(replacement),
            }
        };
        // 退场连接的关闭在锁外做，而且不阻塞其它访问：重建路径不能因为一次关闭把别人堵住。
        if let Some(replaced) = replaced {
            retire(replaced).await;
        }
        Ok(())
    }

    /// 当前**服务中**的连接是否已被记为失效；返回那一代的代际号。
    ///
    /// 关闭中的槽没有服务中的连接，也就没有「重建」这回事（返回 `None`：业务读如实失败，
    /// 不重连）。
    async fn retired_generation(&self) -> Option<u64> {
        let slot = self.slot.read().await;
        let store = slot.serving_store()?;
        self.gate
            .is_invalid(store.generation())
            .then(|| store.generation())
    }

    /// 重建后的重核实：新连接必须就是**同一个** store，且本构建认识它。
    ///
    /// 只读检查——不建表、不初始化、不登记：重连不改变任何事实，只证明「还是这个库」。
    /// 只读打开因此与可写打开走同一条重建路径，且不产生任何写入。
    async fn verify_reconnect(&self, store: &RemoteStore) -> SessionResourceResult<()> {
        match store.read_identity().await? {
            StoreIdentityRead::Present(snapshot)
                if !(snapshot.matches_build()
                    || (store.access() == StoreAccess::ReadOnly && snapshot.readable())) =>
            {
                Err(unsupported_behavior(
                    "unrecognized remote store schema after reconnect",
                ))
            }
            StoreIdentityRead::Present(snapshot)
                if snapshot.store_id.as_str() != self.store_id.as_str() =>
            {
                Err(unsupported_behavior(
                    "reconnect reached a different remote store",
                ))
            }
            StoreIdentityRead::Present(_) => Ok(()),
            StoreIdentityRead::Malformed => Err(session_codec::corrupt(
                "remote session store metadata is not interpretable after reconnect",
            )),
            StoreIdentityRead::Uninitialized => Err(unsupported_behavior(
                "reconnected remote store has no session schema",
            )),
        }
    }
}

/// 关闭态装配用的连接工厂：任何一次重建都明确失败（关闭的实例不重连）。
#[cfg(test)]
pub(super) struct NoConnectionFactory;

#[cfg(test)]
#[async_trait]
impl ConnectionFactory for NoConnectionFactory {
    async fn connect(&self) -> SessionResourceResult<RemoteStore> {
        Err(connection_closed())
    }
}

/// 退场连接的收尾：尝试关闭并记下结果。
///
/// 关闭失败**不**掩盖本次重建的结论：这一代本来就已经不可信，重连的意义是让后续访问有路
/// 可走；但也不能悄悄丢掉——失败按诊断记录，不含凭证、URL 或语句内容。
async fn retire(store: RemoteStore) {
    if store.close().await.is_err() {
        tracing::debug!("retired remote connection did not close cleanly");
    }
}

/// 连接的生命周期槽位：**服务中**，或**关闭中**（含已确认关闭）。
///
/// 两个字段互斥，翻转点只有一个（[`Self::begin_close`]）：取走 `serving`、装上 `closing`。
/// 空槽（两者皆空）只属于「从来没有过连接」的装配。
pub(super) struct ConnectionSlot {
    /// 服务中的连接；关闭开始后为空——业务读不再经过它，也不会因此重连。
    serving: Option<RemoteStore>,
    /// 关闭句柄；关闭开始后一直保留（含已确认关闭），真实资源的关闭进度在它里面。
    closing: Option<Arc<ClosingConnection>>,
}

impl ConnectionSlot {
    /// 装配一条服务中的连接。
    pub(super) fn serving(store: RemoteStore) -> Self {
        Self {
            serving: Some(store),
            closing: None,
        }
    }

    /// 服务中的连接（关闭中、空槽为 `None`）。
    pub(super) fn serving_store(&self) -> Option<&RemoteStore> {
        self.serving.as_ref()
    }

    /// 换掉服务中的连接，返回退场的那一条；只在确认槽里有服务中的连接时调用。
    fn replace_serving(&mut self, store: RemoteStore) -> Option<RemoteStore> {
        self.serving.replace(store)
    }

    /// 开始关闭（幂等）：把服务中的连接移进关闭句柄；已经在关闭中时复用同一个句柄。
    ///
    /// `None` 只出现在空槽（从来没有过连接）：这与「关闭失败」**不同**——失败时句柄已经被
    /// 保留下来，重试关闭的是同一条真实连接。
    pub(super) fn begin_close(&mut self) -> Option<Arc<ClosingConnection>> {
        if let Some(closing) = &self.closing {
            return Some(Arc::clone(closing));
        }
        let store = self.serving.take()?;
        let closing = Arc::new(ClosingConnection::new(store));
        self.closing = Some(Arc::clone(&closing));
        Some(closing)
    }
}

impl Default for ConnectionSlot {
    /// 空槽：连接从来没有过（关闭态装配）。
    fn default() -> Self {
        Self {
            serving: None,
            closing: None,
        }
    }
}

/// 关闭句柄：连接退出业务路径之后，真实资源的关闭进度被保留在这里。
///
/// 唯一句柄在**句柄里**而不在关闭 future 里：调用方超时、取消或整个 future 被丢弃都不会丢
/// 连接。关闭失败或被取消都不确认（[`CloseProgress::Unconfirmed`]），下一次调用在**同一条
/// 连接**上继续关——不新建连接，也不把一次未确认的失败固化成永久错误。
pub(super) struct ClosingConnection {
    /// 待关闭的连接：确认关闭成功之前一直保留。
    connection: RemoteStore,
    /// 关闭尝试的串行点：并发关闭共享同一次真实关闭的结论，成功只发生一次。
    progress: tokio::sync::Mutex<CloseProgress>,
}

/// 真实关闭的进度：只有「已确认成功」会让关闭不再重做。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CloseProgress {
    /// 尚未成功（从未尝试，或上一次失败/被取消）：允许重试。
    Unconfirmed,
    /// 已确认成功：不再重复关闭同一条连接，后续关闭幂等成功。
    Confirmed,
}

impl ClosingConnection {
    fn new(connection: RemoteStore) -> Self {
        Self {
            connection,
            progress: tokio::sync::Mutex::new(CloseProgress::Unconfirmed),
        }
    }

    /// 真实关闭；并发调用被串行化，成功只发生一次。
    ///
    /// 失败如实上报且**不**确认：连接仍在句柄里，下一次调用在同一条连接上继续关闭。
    /// 调用 future 被丢弃只放弃这一次尝试——句柄与进度都不在 future 里，因此什么也不会丢。
    pub(super) async fn shutdown(&self) -> SessionResourceResult<()> {
        let mut progress = self.progress.lock().await;
        if *progress == CloseProgress::Confirmed {
            return Ok(());
        }
        self.connection.close().await?;
        *progress = CloseProgress::Confirmed;
        Ok(())
    }
}

/// 槽里没有可服务的连接（关闭已经开始、关闭已确认，或从来没有过连接）：不是「没有连接就没问题」。
pub(super) fn connection_closed() -> SessionResourceError {
    SessionResourceError::new(SessionResourceErrorKind::Internal {
        detail: "remote session store connection is closed".to_owned(),
    })
}

/// 这一代在守卫交出去之前就被记为失效：本调用**没有发出任何请求**。
///
/// 保守归类为 `Unavailable`（连接这一刻不可用），而不是「关闭」或「契约」类错误：这不是
/// 会话数据的结论，也不代表远端拒绝；下一次访问会按同一份打开事实重建。
fn retired_connection() -> SessionResourceError {
    SessionResourceError::new(SessionResourceErrorKind::Unavailable {
        detail: "remote session store connection was retired before use".to_owned(),
    })
}
