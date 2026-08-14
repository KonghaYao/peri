//! ws 传输层（F6）：outbound 连 server、单连接多路复用、指数退避重连、
//! 双向认证握手编排、帧读写（§3.1/§4.3/§7.1/§9.2）。
//!
//! - **连接**：`tokio_tungstenite::connect_async`（URL 路径 `/instance`）；发送/
//!   接收拆双任务，共用有界 `mpsc` 发送队列——单连接多路复用由 `Frame` 枚举
//!   天然承载（§3.1）；
//! - **指数退避重连**（§7.1）：base 起 ×2 → max 上限（默认 1s→60s）；连接成功
//!   且认证通过后重置为 base；不引入随机抖动【决策】（单机场景无惊群问题）；
//! - **心跳**：定时器在 hub（§4.2），transport 只负责帧收发；
//! - **关闭码策略**（§4.7）：收到 4502（配置性永久失败）→ `Stopped(ConfigFatal)`；
//!   认证失败（HMAC 校验失败，§9.2）→ `Stopped(AuthFailed)`；其余关闭码/网络
//!   错误 → `Disconnected` → 退避重连（4500/4501 是 server→client 关闭码，
//!   instance 侧不适用【决策】）；
//! - **入站校验**：每帧 `Frame::parse`（未知 tag → 计数，不 panic）+
//!   `whitelist::m1_check(tag, Role::Instance, Direction::Outbound)`（防异常帧）；
//! - **发送确认**：`send_acked` 等待 writer 实际写入成功（hub 据此推进
//!   `last_sent_seq`）；断线时队列中未写入帧的确认全部以 `SendError` 返回
//!   （帧未发出，hub 转缓冲路径，不丢帧）。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use futures::{SinkExt, StreamExt};
use tokio::sync::{mpsc, oneshot, watch};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;

use acp_hub_proto::conn::CLOSE_CONFIG_FATAL;
use acp_hub_proto::frame::{Frame, ProtoError};
use acp_hub_proto::instance::InstanceHello;
use acp_hub_proto::whitelist::{m1_check, Direction, M1Check, Role};

use crate::auth::{AuthError, AuthSession};

// ---------------------------------------------------------------------------
// 事件与错误
// ---------------------------------------------------------------------------

/// 停止自动重连的原因（§3 步骤 7 / §4.7 关闭码策略）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoppedReason {
    /// 认证失败（HMAC 校验失败 / 握手超时，§9.2 步骤 3）：不自动重连（防冒充
    /// server 反复投毒）。
    AuthFailed,
    /// server 以 4502 关闭（配置性永久失败，§4.7）：不自动重连。
    ConfigFatal,
    /// 本机优雅关闭（ctrl_c）。
    Shutdown,
}

/// transport → hub 的事件流。
#[derive(Debug)]
pub enum TransportEvent {
    /// ws 建立（认证前）。
    Connected,
    /// 认证通过（hub 才进入 READY/补推）。
    Authenticated,
    /// 断线（hub 切缓冲模式）。
    Disconnected,
    /// 停止自动重连（hub 应记录错误与审计日志后结束）。
    Stopped(StoppedReason),
    /// 入站帧（解析 + M1 校验通过后）。
    Frame(Box<Frame>),
    /// 握手超时（诊断信号；随后以 `Stopped(AuthFailed)` 结束）。
    AuthTimeout,
}

/// 发送错误。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SendError {
    /// transport 已停止（停止重连或优雅关闭）。
    #[error("transport 已停止")]
    Stopped,
    /// 发送队列已满（背压，§8.6 硬阈值）。由 [`TransportHandle::dispatch`]
    /// 处置为关闭通道 + [`SendError::Disconnected`]（走断线重连补推），
    /// 不直接暴露给 hub。
    #[error("发送队列已满")]
    QueueFull,
    /// 连接已断开（帧未写入）。
    #[error("连接断开")]
    Disconnected,
}

// ---------------------------------------------------------------------------
// 配置
// ---------------------------------------------------------------------------

