//! Builtin MCP 运行时（owner E-03）：同进程 duplex 链路、server task 归属、有界关闭。
//!
//! 职责边界（IF-D12 / sub-plan E §4.3）：
//! - 本模块**只**负责把 handler 交给 `rmcp::serve_server`、持有 server task，并给出
//!   有界关闭语义。handler 的业务语义（工具清单、`tools/list`、`call_tool` 的结果映射）
//!   归独立的 `peri-mcp-*` capability crates。
//! - builtin **恒为同进程对象**：不引入子进程、不读 env、不写磁盘、不接触任何凭据。
//! - 三分类超时（IF-D1）的事实源是 [`crate::mcp::transport::TransportKind`]；本模块不
//!   复制判定（映射在 `mcp/initialize.rs` 的 `connect_timeout` / `transport_label`）。
//! - 每个实例一条**独立** duplex 与一个**独立** task（隔离契约：不得共用 transport）。

use std::sync::Arc;

use rmcp::{serve_server, service::QuitReason, ServerHandler};
use thiserror::Error;
use tokio::{
    io::{DuplexStream, ReadHalf, WriteHalf},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

use super::context::BuiltinInstanceContext;

/// builtin duplex 通道容量（A16）。
///
/// **capacity 只影响背压，不是单帧上限**：单帧大小不受它约束。两侧读写都在各自 task
/// 中推进，缓冲写满时 `poll_write` 返回 `Pending` 反压调用方，而不是截断或拒绝某一帧。
/// 因此大正文（WebFetch 级，数十~数百 KB）可以完整往返，**不得**按帧大小调整本值。
pub(crate) const BUILTIN_DUPLEX_BUF: usize = 8 * 1024;

/// server task 有界收敛等待上界。
///
/// 关闭顺序（§4.3 第 4 条）：① client service `close_with_timeout` ② 有界等待 server
/// task 靠输入流 EOF 收敛 ③ 未收敛才 `abort` + `await`。`abort` **不是**正常路径。
pub(crate) const BUILTIN_CONVERGE_TIMEOUT: std::time::Duration =
    std::time::Duration::from_millis(1000);

/// tick 驱动的固定周期：1s，语义搬运自既有宿主 `HostTaskKind::CronTick` 的
/// `peri_time::interval(1s)`（该宿主 task 在 W2 装配收口时删除，见 sub-plan H §5.1）。
///
/// **唯一 spawn 点**是 [`crate::mcp::client::McpClientPool::spawn_builtin_transport`]：本
/// 常量只描述周期，不描述「谁该起 tick」——tick 是否挂载由实例名与
/// `CronInstanceInput::tick_enabled` 在 spawn 点共同决定。
pub(crate) const BUILTIN_TICK_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);

/// builtin 链路装配失败（typed）。
///
/// 错误文本只含实例名与固定规则文本：不含路径、env、URL 认证信息或任何凭据（§9 规则 7）。
#[derive(Debug, Error)]
pub(crate) enum BuiltinSpawnError {
    /// 实例名不在注册表内（`builtin_mcp::find` 只命中已实现实例）。
    ///
    /// 文本与 `transport::TransportError::UnknownBuiltinInstance` 一致：同一事实只有一种
    /// 用户可见措辞。
    #[error("builtin MCP 实例未注册: {instance}")]
    UnknownInstance { instance: String },
    /// pool 尚未注入实例上下文（A33：注入早于 `initialize`）。
    ///
    /// 该状态**不**降级：不 panic、不退回「用进程 cwd 凑合」、不静默改造为其它传输形态，
    /// 也不产生任何 ready 证据。唯一修复路径是宿主装配在 `run_initialize` 之前注入。
    #[error("builtin 实例上下文未注入: {instance}")]
    ContextMissing { instance: String },
    /// 上下文已注入，但缺该实例所需的输入（`cron` 缺 scheduler / `lsp` 缺 pool）。
    ///
    /// 与 [`Self::HandlerNotWired`] **分开**：两者的修复动作不同（补上下文 vs 接 handler），
    /// 合并会让「输入没给全」看起来像「代码没写完」。
    #[error("builtin 实例缺少上下文输入: {instance}")]
    InstanceInputMissing { instance: String },
    /// 已注册实例尚无 handler 模块（`dispatch::builtin_server_handler` 未覆盖该实例）。
    ///
    /// 该状态**不**降级：不 panic、不静默改造为 stdio / http，也不产生任何 ready 证据。
    /// 当前注册表内实例均由 dispatch 显式接线；该分支保护未来新增实现避免静默回退。
    #[error("builtin 实例 handler 尚未接线: {instance}")]
    HandlerNotWired { instance: String },
}

