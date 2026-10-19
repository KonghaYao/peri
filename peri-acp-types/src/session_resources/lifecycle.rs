use async_trait::async_trait;

use super::SessionResourceResult;

/// 部署生命周期关闭端口：关闭整个会话存储的**唯一**入口。
///
/// 门面（[`super::SessionResources`]）是业务句柄：可以克隆、可以注入 Agent/Controller/
/// middleware，它只提供会话行为与必要的收敛能力（恢复、排空）。**关闭整个存储不是
/// 业务行为**——那会让任何持有句柄的调用方获得全局销毁权，因此关闭权单独由本端口
/// 承载，并由部署装配（TUI/print/stdio 的宿主配置）在**自己的任务排空之后**调用一次。
///
/// 实现必须把两件事分开：
///
/// - 关闭**不可逆地**停止新写入，但「停止新写入」不等于「已关闭」：只有真实检查
///   （在途写入、未决持久化）全部结清并确实关闭数据面之后才确认关闭，此后的重复调用
///   才幂等成功；
/// - 失败或被取消的关闭不构成确认，重复调用必须重新检查。未确认期间该存储的恢复与
///   排空仍然可用（业务句柄不受影响），否则第一次关闭失败就再也没有收敛路径。
#[async_trait]
pub trait SessionStoreShutdownPort: Send + Sync {
    /// 排空之后关闭会话存储；只有确认关闭才返回 `Ok`。
    async fn shutdown(&self) -> SessionResourceResult<()>;
}