/// transport 配置（hub 从 `InstanceConfig` 派生，§10）。
#[derive(Debug, Clone)]
pub struct TransportConfig {
    /// server ws 地址（`ws://host:port/instance`）。
    pub url: String,
    /// 握手超时（auth_response 等待，默认 10s【决策】）。
    pub auth_timeout: Duration,
    /// 重连退避起点（§7.1，默认 1s）。
    pub reconnect_base: Duration,
    /// 重连退避上限（§7.1，默认 60s）。
    pub reconnect_max: Duration,
}

/// 指数退避：×2 递增至 `max` 封顶（§7.1 序列 1s→2s→4s→…→60s，无抖动【决策】）。
pub fn next_backoff(current: Duration, max: Duration) -> Duration {
    let doubled = current.saturating_mul(2);
    if doubled >= max {
        max
    } else if doubled > current {
        doubled
    } else {
        max // 溢出防御（current 为 0 或 saturating 退化）
    }
}

// ---------------------------------------------------------------------------
// 发送队列 / 句柄
// ---------------------------------------------------------------------------

/// 入队一帧（非阻塞，`try_send` 语义）：
/// channel 满 → [`SendError::QueueFull`]（由 [`TransportHandle::dispatch`]
/// 处置：drop sender 关闭通道 → writer 退出 → 断线重连 → 已入缓冲帧经
/// buffer_sync 补推）；通道已关 → Disconnected。不用 `send().await`——否则
/// writer 被 TCP 背压阻塞时 hub 主循环会无限挂起（心跳停发）。
fn try_dispatch(tx: &mpsc::Sender<Outbound>, out: Outbound) -> Result<(), SendError> {
    match tx.try_send(out) {
        Ok(()) => Ok(()),
        Err(mpsc::error::TrySendError::Full(_)) => Err(SendError::QueueFull),
        Err(mpsc::error::TrySendError::Closed(_)) => Err(SendError::Disconnected),
    }
}

/// 发送队列条目：普通帧（fire-and-forget）或需写入确认的帧。
enum Outbound {
    Frame(Frame),
    Acked(Frame, oneshot::Sender<Result<(), SendError>>),
}

/// 向 transport 发送帧的句柄（hub 持有；克隆共享同一队列）。
#[derive(Clone)]
pub struct TransportHandle {
    tx: Arc<tokio::sync::Mutex<Option<mpsc::Sender<Outbound>>>>,
    connected: Arc<AtomicBool>,
    authenticated: Arc<AtomicBool>,
    cancel: watch::Sender<bool>,
    /// 发送队列上限（帧循环创建 channel 时使用）。
    queue_cap: usize,
}

impl TransportHandle {
    /// 创建句柄（`frame_queue` 为有界发送队列上限）。
    pub fn new(frame_queue: usize) -> (Self, watch::Receiver<bool>) {
        let (cancel, _rx) = watch::channel(false);
        (
            TransportHandle {
                tx: Arc::new(tokio::sync::Mutex::new(None)),
                connected: Arc::new(AtomicBool::new(false)),
                authenticated: Arc::new(AtomicBool::new(false)),
                cancel,
                queue_cap: frame_queue,
            },
            _rx,
        )
    }

    /// 发送帧（不等待写入确认；hello/heartbeat/ack 用）。
    pub async fn send(&self, frame: Frame) -> Result<(), SendError> {
        self.dispatch(Outbound::Frame(frame)).await
    }

    /// 发送帧并等待 writer 实际写入成功（instance/event 与 buffer_sync 批次用；
    /// hub 据此推进 `last_sent_seq`）。断线时返回 `SendError`（帧未发出）。
    pub async fn send_acked(&self, frame: Frame) -> Result<(), SendError> {
        let (ack_tx, ack_rx) = oneshot::channel();
        self.dispatch(Outbound::Acked(frame, ack_tx)).await?;
        ack_rx.await.map_err(|_| SendError::Disconnected)?
    }