/// server task 的退出事实。
///
/// 不把「未进入服务就退出」伪装成正常关闭：握手装配失败与优雅关闭是两件事。
#[derive(Debug)]
pub(crate) enum BuiltinServerExit {
    /// 已进入服务并收敛（正常路径：client 关闭 → 输入流 EOF → `QuitReason::Closed`）。
    Quit(QuitReason),
    /// 尚未进入服务即退出（`serve_server` 装配/握手失败），文本已由 rmcp 脱敏。
    NotStarted(String),
    /// 未在等待上界内收敛，已被 `abort`。
    AbortedAfterTimeout,
    /// task 本身失败（panic / 被取消以外的 join 错误）。
    TaskFailed(String),
}

/// 一个 builtin 实例的 server task 句柄（pool 侧 task 表的 value）。
pub(crate) struct BuiltinServerTask {
    instance: String,
    handle: JoinHandle<BuiltinServerExit>,
}

impl BuiltinServerTask {
    /// 所属实例名（server name / 配置 key）。
    pub(crate) fn instance(&self) -> &str {
        &self.instance
    }

    /// task 是否已结束——「无 orphan」的可观察证据。
    pub(crate) fn is_finished(&self) -> bool {
        self.handle.is_finished()
    }

    /// 有界收敛：先等 `timeout`；**未收敛才** `abort` + `await`。
    ///
    /// 返回退出事实。`AbortedAfterTimeout` 表示等待上界内没有靠 EOF 自然收敛，
    /// 是异常信号（正常关闭必须落在 `Quit`），调用方据此告警。
    pub(crate) async fn converge(&mut self, timeout: std::time::Duration) -> BuiltinServerExit {
        match peri_time::timeout(timeout, &mut self.handle).await {
            Ok(Ok(exit)) => exit,
            Ok(Err(error)) => BuiltinServerExit::TaskFailed(error.to_string()),
            Err(_) => {
                self.handle.abort();
                let _ = (&mut self.handle).await;
                BuiltinServerExit::AbortedAfterTimeout
            }
        }
    }
}

/// tick task 的退出结论。
///
/// 与 [`BuiltinServerExit`] 分开：tick 没有「进入服务」这一状态，`NotSpawned`（本代
/// 没有 tick）与 `Joined`（真的被 cancel 后有界 join 完成）是两件不同的事，不得合并成
/// 「没有错误」——否则「无 tick 实例」与「tick 已停」在关闭检查里不可区分。
#[derive(Debug)]
pub(crate) enum TickCloseOutcome {
    /// 本代没有 tick（非 cron 实例，或 `tick_enabled=false`）。
    NotSpawned,
    /// 在被 cancel 后有界 join 完成。
    Joined,
    /// 未在有界等待内退出，已 abort。
    AbortedAfterTimeout,
    /// task 本身失败（join 错误）。
    TaskFailed(String),
}

/// 每 interval 调一次 `on_tick` 的驱动 task 归属（A32：tick **不放在 handler** 里）。
///
/// 「代」的粒度：一代 builtin transport 一个 guard。同一 scheduler 任一时刻至多一个
/// 驱动——reconnect 先 `close` 旧代（其中 tick 已 join）再建新代。
///
/// `Drop` 只 `cancel()`：兜底、不 abort、不阻塞、不承担正常收敛。正常路径必须走
/// [`Self::shutdown`]，因为只有它给出可断言的退出结论（超时/abort 是异常信号）。
pub(crate) struct TickGuard {
    cancel: CancellationToken,
    join: JoinHandle<()>,
    /// tick task 退出前触发一次（测试观测点）。生产不等待它，也不据它判收敛。
    stopped: Arc<tokio::sync::Notify>,
}

