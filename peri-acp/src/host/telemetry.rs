//! 宿主遥测装配与关闭结果的保有规则（契约 §3.6、§5.2、§6、§7 P4b）。
//!
//! ACP 在这里**只装配能力**：按部署配置打开 native 暂存端口（目录布局、启动恢复与
//! 私有根解析都留在 `langfuse-client` 适配器内），把端口注入 `LangfuseSession::new_owned`。
//! 宿主不持有路径、临时目录或配额细节。
//!
//! 资源释放顺序（契约 §7 P4b「发送资源比发送器晚关闭」）：端口的强引用随注入交给
//! Batcher worker（唯一发送器与 join owner），宿主只把 `LangfuseShutdownOwner` 交给
//! 部署关闭链。因此端口不可能在发送器收敛之前被释放；关闭报告为待交付时，仍持
//! 任务/清理 owner 的上下文继续保留（见 [`LangfuseShutdownReport::retains_ownership`]），
//! 资源随该上下文一起存活到下一次真实关闭，而不是先释放再补账。
//!
//! 降级一律显式诊断：关闭暂存时不创建任何文件；端点/凭据不可用时按
//! `Unsupported` 有界内存模式继续，不静默冒充持久化。

use std::sync::Arc;

use langfuse_client::spool::SpoolStore;
use peri_config::observability::LangfuseConfig;
use peri_controller::langfuse::{LangfuseSession, LangfuseShutdownOwner};

/// 宿主遥测装配结果：仍由宿主持有的两部分所有权（会话共享 + 独占关闭权限）。
#[derive(Default)]
pub(super) struct HostTelemetry {
    pub(super) session: Option<Arc<LangfuseSession>>,
    pub(super) shutdown_owner: Option<LangfuseShutdownOwner>,
}

/// 装配宿主遥测：无凭据配置时保持未安装（与迁移前一致，不创建端口、不装指标出口）。
///
/// 指标出口的安装由装配点（`host/assemble.rs`）在拿到会话后执行：本函数不触碰
/// `peri-agent` 的进程级 sink，保持纯装配、可单独测试。
pub(super) async fn assemble(config: Option<LangfuseConfig>) -> HostTelemetry {
    let Some(config) = config else {
        return HostTelemetry::default();
    };
    let spool = open_spool_capability(&config);
    if config.spool_enabled && spool.is_none() {
        tracing::warn!(
            "langfuse telemetry spool capability unavailable; bounded in-memory mode for this deployment"
        );
    }
    match LangfuseSession::new_owned(config, "live".into(), spool).await {
        Some((session, owner)) => {
            tracing::info!("Langfuse tracing enabled (host mode)");
            HostTelemetry {
                session: Some(session),
                shutdown_owner: Some(owner),
            }
        }
        None => {
            // 运行期配置被拒已由 session 侧留下结构化诊断；这里不重试、不伪造能力。
            HostTelemetry::default()
        }
    }
}

/// 按配置决定是否注入暂存能力：关闭时不触碰文件系统（契约 §6、§9「关闭=文件增量 0」）。
///
/// 端点或凭据无法解析成 route 摘要时不给端口：此时导出本身也无法冻结 route 上下文，
/// 降级路径必须与 batcher 的 `Unsupported` 判定一致，而不是提交一个无法使用的端口。
fn open_spool_capability(config: &LangfuseConfig) -> Option<Arc<dyn SpoolStore>> {
    if !config.spool_enabled {
        return None;
    }
    let (Some(public_key), Some(secret_key)) =
        (config.public_key.as_deref(), config.secret_key.as_deref())
    else {
        return None;
    };
    let route =
        match langfuse_client::spool::route_fingerprint(&config.host, public_key, secret_key) {
            Ok(route) => route,
            Err(error) => {
                tracing::warn!(%error, "langfuse spool route rejected; bounded in-memory mode");
                return None;
            }
        };
    native_spool(route, config.spool_quota_bytes)
}

/// native 适配器装配；不支持 native 持久化的目标显式返回 `None`（有界内存模式）。
#[cfg(all(not(target_arch = "wasm32"), not(target_os = "emscripten")))]
fn native_spool(
    route: langfuse_client::spool::RouteFingerprint,
    quota_bytes: u64,
) -> Option<Arc<dyn SpoolStore>> {
    match langfuse_client::spool::native::NativeSpool::open_default(route, quota_bytes) {
        Ok(spool) => Some(Arc::new(spool)),
        Err(error) => {
            tracing::warn!(%error, "langfuse spool unavailable; bounded in-memory mode");
            None
        }
    }
}

#[cfg(any(target_arch = "wasm32", target_os = "emscripten"))]
fn native_spool(
    _route: langfuse_client::spool::RouteFingerprint,
    _quota_bytes: u64,
) -> Option<Arc<dyn SpoolStore>> {
    None
}

#[cfg(test)]
#[path = "telemetry_test.rs"]
mod tests;