    async fn dispatch(&self, out: Outbound) -> Result<(), SendError> {
        let mut guard = self.tx.lock().await;
        let Some(tx) = guard.as_ref() else {
            return Err(SendError::Disconnected);
        };
        match try_dispatch(tx, out) {
            Ok(()) => Ok(()),
            // §8.6 硬背压：drop 唯一 sender → 通道关闭 → writer 退出 → 断线
            // 重连 → 已入缓冲帧经 buffer_sync 补推。不用 `send().await`：
            // 否则 writer 被 TCP 背压阻塞时 hub 主循环会无限挂起（心跳停发）。
            Err(SendError::QueueFull) => {
                tracing::warn!(target: "acp_hub::instance", "发送队列已满（§8.6 硬阈值），关闭连接走重连补推");
                *guard = None;
                Err(SendError::Disconnected)
            }
            Err(e) => Err(e),
        }
    }

    /// ws 是否已建立（hub 据此决定实时/缓冲）。
    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Relaxed)
    }

    /// 认证是否通过。
    pub fn is_authenticated(&self) -> bool {
        self.authenticated.load(Ordering::Relaxed)
    }

    /// 请求优雅停止（transport 循环退出）。
    pub fn shutdown(&self) {
        let _ = self.cancel.send(true);
    }
}

// ---------------------------------------------------------------------------
// 连接循环
// ---------------------------------------------------------------------------

/// 帧读写循环的退出原因（内部）。
enum LoopExit {
    Disconnected,
    Stopped(StoppedReason),
}

/// transport 主循环：连接 → 握手 → 帧读写 → 断线退避重连，直至停止。
///
/// `make_hello` 每次连接调用：返回 (认证会话, hello)——**每次连接新 nonce**
/// （§9.2 重连重新握手）；认证会话的 nonce 与 hello 绑定，用于校验随后的
/// `auth_response`。
pub async fn run<F>(
    config: TransportConfig,
    make_hello: F,
    events: mpsc::Sender<TransportEvent>,
    handle: TransportHandle,
    mut cancel: watch::Receiver<bool>,
) -> anyhow::Result<()>
where
    F: Fn() -> (AuthSession, InstanceHello) + Send + Sync + 'static,
{
    let mut backoff = config.reconnect_base;
    loop {
        if *cancel.borrow() {
            return Ok(());
        }
        // 连接（失败 → 退避重试，不发事件：从未建立连接）。
        let ws = match tokio_tungstenite::connect_async(&config.url).await {
            Ok((ws, _)) => ws,
            Err(e) => {
                tracing::warn!(target: "acp_hub::instance", error = %e,
                    backoff_ms = backoff.as_millis(), "连接 server 失败，退避重试");
                if sleep_cancellable(&mut cancel, backoff).await.is_err() {
                    return Ok(());
                }
                backoff = next_backoff(backoff, config.reconnect_max);
                continue;
            }
        };

        handle.connected.store(true, Ordering::Relaxed);
        let _ = events.send(TransportEvent::Connected).await;

        // 握手（§9.2）：hello（新 nonce）→ auth_response 校验（超时 → 停止）。
        let ws = match handshake(ws, &config, &make_hello, &events, &handle, &mut cancel).await {
            HandshakeOutcome::Authenticated(ws) => {
                tracing::info!(target: "acp_hub::instance", "认证通过");
                backoff = config.reconnect_base;
                Some(*ws)
            }
            HandshakeOutcome::Stopped(reason) => {
                handle.connected.store(false, Ordering::Relaxed);
                let _ = events.send(TransportEvent::Stopped(reason)).await;
                return Ok(());
            }
            HandshakeOutcome::Retry => {
                handle.connected.store(false, Ordering::Relaxed);
                let _ = events.send(TransportEvent::Disconnected).await;
                if sleep_cancellable(&mut cancel, backoff).await.is_err() {
                    return Ok(());
                }
                backoff = next_backoff(backoff, config.reconnect_max);
                continue;
            }
        };
        let Some(ws) = ws else {
            unreachable!("Authenticated 分支必有 ws")
        };

        // 帧读写循环（writer + reader 双任务）。
        let exit = frame_loop(ws, &events, &handle, &mut cancel).await;

        handle.connected.store(false, Ordering::Relaxed);
        handle.authenticated.store(false, Ordering::Relaxed);
        {
            let mut guard = handle.tx.lock().await;
            *guard = None;
        }
        let _ = events.send(TransportEvent::Disconnected).await;

        match exit {
            LoopExit::Stopped(reason) => {
                let _ = events.send(TransportEvent::Stopped(reason)).await;
                return Ok(());
            }
            LoopExit::Disconnected => {}
        }

        if sleep_cancellable(&mut cancel, backoff).await.is_err() {
            return Ok(());
        }
        backoff = next_backoff(backoff, config.reconnect_max);
    }
}

