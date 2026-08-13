//! challenge nonce 防重放注册表（§9.2「短期有效窗口 30s 过期」）。
//!
//! proto 的 [`SeenNonces`] 是无时间戳的 HashSet（纯内存容器）；「30s 过期」的
//! 判定在本模块落地：`HashMap<nonce, 登记时刻>`。窗口语义（`docs/plans/
//! f2-auth-config.md` §4.4【决策】）：nonce 在登记时刻起 30s 内有效；过期后
//! 同 nonce 再次提交按新 nonce 处理（Accepted）——机器每次连接新生成 nonce，
//! 无冲突。连接断开即失效由全局 30s TTL 自然覆盖。

use std::collections::HashMap;
use std::time::Instant;

use acp_hub_proto::hmac::{CHALLENGE_NONCE_LEN, NONCE_TTL};

/// 判定结果（与 [`AuthError`](crate::auth::AuthError) 区分：前者是正反判定，
/// 后者是错误面）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NonceVerdict {
    /// 未见过（或已过期重新登记），放行。
    Accepted,
    /// 见过且在窗口内，重放，拒绝。
    Replay,
    /// 见过且已过期（当前流程按新 nonce 处理为 Accepted；保留该变体供
    /// 未来显式过期拒绝语义）。
    Expired,
}

/// proto::SeenNonces + 时间戳包装：非重复 + 30s 窗口 + 过期清理。
#[derive(Debug, Default)]
pub struct NonceRegistry {
    seen: HashMap<[u8; CHALLENGE_NONCE_LEN], Instant>,
}

impl NonceRegistry {
    /// 空注册表。
    pub fn new() -> Self {
        Self::default()
    }

    /// 判定并登记：未见过且在窗口内 → `Accepted` 并记录；窗口内重提 → `Replay`；
    /// 见过但已过期 → 视为新 nonce（重新登记，`Accepted`）。
    ///
    /// `now` 由调用方注入（测试可推进虚拟时钟，§6.3 N4）。
    pub fn check_and_mark(
        &mut self,
        nonce: &[u8; CHALLENGE_NONCE_LEN],
        now: Instant,
    ) -> NonceVerdict {
        if let Some(&marked_at) = self.seen.get(nonce) {
            if now.duration_since(marked_at) <= NONCE_TTL {
                return NonceVerdict::Replay;
            }
            // 已过期：按新 nonce 处理（TTL 语义，§4.4）。
            self.seen.insert(*nonce, now);
            return NonceVerdict::Accepted;
        }
        self.seen.insert(*nonce, now);
        NonceVerdict::Accepted
    }

    /// 惰性 + 周期清理：按 `now` 清除过期条目（防内存无限增长；幂等）。
    pub fn sweep(&mut self, now: Instant) {
        self.seen
            .retain(|_, marked_at| now.duration_since(*marked_at) <= NONCE_TTL);
    }

    /// 已登记 nonce 数量。
    pub fn len(&self) -> usize {
        self.seen.len()
    }

    /// 是否为空。
    pub fn is_empty(&self) -> bool {
        self.seen.is_empty()
    }
}

#[cfg(test)]
#[path = "nonce_test.rs"]
mod nonce_test;