impl TickGuard {
    /// 固定周期驱动。语义逐位对齐既有宿主 tick（`peri-acp/src/host/assemble.rs` 的
    /// `HostTaskKind::CronTick`）：每个周期调用一次 `on_tick`（同步闭包，内部不得
    /// await）；`cancel()` 后当前周期结束即退出。
    pub(crate) fn spawn(
        interval: std::time::Duration,
        mut on_tick: impl FnMut() + Send + 'static,
    ) -> Self {
        let cancel = CancellationToken::new();
        let stopped = Arc::new(tokio::sync::Notify::new());
        let task_cancel = cancel.clone();
        let task_stopped = Arc::clone(&stopped);
        let join = tokio::spawn(async move {
            let mut interval = peri_time::interval(interval);
            loop {
                tokio::select! {
                    _ = task_cancel.cancelled() => break,
                    _ = interval.tick() => on_tick(),
                }
            }
            task_stopped.notify_waiters();
        });
        Self {
            cancel,
            join,
            stopped,
        }
    }

    /// 有界退出：先 `cancel()`，**未在 `timeout` 内 join 完成才** `abort()` + await。
    ///
    /// 顺序冻结（A32）：代监督者必须在本步**返回之后**才收敛 server task。`cancel()` 是
    /// 协作式的，所以「已 cancel」不等于「已退出」——只有本方法的返回值能作为证据。
    pub(crate) async fn shutdown(mut self, timeout: std::time::Duration) -> TickCloseOutcome {
        self.cancel.cancel();
        match peri_time::timeout(timeout, &mut self.join).await {
            Ok(Ok(())) => TickCloseOutcome::Joined,
            Ok(Err(error)) => TickCloseOutcome::TaskFailed(error.to_string()),
            Err(_) => {
                self.join.abort();
                let _ = (&mut self.join).await;
                TickCloseOutcome::AbortedAfterTimeout
            }
        }
    }

    /// task 是否已结束。
    pub(crate) fn is_finished(&self) -> bool {
        self.join.is_finished()
    }

    /// 测试专用：task 在返回前触发一次（顺序可被因果验证，见 runtime_test）。
    #[cfg(test)]
    pub(crate) fn stopped(&self) -> Arc<tokio::sync::Notify> {
        Arc::clone(&self.stopped)
    }
}

impl Drop for TickGuard {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

/// 一代 builtin transport 的关闭所有权：tick 驱动 + server task。
///
/// 调用方经 [`Self::close`] 关闭，顺序不可绕过：先停 tick，再收敛 server task。两个
/// 部件由**同一次** transport 装配产出，因此「本代」身份一致（见
/// [`BuiltinTransport::into_parts`]）。
pub(crate) struct BuiltinInstanceSupervisor {
    instance: String,
    tick: Option<TickGuard>,
    server_task: BuiltinServerTask,
    #[cfg(not(target_os = "emscripten"))]
    workspace_tasks: Option<peri_mcp_workspace::WorkspaceMcpServer>,
}

impl BuiltinInstanceSupervisor {
    /// 组装一代的关闭所有权（`tick` 与 `server_task` 必须同代）。
    pub(crate) fn new(
        instance: String,
        server_task: BuiltinServerTask,
        tick: Option<TickGuard>,
    ) -> Self {
        Self {
            instance,
            tick,
            server_task,
            #[cfg(not(target_os = "emscripten"))]
            workspace_tasks: None,
        }
    }

    /// 有界关闭：①`tick.take()` → [`TickGuard::shutdown`] ②server task `converge`。
    ///
    /// 两侧结论同时返回。任一侧落到 `*AbortedAfterTimeout` 时告警——`abort` **不是**正常
    /// 路径（正常关闭必须是 `Joined` + `Quit`）。
    ///
    /// 顺序是**因果必需**而非风格：tick 是会持续驱动业务动作的来源，若先收敛 server task
    /// （或两侧并发），tick 可能在 server 已退出后仍触发一轮动作。
    pub(crate) async fn close(mut self, timeout: std::time::Duration) -> BuiltinCloseOutcome {
        let tick = match self.tick.take() {
            Some(guard) => {
                let outcome = guard.shutdown(timeout).await;
                if matches!(outcome, TickCloseOutcome::AbortedAfterTimeout) {
                    tracing::warn!(
                        server = %self.instance,
                        "builtin tick task 未在有界等待内退出，已 abort"
                    );
                }
                outcome
            }
            None => TickCloseOutcome::NotSpawned,
        };
        let server = self.server_task.converge(timeout).await;
        #[cfg(not(target_os = "emscripten"))]
        if let Some(workspace) = self.workspace_tasks.take() {
            if peri_time::timeout(timeout, workspace.shutdown_shell_tasks())
                .await
                .is_err()
            {
                tracing::warn!("builtin Workspace task cleanup timed out");
            }
        }
        if matches!(server, BuiltinServerExit::AbortedAfterTimeout) {
            tracing::warn!(
                server = %self.instance,
                "builtin server task 未在有界等待内收敛，已 abort"
            );
        }
        BuiltinCloseOutcome { tick, server }
    }