/// 可取消的退避等待。
async fn sleep_cancellable(cancel: &mut watch::Receiver<bool>, d: Duration) -> Result<(), ()> {
    if *cancel.borrow() {
        return Err(());
    }
    tokio::select! {
        _ = tokio::time::sleep(d) => Ok(()),
        _ = cancel.changed() => Err(()),
    }
}

enum HandshakeOutcome {
    /// 认证通过，进入帧循环。
    Authenticated(Box<WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>>),
    /// 停止（AuthFailed / ConfigFatal）。
    Stopped(StoppedReason),
    /// 断开重连（连接建立但握手未完成：畸形帧/关闭）。
    Retry,
}

async fn handshake<F>(
    mut ws: WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    config: &TransportConfig,
    make_hello: &F,
    events: &mpsc::Sender<TransportEvent>,
    handle: &TransportHandle,
    cancel: &mut watch::Receiver<bool>,
) -> HandshakeOutcome
where
    F: Fn() -> (AuthSession, InstanceHello) + Send + Sync,
{
    let (session, hello) = make_hello();
    let hello_frame = Frame::InstanceHello(hello);
    let text = match serde_json::to_string(&hello_frame) {
        Ok(t) => t,
        Err(e) => {
            tracing::error!(target: "acp_hub::instance", "hello 序列化失败: {e}");
            return HandshakeOutcome::Retry;
        }
    };
    // 发送挂起（server 不读 TCP）或写失败：视为握手失败，断开重连。
    let sent = match tokio::time::timeout(config.auth_timeout, ws.send(Message::Text(text.into())))
        .await
    {
        Ok(Ok(())) => true,
        Ok(Err(_)) | Err(_) => false,
    };
    if !sent {
        tracing::warn!(target: "acp_hub::instance", "hello 发送失败/超时，断开重连");
        return HandshakeOutcome::Retry;
    }

    // 等待 auth_response（超时 → 审计 + 停止，不重连，§9.2 步骤 3）。
    let timeout = config.auth_timeout;
    let wait = tokio::time::timeout(timeout, wait_auth_response(&mut ws, cancel)).await;
    match wait {
        Err(_elapsed) => {
            tracing::error!(target: "acp_hub::instance", "握手超时（{timeout:?}），断开且不自动重连");
            let _ = events.send(TransportEvent::AuthTimeout).await;
            close_with_timeout(&mut ws, "auth_timeout").await;
            HandshakeOutcome::Stopped(StoppedReason::AuthFailed)
        }
        Ok(AuthWait::Response(resp)) => match session.verify_auth_response(&resp) {
            Ok(()) => {
                handle.authenticated.store(true, Ordering::Relaxed);
                let _ = events.send(TransportEvent::Authenticated).await;
                HandshakeOutcome::Authenticated(Box::new(ws))
            }
            Err(e) => {
                audit_auth_failure(&e);
                close_with_timeout(&mut ws, "auth_failed").await;
                HandshakeOutcome::Stopped(StoppedReason::AuthFailed)
            }
        },
        Ok(AuthWait::ProtocolViolation) => {
            tracing::warn!(target: "acp_hub::instance", "握手阶段收到非 auth_response 帧/关闭，断开重连");
            HandshakeOutcome::Retry
        }
        Ok(AuthWait::CloseConfigFatal) => {
            tracing::error!(target: "acp_hub::instance", "握手阶段 server 以 4502 关闭（配置性失败），停止重连");
            HandshakeOutcome::Stopped(StoppedReason::ConfigFatal)
        }
        Ok(AuthWait::Cancelled) => HandshakeOutcome::Stopped(StoppedReason::Shutdown),
    }
}

