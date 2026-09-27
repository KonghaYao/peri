//! 连接代际与连接工厂：把「这条连接还可不可信」和「凭证在哪里」分别定死。
//!
//! ## 为什么按代际记账
//!
//! 放弃一个**已经发出**的在途请求（超时、取消、任务被丢弃）之后，远端流的状态无法证明：
//! 请求可能已经执行，传输层也可能停在半路。此时只有一种诚实做法——把**用到它的那一代**
//! 记为失效，下一次访问重建。代际号把「失效」限制在具体那一条连接上：
//!
//! - 失效事实由在途调用的守卫在 **Drop 点同步**落下（见 [`GenerationLease`]），不依赖任何
//!   异步收尾任务；
//! - 失效事实按代际**单调累积**（只增不减的失效水位，判定是「代际 ≤ 水位」）：一代被记为
//!   失效之后不会被更早的迟到失效「洗白」，而迟到任务拿着旧代际号来标记失效也不会碰到
//!   比它新的连接——新代际号晚于既有水位诞生。
//!
//! 凭证由工厂持有：它只在 SDK 调用边界 [`expose`](super::credentials::SessionStoreCredential::expose)
//! 一次，本模块的类型都不实现 `Debug`/`Serialize`，也不把值写进日志或错误文本。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use async_trait::async_trait;

use peri_acp_types::session_resources::SessionResourceResult;

use super::connection::{connect_sdk, SdkTransport};
use super::credentials::SessionStoreCredential;
use super::endpoint::RemoteEndpoint;
use super::mutation::{RemoteStore, StoreAccess};

/// 连接代际门禁：当前服务的是哪一代、失效事实已经累积到哪一代。
///
/// 只有两个原子量，没有可独立漂移的第二份真相：代际号由工厂铸造（单调递增），失效事实按
/// 代际号记且**只增不减**——落下的失效不会被更早的代际号撤销。
#[derive(Debug, Default)]
pub(super) struct ConnectionGate {
    /// 失效水位：**代际 ≤ 水位**的连接都已被记为不可信；0 表示还没有失效事实（代际从 1 起）。
    invalid: AtomicU64,
    /// 已经铸造出去的代际数。
    minted: AtomicU64,
}

impl ConnectionGate {
    /// 铸造一条新连接的代际号：从 1 起单调递增，只由工厂调用。
    pub(super) fn mint(&self) -> u64 {
        self.minted.fetch_add(1, Ordering::Relaxed) + 1
    }

    /// 某一代是否**已知**失效：`代际 ≤ 失效水位`。
    ///
    /// 两个方向由同一条判定保证：
    ///
    /// - 旧代际的迟失效不会波及后来重建的连接——新代际号在铸造时大于当时的水位，
    ///   之后也只有**更晚**的代际的失效事实能把水位抬到它上面；
    /// - 已经落下的失效事实不会被更早的迟到失效抹掉——水位只增不减，被记为失效的那一代
    ///   不会因为一次旧代际号的失效而重新变成可用。
    pub(super) fn is_invalid(&self, generation: u64) -> bool {
        generation != 0 && generation <= self.invalid.load(Ordering::Acquire)
    }

    /// 借用一次代际守卫；守卫被丢弃而没有被 `release`/`invalidate` 时，这一代记为失效。
    pub(super) fn lease(&self, generation: u64) -> GenerationLease<'_> {
        GenerationLease {
            gate: self,
            generation,
            armed: true,
        }
    }

    /// 记录一次失效：只把水位**抬高**（`fetch_max`），不覆盖。
    ///
    /// 覆盖式写入会让「新一代已失效、旧一代的迟到失效随后到达」把新代的失效事实撤销，
    /// 于是一条已经无法证明的连接被当回可用的。水位只增不减就没有这个方向。
    pub(super) fn invalidate(&self, generation: u64) {
        self.invalid.fetch_max(generation, Ordering::AcqRel);
    }
}

/// 一次在途调用的代际守卫（RAII）。
///
/// 在途 future 被丢弃的机会只有一次，而且发生在 **drop 点**：`tokio::time::timeout` 到点、
/// 外层任务取消、调用方放弃，都在那里同步落定。因此失效事实由 `Drop` 落下——
/// 「调用方已经不管这条连接了」与「这条连接被记为失效」之间没有时间窗。
#[derive(Debug)]
pub(super) struct GenerationLease<'a> {
    gate: &'a ConnectionGate,
    generation: u64,
    armed: bool,
}

