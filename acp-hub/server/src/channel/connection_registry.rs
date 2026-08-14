//! 连接配额与连接上下文生命周期（架构 §8.6/§9.5，设计稿 `f5-channel-control.md` §5）。
//!
//! 连接配额默认 200（§8.6）；超配额以 1013 关闭（§4.7）。注册发生在认证
//! **前**（防未认证连接占满配额；认证失败由连接 task 释放）。
//!
//! 非回环拒绝（§9.5）在 gateway accept 时用 [`Config::allow_peer`] 判定，
//! 不进本注册表。

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use crate::auth::ConnectionCtx;

/// 连接 id（进程内单调递增）。
pub type ConnId = u64;

/// 连接配额超限（§8.6：以 1013 关闭）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("connection quota exceeded")]
pub struct RegistryFull;

/// 连接句柄（发送侧 + 生命周期 id；`ConnectionCtx` 携带 token_id/role/peer/
/// hostname/established_at，§9.5 token 即身份）。
#[derive(Debug, Clone)]
pub struct ConnHandle {
    /// 连接 id。
    pub id: ConnId,
    /// 认证后的身份上下文。
    pub ctx: ConnectionCtx,
}

/// 连接注册表（配额 + 连接上下文生命周期，§8.6）。
///
/// 并发模型：`Mutex<HashMap>`（低频操作，注册/注销只在连接建立/断开时）。
#[derive(Debug)]
pub struct ConnectionRegistry {
    quota: usize,
    next_id: AtomicU64,
    active: Mutex<HashMap<ConnId, ConnectionCtx>>,
}

impl ConnectionRegistry {
    /// 以配额构建（§16 默认 200）。
    pub fn new(quota: usize) -> Self {
        ConnectionRegistry {
            quota,
            next_id: AtomicU64::new(1),
            active: Mutex::new(HashMap::new()),
        }
    }

    /// 注册连接（认证**前**占位，防未认证连接占满配额；认证失败由调用方
    /// 释放）。超配额 → [`RegistryFull`]。
    pub fn register(&self, ctx: ConnectionCtx) -> Result<ConnHandle, RegistryFull> {
        let mut active = self.active.lock().expect("conn registry lock poisoned");
        if active.len() >= self.quota {
            return Err(RegistryFull);
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        active.insert(id, ctx.clone());
        Ok(ConnHandle { id, ctx })
    }

    /// 连接结束释放（幂等：未知 id 静默忽略）。
    pub fn unregister(&self, conn_id: ConnId) {
        let removed = self
            .active
            .lock()
            .expect("conn registry lock poisoned")
            .remove(&conn_id);
        if removed.is_some() {
            tracing::debug!(conn_id, "connection unregistered");
        }
    }

    /// 认证后上下文替换（§5「认证前占位，防未认证连接占满配额」：注册时
    /// token_id 为占位值，认证成功后替换为真实身份）。幂等。
    pub fn upgrade(&self, conn_id: ConnId, ctx: ConnectionCtx) {
        if let Some(entry) = self
            .active
            .lock()
            .expect("conn registry lock poisoned")
            .get_mut(&conn_id)
        {
            *entry = ctx;
        }
    }

    /// 在线连接数（§17.1 指标）。
    pub fn online(&self) -> usize {
        self.active
            .lock()
            .expect("conn registry lock poisoned")
            .len()
    }

    /// 连接上下文查询（审计/诊断）。
    pub fn ctx(&self, conn_id: ConnId) -> Option<ConnectionCtx> {
        self.active
            .lock()
            .expect("conn registry lock poisoned")
            .get(&conn_id)
            .cloned()
    }
}

#[cfg(test)]
mod connection_registry_test {
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};

    use chrono::Utc;

    use super::*;
    use crate::auth::TokenRole;

    fn ctx(name: &str) -> ConnectionCtx {
        ConnectionCtx {
            token_id: format!("tok-{name}"),
            role: TokenRole::Full,
            name: name.to_string(),
            peer: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 1234),
            hostname: None,
            established_at: Utc::now(),
        }
    }

    #[test]
    fn quota_200_and_full() {
        let reg = ConnectionRegistry::new(200);
        for i in 0..200 {
            reg.register(ctx(&format!("c{i}"))).unwrap();
        }
        assert_eq!(reg.online(), 200);
        assert!(matches!(reg.register(ctx("overflow")), Err(RegistryFull)));
    }

    #[test]
    fn release_after_unregister() {
        let reg = ConnectionRegistry::new(1);
        let h = reg.register(ctx("a")).unwrap();
        assert!(matches!(reg.register(ctx("b")), Err(RegistryFull)));
        reg.unregister(h.id);
        assert_eq!(reg.online(), 0);
        reg.register(ctx("b")).unwrap();
    }

    #[test]
    fn unregister_idempotent() {
        let reg = ConnectionRegistry::new(2);
        let h = reg.register(ctx("a")).unwrap();
        reg.unregister(h.id);
        reg.unregister(h.id); // 幂等
        assert_eq!(reg.online(), 0);
    }

    #[test]
    fn ids_unique() {
        let reg = ConnectionRegistry::new(10);
        let a = reg.register(ctx("a")).unwrap();
        let b = reg.register(ctx("b")).unwrap();
        assert_ne!(a.id, b.id);
    }
}