/// 主动关闭（4502 语义）：tungstenite `close` 等待对端 Close 应答，加超时保护
/// 防止对端不响应时握手失败路径挂起。
async fn close_with_timeout(
    ws: &mut WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    reason: &str,
) {
    let _ = tokio::time::timeout(
        Duration::from_secs(1),
        ws.close(Some(tokio_tungstenite::tungstenite::protocol::CloseFrame {
            code: CLOSE_CONFIG_FATAL.into(),
            reason: reason.into(),
        })),
    )
    .await;
}

/// 握手等待结果。
enum AuthWait {
    /// 收到合法 auth_response。
    Response(acp_hub_proto::conn::AuthResponse),
    /// 收到非 auth_response 帧 / 畸形帧 / 关闭（重连）。
    ProtocolViolation,
    /// server 以 4502 关闭（配置性永久失败，§4.7）——握手阶段同样识别。
    CloseConfigFatal,
    /// 优雅关闭信号。
    Cancelled,
}

/// 审计日志（token_id 级别，不含 token 本体，§9.2 步骤 3 / §9.3）。
fn audit_auth_failure(err: &AuthError) {
    tracing::error!(target: "acp_hub::instance", auth_failed_total = 1, reason = %err,
        "认证失败（HMAC 校验），断开且不自动重连（审计计数）");
}

/// 读取并解析 auth_response（任何其他帧/关闭 → 协议违反）。
async fn wait_auth_response(
    ws: &mut WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    cancel: &mut watch::Receiver<bool>,
) -> AuthWait {
    loop {
        tokio::select! {
            biased;
            _ = cancel.changed() => return AuthWait::Cancelled,
            msg = ws.next() => {
                match msg {
                    Some(Ok(Message::Text(t))) => match Frame::parse(&t) {
                        Ok(Frame::AuthResponse(r)) => return AuthWait::Response(r),
                        Ok(_) => return AuthWait::ProtocolViolation,
                        Err(e) => {
                            count_inbound_problem(&e);
                            return AuthWait::ProtocolViolation;
                        }
                    },
                    Some(Ok(Message::Close(frame))) => {
                        let code = frame.as_ref().map(|f| u16::from(f.code));
                        return if code == Some(CLOSE_CONFIG_FATAL) {
                            AuthWait::CloseConfigFatal
                        } else {
                            AuthWait::ProtocolViolation
                        };
                    }
                    Some(Ok(_)) => {}
                    Some(Err(_)) | None => return AuthWait::ProtocolViolation,
                }
            }
        }
    }
}

/// 入站帧问题计数（未知 tag / 畸形 / 方向拒绝，不 panic，§4.3）。
fn count_inbound_problem(err: &ProtoError) {
    match err {
        ProtoError::Unsupported(tag) => {
            tracing::warn!(target: "acp_hub::instance", tag, "入站未知/非 M1 帧（计数，丢弃）");
        }
        ProtoError::Malformed(e) => {
            tracing::warn!(target: "acp_hub::instance", error = %e, "入站畸形帧（计数，丢弃）");
        }
        ProtoError::DirectionRejected(tag) => {
            tracing::warn!(target: "acp_hub::instance", tag, "入站帧方向违反（计数，丢弃）");
        }
    }
}