    /// 所属实例名（server name / 配置 key）。
    pub(crate) fn instance(&self) -> &str {
        &self.instance
    }

    /// 「本代已无运行中的 tick」：无 tick ⇒ `true`（**不是** `false`）。
    ///
    /// 无 tick 的一代不存在「tick 还在跑」的风险；读成 `false` 会让非 cron 实例永远
    /// 无法通过关闭检查。
    pub(crate) fn tick_is_finished(&self) -> bool {
        match &self.tick {
            Some(guard) => guard.is_finished(),
            None => true,
        }
    }
}

/// 一代 builtin transport 的关闭结论：两侧结论同时返回。
#[derive(Debug)]
pub(crate) struct BuiltinCloseOutcome {
    /// tick 侧结论（无 tick 的一代是 [`TickCloseOutcome::NotSpawned`]）。
    pub(crate) tick: TickCloseOutcome,
    /// server task 侧退出事实。
    pub(crate) server: BuiltinServerExit,
}

/// client 侧 duplex 分半（原 `BuiltinTransport::io` 的类型）。
pub(crate) type TransportIo = (ReadHalf<DuplexStream>, WriteHalf<DuplexStream>);

/// 一条已装配的 builtin 链路：client 侧 duplex 分半 + 本代关闭所有权。
pub(crate) struct BuiltinTransport {
    /// 交给 `serve_client_auto` 的 client 侧 transport（与 stdio / http 同参进入同一条
    /// 处理链：握手、发现、句柄构造、提交语义完全同构）。
    pub(crate) io: TransportIo,
    /// 本代的 tick 驱动。装配层恒为 `None`，由 pool 的唯一 spawn 点
    /// （`McpClientPool::spawn_builtin_transport`）按实例名与 `tick_enabled` 决定是否挂上；
    /// `pub(crate)` 便于 pool 方法据此挂 tick。
    pub(crate) tick: Option<TickGuard>,
    /// server 侧 task。**必须**经 [`Self::into_parts`] 的监督者登记进 pool 的 builtin 表，
    /// 否则关闭/重连留 orphan。
    pub(crate) server_task: BuiltinServerTask,
    #[cfg(not(target_os = "emscripten"))]
    workspace_tasks: Option<peri_mcp_workspace::WorkspaceMcpServer>,
}

impl BuiltinTransport {
    /// 拆出 client 侧 io 与**本代**的关闭所有权。
    ///
    /// 登记进 pool 表的必须是监督者而不是裸 server task：pool 的关闭路径据此拿到固定
    /// 顺序（先停 tick 再收敛 server task），不新增第二条关闭路径。
    pub(crate) fn into_parts(self) -> (TransportIo, BuiltinInstanceSupervisor) {
        let instance = self.server_task.instance().to_string();
        let mut supervisor = BuiltinInstanceSupervisor::new(instance, self.server_task, self.tick);
        #[cfg(not(target_os = "emscripten"))]
        {
            supervisor.workspace_tasks = self.workspace_tasks;
        }
        (self.io, supervisor)
    }
}

/// 装配一条同进程链路：`tokio::io::duplex` 分半 + 真实 `rmcp::serve_server` 在 task 中运行。
///
/// `ServerHandler` 带 `Sized` 约束（`rmcp-3.1.4/src/handler/server.rs:593`），无法用
/// `Box<dyn ServerHandler>` 抹平类型差异，因此 handler 以泛型参数进入；实例名 → handler
/// 的解析见 [`spawn_builtin_transport_with_context`]。
///
/// **不覆写 `discover`**（spike Q1(b)/Q3(b)）：一旦 builtin handler 覆写 `discover` 返回
/// `method_not_found`，Auto 会在同一连接上回退 legacy，此后该连接上的 `tools/list` 恒被
/// `-32602` 拒绝（工具发现在生产链路上必然失败）。本模块不使用任何自定义 discover。
pub(crate) fn spawn_builtin_transport_with_handler<H>(
    instance: &str,
    handler: H,
) -> BuiltinTransport
where
    H: ServerHandler,
{
    spawn_builtin_link(instance, handler, |read| read)
}

/// 唯一的链路装配实现。`wrap_server_read` 只用于测试的线路观测（生产传恒等函数）。
fn spawn_builtin_link<R, H>(
    instance: &str,
    handler: H,
    wrap_server_read: impl FnOnce(ReadHalf<DuplexStream>) -> R,
) -> BuiltinTransport
where
    H: ServerHandler,
    R: tokio::io::AsyncRead + Send + Unpin + 'static,
{
    let (client_io, server_io) = tokio::io::duplex(BUILTIN_DUPLEX_BUF);
    let (server_read, server_write) = tokio::io::split(server_io);
    let server_read = wrap_server_read(server_read);
    let task_instance = instance.to_string();
    let handle = tokio::spawn(async move {
        match serve_server(handler, (server_read, server_write)).await {
            // `waiting()` 消费 running service；输入流 EOF / 对端关闭即返回 QuitReason。
            Ok(running) => match running.waiting().await {
                Ok(reason) => BuiltinServerExit::Quit(reason),
                Err(error) => BuiltinServerExit::TaskFailed(error.to_string()),
            },
            Err(error) => BuiltinServerExit::NotStarted(error.to_string()),
        }
    });
    BuiltinTransport {
        io: tokio::io::split(client_io),
        // 本层（链路装配）恒不挂 tick：tick 由 pool 的唯一 spawn 点
        // （`McpClientPool::spawn_builtin_transport`）在拿到本代 transport 后按实例名与
        // `tick_enabled` 决定是否挂上——tick 必须与**本代** server task 同生共死，因此
        // 不能在这里凭空生成。
        tick: None,
        server_task: BuiltinServerTask {
            instance: task_instance,
            handle,
        },
        #[cfg(not(target_os = "emscripten"))]
        workspace_tasks: None,
    }
}

/// 实例上下文 → handler 解析 + 链路装配（pool 的唯一 spawn 点调用的 seam）。
///
/// 调用方（[`crate::mcp::client::McpClientPool::spawn_builtin_transport`]）必须先解析实例与
/// 上下文；本函数重复实例校验，使直接调用本 seam 的代码也不可能为未注册实例建立传输。
///
/// 三步各有独立的 typed 收口，**顺序不可换**：
/// 1. 实例不在注册表 → [`BuiltinSpawnError::UnknownInstance`]；
/// 2. 注册表在册但上下文缺该实例所需输入 → [`BuiltinSpawnError::InstanceInputMissing`]。
///    本步必须在 dispatch **之前**：`builtin_server_handler` 的签名是
///    `Option<...>`，「输入缺失」与「handler 未接线」在那里无法区分，若让 dispatch 先跑，
///    缺输入的 cron / lsp 会被误报成 `HandlerNotWired`（把宿主的装配缺陷写成代码缺陷）；
/// 3. handler 未接线 → [`BuiltinSpawnError::HandlerNotWired`]：**不** panic、**不**静默降级
///    成 stdio / http、**不**伪造 ready 证据。`ctx.cwd` 是 artifact 实例的文件解析根
///    （web 忽略；cron / lsp 不经它取状态）。
#[cfg(not(target_os = "emscripten"))]
pub(crate) fn spawn_builtin_transport_with_context(
    instance: &str,
    ctx: &BuiltinInstanceContext,
    env: &std::collections::HashMap<String, String>,
) -> Result<BuiltinTransport, BuiltinSpawnError> {
    crate::mcp::transport::require_known_builtin_instance(instance).map_err(|_| {
        BuiltinSpawnError::UnknownInstance {
            instance: instance.to_string(),
        }
    })?;
    if !ctx.instance_input_ready(instance) {
        return Err(BuiltinSpawnError::InstanceInputMissing {
            instance: instance.to_string(),
        });
    }
    let handler =
        super::dispatch::builtin_server_handler_with_env(instance, ctx, env).ok_or_else(|| {
            BuiltinSpawnError::HandlerNotWired {
                instance: instance.to_string(),
            }
        })?;
    let workspace_tasks = match &handler {
        super::dispatch::BuiltinServerHandler::Workspace(workspace) => Some(workspace.clone()),
        _ => None,
    };
    let mut transport = spawn_builtin_transport_with_handler(instance, handler);
    transport.workspace_tasks = workspace_tasks;
    Ok(transport)
}

#[cfg(target_os = "emscripten")]
pub(crate) fn spawn_builtin_transport_with_context(
    instance: &str,
    _ctx: &BuiltinInstanceContext,
    _env: &std::collections::HashMap<String, String>,
) -> Result<BuiltinTransport, BuiltinSpawnError> {
    Err(BuiltinSpawnError::HandlerNotWired {
        instance: instance.to_owned(),
    })
}

/// 测试专用：把「本代的 tick + server task」组装成监督者。**仅测试用**，生产构造路径不变
/// （生产唯一构造点是 [`BuiltinTransport::into_parts`]；本函数不接线、不被生产路径调用）。
///
/// 存在的理由：`BuiltinServerTask` 的字段只对 `runtime` 及其子模块可见，而 `cron` 侧的代
/// 监督者用例（`mcp/builtin/cron_test.rs`）需要一个「立即收敛为 `Quit`」的 server task 来
/// 断言 [`BuiltinInstanceSupervisor::close`] 的两侧结论。这里开一个 `#[cfg(test)]` 的窄口子，
/// **不**为测试放宽生产字段可见性，也不新增第二条生产关闭路径。
#[cfg(test)]
pub(crate) fn test_supervisor(
    instance: &str,
    tick: Option<TickGuard>,
    server: JoinHandle<BuiltinServerExit>,
) -> BuiltinInstanceSupervisor {
    BuiltinInstanceSupervisor::new(
        instance.to_string(),
        BuiltinServerTask {
            instance: instance.to_string(),
            handle: server,
        },
        tick,
    )
}

/// 测试专用：与 [`spawn_builtin_transport_with_handler`] 同链路，额外在 server 读半挂
/// 线路观测（记录收到的 JSON-RPC method 序列）。
///
/// 用途（A13 子项 ④ 的观测面）：断言 namespace 路由不串（`mcp__web__*` 只落 web 的
/// handler），以及 modern 路径下线路**不含** `initialize` 帧——真实 `rmcp::serve_server`
/// 的默认 `discover` / `initialize` 没有插桩点，只能从线路取证（spike Q1(b)）。
#[cfg(test)]
pub(crate) fn spawn_builtin_transport_with_tap<H>(
    instance: &str,
    handler: H,
) -> (BuiltinTransport, std::sync::Arc<BuiltinWireLog>)
where
    H: ServerHandler,
{
    let log = std::sync::Arc::new(BuiltinWireLog::default());
    let transport = spawn_builtin_link(instance, handler, |read| {
        MethodTap::new(read, std::sync::Arc::clone(&log))
    });
    (transport, log)
}

/// 测试专用：server 侧读到的 JSON-RPC method 序列（线路级证据）。
#[cfg(test)]
#[derive(Debug, Default)]
pub(crate) struct BuiltinWireLog {
    methods: parking_lot::Mutex<Vec<String>>,
}

#[cfg(test)]
impl BuiltinWireLog {
    pub(crate) fn methods(&self) -> Vec<String> {
        self.methods.lock().clone()
    }

