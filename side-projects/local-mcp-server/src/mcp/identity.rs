//! 连接身份：可信 principal 与连接实例（WP-005）。
//!
//! 授权规则（冻结，见 `artifacts/designs/WP-000/interface-plan.md` §1 与
//! `artifacts/designs/WP-001/interfaces.md` §8）：
//!
//! - `principal` **只**由认证层（HTTP bearer token）或可信连接层（stdio 连接实例）
//!   产生；请求里的 `_meta.io.modelcontextprotocol/clientInfo` 可能是伪造的，仅用于
//!   显示与能力判断，**永不**参与授权。
//! - `client_instance` 每条可信连接唯一且不可猜；任务句柄与资源绑定
//!   `(principal, client_instance)`，跨主体/跨实例一律拒绝。
//! - stdio 下一个进程就是一条连接，因此身份在连接建立时一次性生成：同一进程内两个
//!   `stdio()` 身份必须不同，避免把"同一进程"误当作"同一主体"。

use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::wire::{ClientInstanceId, PrincipalId, RequestContext, RequestId, TaskSnapshot};

/// 一条可信连接的身份。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionIdentity {
    principal: PrincipalId,
    client_instance: ClientInstanceId,
}

impl ConnectionIdentity {
    /// 由认证层/传输层构造（调用方必须已经完成可信判定）。
    pub fn new(
        principal: impl Into<PrincipalId>,
        client_instance: impl Into<ClientInstanceId>,
    ) -> Self {
        Self {
            principal: principal.into(),
            client_instance: client_instance.into(),
        }
    }

    /// 生成 stdio 连接身份：每个进程一条连接，两个 id 都是不可猜的随机标识。
    pub fn stdio() -> Self {
        Self::new(
            format!("stdio-principal-{}", Uuid::new_v4()),
            format!("stdio-conn-{}", Uuid::new_v4()),
        )
    }

    /// 可信主体。
    pub fn principal(&self) -> &str {
        &self.principal
    }

    /// 连接实例。
    pub fn client_instance(&self) -> &str {
        &self.client_instance
    }

    /// 是否拥有该任务快照（owner 与连接实例都必须一致）。
    ///
    /// 只有 `true` 才允许把快照投影成资源或通知；`false` 一律按"不存在"处理，
    /// 不区分"别人的"与"没有的"，避免泄露他人任务是否存在。
    pub fn owns(&self, snapshot: &TaskSnapshot) -> bool {
        snapshot.owner == self.principal && snapshot.client_instance == self.client_instance
    }

    /// 构造一次工具调用的上下文。
    ///
    /// `cancellation` 由传输层给出（`notifications/cancelled` 与连接关闭都会触发），
    /// 必须一路传给执行器，避免已取消的请求继续占用执行面。
    pub fn request_context(
        &self,
        request_id: impl Into<RequestId>,
        cancellation: CancellationToken,
    ) -> RequestContext {
        RequestContext {
            request_id: request_id.into(),
            principal: self.principal.clone(),
            client_instance: self.client_instance.clone(),
            cancellation,
        }
    }
}

#[cfg(test)]
#[path = "identity_test.rs"]
mod tests;