/// 帧读写循环：writer（消费发送队列）+ reader（解析入站帧）双任务。
///
/// writer 断线时：先 close 队列（挡住新发送）再 drain 清空——所有未写入的
/// `Acked` 帧以 `SendError::Disconnected` 回执（hub 转缓冲路径）。writer 的
/// 清理在 frame_loop 返回后由「channel close」驱动完成（挂起的 `send_acked`
/// 会被回执唤醒，不会悬挂）。
async fn frame_loop(
    ws: WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    events: &mpsc::Sender<TransportEvent>,
    handle: &TransportHandle,
    cancel: &mut watch::Receiver<bool>,
) -> LoopExit {
    use futures::StreamExt;
    let (mut write_half, mut read_half) = ws.split();
    let mut out_rx = {
        let mut guard = handle.tx.lock().await;
        let (tx, rx) = mpsc::channel(handle.queue_cap);
        *guard = Some(tx);
        rx
    };

    let (fail_tx, mut fail_rx) = mpsc::channel::<LoopExit>(1);
    let w_cancel = cancel.clone();

    // writer task（channel close 或 cancel 时退出并清理队列）。
    let writer = tokio::spawn(async move {
        let mut local_cancel = w_cancel;
        loop {
            tokio::select! {
                out = out_rx.recv() => {
                    match out {
                        Some(Outbound::Frame(f)) => {
                            if write_frame(&mut write_half, &f).await.is_err() {
                                break;
                            }
                        }
                        Some(Outbound::Acked(f, ack)) => {
                            match write_frame(&mut write_half, &f).await {
                                Ok(()) => { let _ = ack.send(Ok(())); }
                                Err(_) => {
                                    let _ = ack.send(Err(SendError::Disconnected));
                                    break;
                                }
                            }
                        }
                        None => break,
                    }
                }
                _ = local_cancel.changed() => break,
            }
        }
        // 断线清理：close 队列（新发送立即 Err）→ drain 未写入帧（ack 回执 Err）。
        out_rx.close();
        while let Ok(out) = out_rx.try_recv() {
            if let Outbound::Acked(_, ack) = out {
                let _ = ack.send(Err(SendError::Disconnected));
            }
        }
        let _ = fail_tx.send(LoopExit::Disconnected).await;
    });

    // reader：主任务（同时监听 writer 失败与取消）。
    let exit = loop {
        tokio::select! {
            msg = read_half.next() => {
                match msg {
                    Some(Ok(Message::Text(t))) => {
                        match Frame::parse(&t) {
                            Ok(frame) => {
                                if m1_check(frame.tag(), Role::Instance, Direction::Outbound) == M1Check::Allowed {
                                    if events.send(TransportEvent::Frame(Box::new(frame))).await.is_err() {
                                        break LoopExit::Stopped(StoppedReason::Shutdown);
                                    }
                                } else {
                                    count_inbound_problem(&ProtoError::DirectionRejected(frame.tag().0.to_string()));
                                }
                            }
                            Err(e) => count_inbound_problem(&e),
                        }
                    }
                    Some(Ok(Message::Close(frame))) => {
                        let code = frame.as_ref().map(|f| u16::from(f.code));
                        break match code {
                            Some(c) if c == CLOSE_CONFIG_FATAL => {
                                LoopExit::Stopped(StoppedReason::ConfigFatal)
                            }
                            _ => LoopExit::Disconnected,
                        };
                    }
                    Some(Ok(_)) => {}
                    Some(Err(_)) | None => break LoopExit::Disconnected,
                }
            }
            exit = fail_rx.recv() => {
                break exit.unwrap_or(LoopExit::Disconnected);
            }
            _ = cancel.changed() => {
                break LoopExit::Stopped(StoppedReason::Shutdown);
            }
        }
    };

    // 关闭发送队列：writer 退出并回执所有未写入的 Acked（不等待，回执异步完成）。
    {
        let mut guard = handle.tx.lock().await;
        *guard = None;
    }
    drop(writer); // 分离 writer task（清理在后台完成）
    exit
}

/// 单帧写超时（【决策】10s，对齐 §16 spawn/initialize 超时组）：writer 被 TCP
/// 背压阻塞超过此时长视为连接死亡 → 断线重连（§8.6 硬阈值语义，防「server
/// 停止读取 → 队列填满 → 心跳停发」的静默死锁）。
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);

/// 序列化并写入一帧（文本消息）。写超时/失败 → `Err`（调用方断开重连；
/// 超时后 sink 状态不可复用，不得继续写）。
async fn write_frame<S>(sink: &mut S, frame: &Frame) -> anyhow::Result<()>
where
    S: futures::Sink<Message, Error = tokio_tungstenite::tungstenite::Error> + Unpin,
{
    let text = serde_json::to_string(frame).context("帧序列化失败")?;
    tokio::time::timeout(WRITE_TIMEOUT, sink.send(Message::Text(text.into())))
        .await
        .map_err(|_| anyhow::anyhow!("ws 写超时（{WRITE_TIMEOUT:?}）"))?
        .context("ws 写失败")
}

#[cfg(test)]
#[path = "transport_test.rs"]
mod transport_test;