impl GenerationLease<'_> {
    /// 调用拿到了确定回答（成功，或远端明确的拒绝）：这一代仍然可信。
    pub(super) fn release(&mut self) {
        self.armed = false;
    }

    /// 结果无法证明：这一代从此不可再用。
    pub(super) fn invalidate(&mut self) {
        self.armed = false;
        self.gate.invalidate(self.generation);
    }

    /// 本守卫盯着的代际号。
    pub(super) fn generation(&self) -> u64 {
        self.generation
    }
}

impl Drop for GenerationLease<'_> {
    fn drop(&mut self) {
        if self.armed {
            // 在途调用被丢弃：这一代的命运无法证明，同步记为失效。
            self.gate.invalidate(self.generation);
        }
    }
}

/// 建立新连接的工厂：**本 crate 唯一持有凭证的地方**。
///
/// 实现必须是纯函数式的：同样的打开事实（端点、凭证、访问意图）产生同样的连接，
/// 不缓存、不轮换、不在失败后自行改变输入。
#[async_trait]
pub(super) trait ConnectionFactory: Send + Sync {
    /// 建立一条新连接（新代际）；失败即没有连接可用，不返回半个连接。
    async fn connect(&self) -> SessionResourceResult<RemoteStore>;
}

/// 生产工厂：端点、凭证与访问意图在装配时定死，重建不改变它们。
pub(super) struct RemoteConnectionFactory {
    endpoint: RemoteEndpoint,
    credential: SessionStoreCredential,
    access: StoreAccess,
    gate: Arc<ConnectionGate>,
}

impl RemoteConnectionFactory {
    pub(super) fn new(
        endpoint: RemoteEndpoint,
        credential: SessionStoreCredential,
        access: StoreAccess,
        gate: Arc<ConnectionGate>,
    ) -> Self {
        Self {
            endpoint,
            credential,
            access,
            gate,
        }
    }
}

#[async_trait]
impl ConnectionFactory for RemoteConnectionFactory {
    async fn connect(&self) -> SessionResourceResult<RemoteStore> {
        let connection = connect_sdk(&self.endpoint, &self.credential).await?;
        Ok(RemoteStore::new(
            Arc::new(SdkTransport::new(connection)),
            self.access,
            self.gate.mint(),
            Arc::clone(&self.gate),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::ConnectionGate;

    /// 失效事实单调累积：落下的失效不会被更早的代际号撤销，新代际也不受旧失效影响。
    ///
    /// 反例（覆盖式写入 + 相等判定）：第 1 代失效 → 建第 2 代 → 第 2 代也失效 →
    /// 第 1 代的迟到失效把记录写回 1，第 2 代从此被判成「可用」——一条无法证明的连接
    /// 被当成可用的。
    #[test]
    fn an_invalidation_fact_is_never_undone_by_a_stale_generation() {
        let gate = ConnectionGate::default();
        let first = gate.mint();
        let second = gate.mint();
        assert!(second > first, "代际号单调递增");

        assert!(!gate.is_invalid(first), "铸造本身不是失效");
        assert!(!gate.is_invalid(second));

        gate.invalidate(first);
        assert!(gate.is_invalid(first), "第 1 代的失效事实成立");
        assert!(!gate.is_invalid(second), "旧代的失效不波及后来建的第 2 代");

        gate.invalidate(second);
        assert!(gate.is_invalid(second), "第 2 代的失效事实成立");

        // 迟到的第 1 代失效（同一份旧守卫在更晚的时刻被丢弃）不能撤销第 2 代的事实。
        gate.invalidate(first);
        assert!(
            gate.is_invalid(second),
            "更早的代际号不能把已经落下的失效事实洗白"
        );
        assert!(gate.is_invalid(first));

        // 之后铸出的代际号晚于水位诞生，任何既有失效事实都不波及它。
        let third = gate.mint();
        assert!(!gate.is_invalid(third), "新代际出生时不属于任何失效水位");
        // 0 不是代际号：没有「未铸造」的失效事实。
        assert!(!gate.is_invalid(0));
    }
}