    fn record(&self, method: &str) {
        self.methods.lock().push(method.to_string());
    }
}

/// 测试专用 `AsyncRead` 透传包装：按行切分已读字节，记录每帧的 `method`。
#[cfg(test)]
struct MethodTap<R> {
    inner: R,
    log: std::sync::Arc<BuiltinWireLog>,
    pending: Vec<u8>,
}

#[cfg(test)]
impl<R> MethodTap<R> {
    fn new(inner: R, log: std::sync::Arc<BuiltinWireLog>) -> Self {
        Self {
            inner,
            log,
            pending: Vec::new(),
        }
    }

    fn observe(&mut self, bytes: &[u8]) {
        self.pending.extend_from_slice(bytes);
        while let Some(newline) = self.pending.iter().position(|byte| *byte == b'\n') {
            let line: Vec<u8> = self.pending.drain(..=newline).collect();
            let Ok(message) = serde_json::from_slice::<serde_json::Value>(&line) else {
                continue;
            };
            if let Some(method) = message.get("method").and_then(|value| value.as_str()) {
                self.log.record(method);
            }
        }
    }
}

#[cfg(test)]
impl<R: tokio::io::AsyncRead + Unpin> tokio::io::AsyncRead for MethodTap<R> {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let this = self.get_mut();
        let before = buf.filled().len();
        let poll = std::pin::Pin::new(&mut this.inner).poll_read(cx, buf);
        if buf.filled().len() > before {
            let observed = buf.filled()[before..].to_vec();
            this.observe(&observed);
        }
        poll
    }
}

#[cfg(test)]
#[path = "runtime_test.rs"]
mod tests;
