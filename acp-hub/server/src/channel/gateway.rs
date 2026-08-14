//! Gateway：ws 生命周期（架构 §4.6/§4.7/§9.2/§9.5，设计稿
//! `f5-channel-control.md` §10）。
//!
//! ws 入口：accept → 回环检查（§9.5）→ 静态分流（非 ws 的 HTTP GET 交
//! `crate::web` 验证台，§web）→ 配额（§8.6）→ 认证 → 角色分派 → 快照时序
//! （client）/机器会话（instance）→ 心跳接线 → 断链清理。
//!
//! 客户端时序（§4.6）：配额 → `auth` → 订阅（`ysync.subscribe`）→ 打开/恢复
//! Doc（`DocManager::open_chat` 幂等）→ 推全量快照（StoreSink 镜像，
//! 携带 `projection_version`）→ `ready` 握手 → `mark_ready` → flush 缓冲
//! Action → 帧循环 + 心跳（`HeartbeatDriver`，pong 超时 4501）。
//!
//! instance 时序（§9.2/§4.5）：首帧 `instance/hello` → 双向认证 →
//! `auth_response` 下发 → `InstanceRegistry::on_hello`（fencing + 注册）→
//! 帧循环（event/buffer_sync/heartbeat/ack/process_exit）→ 断开 →
//! `on_disconnect` + `RelayEventHandler::on_instance_disconnect`（§8.2）。

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use futures::{SinkExt as _, StreamExt as _};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, Mutex};
use tokio_tungstenite::accept_hdr_async;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;
use tracing::{debug, info, warn};

use acp_hub_proto::ack::{ActionError, ErrorCode};
use acp_hub_proto::action::ActionEnvelope;
use acp_hub_proto::conn::Auth;
use acp_hub_proto::frame::{Frame, ProtoError};
use acp_hub_proto::instance::InstanceHello;
use acp_hub_proto::schema::{InstanceStatus, InstanceView};
use acp_hub_proto::whitelist::{m1_allows_action_type, m1_check, Direction, Role};

use crate::auth::audit::audit;
use crate::auth::AuthService;
use crate::channel::OutboundMsg;
use crate::channel::RelayEventHandler;
use crate::channel::{ChannelDeps, ChatChannel, DispatchOutcome};
use crate::channel::{ConnId, ConnectionRegistry};
use crate::config::Config;
use crate::control::HeartbeatDriver;
use crate::control::StoreSink;
use crate::control::{InstanceAck, InstanceConn};

use crate::state::doc_manager::DocManager;
use crate::state::registry::RegistryState;

/// 首帧等待超时（§4.6 步骤 1 前：10s 无首帧断开）。
pub const FIRST_FRAME_TIMEOUT: Duration = Duration::from_secs(10);

/// HTTP 头部窥探补齐上限（§web：首段未含 `\r\n\r\n` 时短等碎片；超过即按
/// 非 ws 处理。正常 GET/升级请求首段即完整，该上限只兜底慢发包场景）。
const HEAD_PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// 认证前占位 token_id（§5「认证前占位」；认证成功后 upgrade 替换）。
const PENDING_TOKEN_ID: &str = "<pending-auth>";

/// gateway 错误。
#[derive(Debug, thiserror::Error)]
pub enum GatewayError {
    /// accept 循环错误。
    #[error("accept error: {0}")]
    Accept(std::io::Error),
}

/// ws 入口：accept → 配额/回环检查 → 认证 → 角色分派 → 快照时序/机器会话。
#[derive(Clone)]
pub struct Gateway {
    cfg: Arc<Config>,
    auth: Arc<Mutex<AuthService>>,
    conns: Arc<ConnectionRegistry>,
    deps: ChannelDeps,
    relay: Arc<RelayEventHandler>,
    doc: Arc<DocManager>,
    sink: Arc<StoreSink>,
    registry: RegistryState,
    heartbeat_interval: Duration,
    heartbeat_timeout: Duration,
    auth_setup: crate::web::BrowserAuthSetup,
}

impl Gateway {
    /// 装配（hub 调用；心跳参数取 §16 默认 5s，pong 超时 = 3×interval）。
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        cfg: Arc<Config>,
        auth: Arc<Mutex<AuthService>>,
        conns: Arc<ConnectionRegistry>,
        deps: ChannelDeps,
        relay: Arc<RelayEventHandler>,
        doc: Arc<DocManager>,
        sink: Arc<StoreSink>,
        registry: RegistryState,
    ) -> Self {
        let heartbeat_interval = cfg.heartbeat_interval;
        let heartbeat_timeout = heartbeat_interval * 3;
        let auth_setup = crate::web::BrowserAuthSetup::from_config(&cfg);
        Gateway {
            cfg,
            auth,
            conns,
            deps,
            relay,
            doc,
            sink,
            registry,
            heartbeat_interval,
            heartbeat_timeout,
            auth_setup,
        }
    }

    /// Degraded 判定（§17.2）：非 Healthy 时拒绝新 committed 承诺（gateway
    /// 分派 action 前检查；hub 也暴露同一入口）。
    pub fn can_accept_committed(&self) -> bool {
        matches!(
            self.registry.global_status(),
            acp_hub_proto::schema::GlobalStatus::Healthy
        )
    }

    /// accept 循环（hub 装配调用）。
    pub async fn run(&self, listener: TcpListener) -> Result<(), GatewayError> {
        loop {
            let (stream, peer) = match listener.accept().await {
                Ok(x) => x,
                Err(e) => {
                    warn!(error = ?e, "accept failed");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                }
            };
            let this = self.clone();
            tokio::spawn(async move {
                this.connection_task(stream, peer).await;
            });
        }
    }

    /// 连接任务（每连接一个）。
    #[allow(clippy::result_large_err)] // tungstenite handshake callback fixes the public error shape.
    async fn connection_task(&self, stream: TcpStream, peer: SocketAddr) {
        // 1. 非回环拒绝（§9.5：Config::allow_peer；不进注册表）。
        if !self.cfg.allow_peer(&peer) {
            warn!(
                peer = %peer,
                "connection rejected: non-loopback (allow_non_loopback=false, §9.5)"
            );
            drop(stream);
            return;
        }
        // 2. HTTP 分支（内嵌 Web + cookie auth bootstrap）：非 ws 升级请求
        // 交给 bounded HTTP surface；ws 升级
        //    请求原样走后续握手。窥探（peek 不消费数据，握手字节不受影响）
        //    首段：定位 `\r\n\r\n` 后查 `upgrade: websocket`；头部碎片未齐时
        //    短等补齐（HEAD_PROBE_TIMEOUT），避免把碎片到达的 ws 握手误判。
        let mut peeked = [0u8; 4096];
        let mut head_len;
        let head_end = {
            let deadline = tokio::time::Instant::now() + HEAD_PROBE_TIMEOUT;
            loop {
                head_len = match stream.peek(&mut peeked).await {
                    Ok(n) => n,
                    Err(e) => {
                        debug!(peer = %peer, error = ?e, "peek failed");
                        drop(stream);
                        return;
                    }
                };
                match crate::web::header_end(&peeked[..head_len]) {
                    Some(end) => break Some(end),
                    // EOF / 缓冲已满 / 超时仍未齐 → 按非 ws 处理（serve 兜底）。
                    None if head_len == 0
                        || head_len == peeked.len()
                        || tokio::time::Instant::now() >= deadline =>
                    {
                        break None;
                    }
                    None => tokio::time::sleep(Duration::from_millis(20)).await,
                }
            }
        };
        let is_ws = match head_end {
            Some(end) => crate::web::is_ws_upgrade(&peeked[..end]),
            None => false,
        };
        if !is_ws {
            // HTTP 分支：不进配额/注册表（§8.6 只面向 ws 连接）。
            if let Err(e) =
                crate::web::serve_http(stream, peer, self.auth.clone(), self.auth_setup.clone())
                    .await
            {
                debug!(peer = %peer, error = ?e, "static serve failed");
            }
            return;
        }
        // 3. 配额检查（认证**前**占位，§8.6：防未认证连接占满配额）。
        let pending_ctx = crate::auth::ConnectionCtx {
            token_id: PENDING_TOKEN_ID.to_string(),
            role: crate::auth::TokenRole::Full,
            name: String::new(),
            peer,
            hostname: None,
            established_at: chrono::Utc::now(),
        };
        let conn_id = match self.conns.register(pending_ctx) {
            Ok(h) => h.id,
            // 1013 配额超限（§4.7：退避重连）。
            Err(_) => {
                drop(stream);
                return;
            }
        };

        // 4. ws 握手。
        let cookie_principal = Arc::new(std::sync::Mutex::new(None::<String>));
        let captured = cookie_principal.clone();
        let ws = match accept_hdr_async(
            stream,
            move |req: &tokio_tungstenite::tungstenite::handshake::server::Request,
                  resp: tokio_tungstenite::tungstenite::handshake::server::Response| {
                let host = req
                    .headers()
                    .get("host")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or_default();
                let origin = req.headers().get("origin").and_then(|v| v.to_str().ok());
                if !crate::web::valid_loopback_host(host)
                    || origin
                        .map(|o| o != format!("http://{host}"))
                        .unwrap_or(false)
                {
                    return Err(
                        tokio_tungstenite::tungstenite::handshake::server::ErrorResponse::new(
                            Some("forbidden".into()),
                        ),
                    );
                }
                if let Some(cookie) = req
                    .headers()
                    .get("cookie")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| crate::web::cookie_value(v, crate::auth::BROWSER_COOKIE))
                {
                    *captured.lock().unwrap() = Some(cookie);
                }
                Ok(resp)
            },
        )
        .await
        {
            Ok(ws) => ws,
            Err(e) => {
                debug!(peer = %peer, error = ?e, "ws handshake failed");
                self.conns.unregister(conn_id);
                return;
            }
        };
        let (mut ws_sink, mut ws_stream) = ws.split();

        let captured_cookie = { cookie_principal.lock().unwrap().clone() };
        let cookie_ctx = match captured_cookie {
            Some(sid) => match self.auth.lock().await.validate_browser_session(&sid, peer) {
                Ok(ctx) => Some((sid, ctx)),
                Err(_) => {
                    self.finish_connection(conn_id, &mut ws_sink, 4502, "invalid browser session")
                        .await;
                    return;
                }
            },
            None => None,
        };

        // 5. Cookie-authenticated clients may start directly with subscribe/action.
        let first = match tokio::time::timeout(FIRST_FRAME_TIMEOUT, ws_stream.next()).await {
            Ok(Some(Ok(Message::Text(text)))) => match Frame::parse(&text) {
                Ok(f) => f,
                Err(_) => {
                    self.finish_connection(conn_id, &mut ws_sink, 1011, "malformed first frame")
                        .await;
                    return;
                }
            },
            Ok(Some(Ok(_))) => {
                self.finish_connection(conn_id, &mut ws_sink, 1011, "first frame must be text")
                    .await;
                return;
            }
            Ok(Some(Err(e))) => {
                debug!(peer = %peer, error = ?e, "first frame read error");
                self.conns.unregister(conn_id);
                return;
            }
            Ok(None) | Err(_) => {
                self.finish_connection(conn_id, &mut ws_sink, 1011, "first frame timeout (10s)")
                    .await;
                return;
            }
        };

        // 6. 角色分派。
        if let Some((sid, ctx)) = cookie_ctx {
            self.handle_authenticated_client_connection(
                conn_id,
                ws_sink,
                ws_stream,
                ctx,
                Some(sid),
                Some(first),
            )
            .await;
            return;
        }
        match first {
            Frame::Auth(auth) => {
                self.handle_client_connection(conn_id, ws_sink, ws_stream, peer, auth)
                    .await;
            }
            Frame::InstanceHello(hello) => {
                self.handle_instance_connection(conn_id, ws_sink, ws_stream, peer, hello)
                    .await;
            }
            _ => {
                self.finish_connection(
                    conn_id,
                    &mut ws_sink,
                    1011,
                    "first frame must be auth or instance/hello",
                )
                .await;
            }
        }
    }

    /// 客户端连接：认证 → 快照时序 → 帧循环 + 心跳。
    async fn handle_client_connection(
        &self,
        conn_id: ConnId,
        mut ws_sink: futures::stream::SplitSink<WebSocketStream<TcpStream>, Message>,
        ws_stream: futures::stream::SplitStream<WebSocketStream<TcpStream>>,
        peer: SocketAddr,
        auth: Auth,
    ) {
        // 认证（§4.6 步骤 3；失败断开 + 计数在 AuthService 内，§9.2）。
        let ctx = {
            let mut auth_service = self.auth.lock().await;
            match auth_service.authenticate_client(&auth, peer).await {
                Ok(ctx) => ctx,
                Err(e) => {
                    warn!(peer = %peer, error = ?e, "client auth failed");
                    let _ = ws_sink
                        .send(Message::Close(Some(CloseFrame {
                            code: close_code(4502),
                            reason: "authentication failed".into(),
                        })))
                        .await;
                    self.conns.unregister(conn_id);
                    return;
                }
            }
        };
        self.handle_authenticated_client_connection(conn_id, ws_sink, ws_stream, ctx, None, None)
            .await;
    }

    async fn handle_authenticated_client_connection(
        &self,
        conn_id: ConnId,
        mut ws_sink: futures::stream::SplitSink<WebSocketStream<TcpStream>, Message>,
        mut ws_stream: futures::stream::SplitStream<WebSocketStream<TcpStream>>,
        ctx: crate::auth::ConnectionCtx,
        browser_session: Option<String>,
        first_frame: Option<Frame>,
    ) {
        let peer = ctx.peer;
        let (out_tx, mut out_rx) = mpsc::channel::<OutboundMsg>(256);
        self.conns.upgrade(conn_id, ctx.clone());
        audit(
            "conn.open",
            None,
            Some(&ctx.token_id),
            "ok",
            Duration::ZERO,
            None,
        );
        info!(conn_id, token_id = %ctx.token_id, peer = %peer, "client connected");

        let mut channel = ChatChannel::new(ctx.clone());
        let mut heartbeat = HeartbeatDriver::new(self.heartbeat_interval, self.heartbeat_timeout);
        let mut ticker = tokio::time::interval(self.heartbeat_interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        ticker.tick().await;

        if let Some(frame) = first_frame {
            let outcome = channel.dispatch(frame, &self.deps, out_tx.clone()).await;
            if let Some(code) = self
                .apply_outcome(&mut channel, &out_tx, conn_id, outcome)
                .await
            {
                self.finish_connection(conn_id, &mut ws_sink, code, "client connection closed")
                    .await;
                return;
            }
        }

        let deps = self.deps.clone();
        let close_code = loop {
            tokio::select! {
                msg = ws_stream.next() => {
                    let Some(msg) = msg else { break 1011 };
                    match msg {
                        Ok(Message::Text(text)) => {
                            let frame = match Frame::parse(&text) {
                                Ok(f) => f,
                                Err(e) => {
                                    // §4.8：未知/畸形帧 → UNSUPPORTED_FRAME
                                    // （不静默，不 panic；脱敏：不回显正文）。
                                    let _ = out_tx.send(OutboundMsg::Frame(
                                        unsupported_error(&e))).await;
                                    continue;
                                }
                            };
                            // M1 白名单 + 方向（§4.8；Pong 已放行——心跳回执）。
                            if !matches!(frame, Frame::Pong(_))
                                && !matches!(m1_check(frame.tag(), Role::Client, Direction::Inbound),
                                    acp_hub_proto::whitelist::M1Check::Allowed)
                            {
                                let _ = out_tx.send(OutboundMsg::Frame(
                                    unsupported_error(&ProtoError::DirectionRejected(
                                        frame.tag().0.to_string())))).await;
                                continue;
                            }
                            if let Frame::Pong(_) = frame {
                                heartbeat.on_pong();
                                continue;
                            }
                            // M1 action type 收窄（§4.8：`session/load`（M2）、
                            // `events/*`（M3）类型保留但白名单外 → UNSUPPORTED_FRAME，
                            // 不静默）。先于 §17.2 Degraded 检查（协议层检查先于
                            // 资源状态检查——Degraded 拒绝是 retryable 语义，而
                            // 方法不支持是确定语义，二者不可混淆）。
                            if let Frame::Action(action) = &frame {
                                if !m1_allows_action_type(action.type_str()) {
                                    let command_id = match action {
                                        ActionEnvelope::ProjectCreate { command_id, .. }
                                        | ActionEnvelope::ProjectArchive { command_id, .. }
                                        | ActionEnvelope::ProjectRestore { command_id, .. }
                                        | ActionEnvelope::ProjectRename { command_id, .. }
                                        | ActionEnvelope::PersistedSessionCreate { command_id, .. }
                                        | ActionEnvelope::PersistedSessionOpen { command_id, .. }
                                        | ActionEnvelope::PersistedSessionRename { command_id, .. }
                                        | ActionEnvelope::PersistedSessionArchive { command_id, .. }
                                        | ActionEnvelope::PersistedSessionRestore { command_id, .. }
                                        | ActionEnvelope::PersistedSessionImport { command_id, .. }
                                        | ActionEnvelope::PersistedSessionDiscover { command_id, .. }
                                        | ActionEnvelope::PersistedSessionPromptStatus { command_id, .. }
                                        | ActionEnvelope::Create { command_id, .. }
                                        | ActionEnvelope::Load { command_id, .. }
                                        | ActionEnvelope::Close { command_id, .. }
                                        | ActionEnvelope::Prompt { command_id, .. }
                                        | ActionEnvelope::SessionNew { command_id, .. }
                                        | ActionEnvelope::Cancel { command_id, .. }
                                        | ActionEnvelope::ResolvePermission { command_id, .. }
                                        | ActionEnvelope::SubscribeEvents { command_id, .. }
                                        | ActionEnvelope::UnsubscribeEvents { command_id, .. }
                                        | ActionEnvelope::WorkspaceCreate { command_id, .. }
                                        | ActionEnvelope::WorkspaceRemove { command_id, .. }
                                        | ActionEnvelope::SessionList { command_id, .. } => {
                                            command_id.clone()
                                        }
                                    };
                                    let _ = out_tx
                                        .send(OutboundMsg::Frame(Frame::ActionError(
                                            ActionError {
                                                command_id,
                                                code: ErrorCode::UnsupportedFrame,
                                                message: format!(
                                                    "unsupported action type: {}",
                                                    action.type_str()
                                                ),
                                                retryable: false,
                                                retry_after_ms: None,
                                            },
                                        )))
                                        .await;
                                    continue;
                                }
                            }
                            // §17.2/§8.4：Degraded/Restarting 拒绝新 committed
                            // 承诺（与落盘失败语义同源：retryable，客户端退避
                            // 重试；Restarting 期间机器未对账，禁止控制操作，
                            // §8.4.1 不变量 4）。
                            if let Frame::Action(action) = &frame {
                                let read_only_query = matches!(
                                    action,
                                    ActionEnvelope::PersistedSessionPromptStatus { .. }
                                );
                                if !read_only_query && !self.can_accept_committed() {
                                    let _ = out_tx
                                        .send(OutboundMsg::Frame(Frame::ActionError(
                                            action_error_committed_rejected(action),
                                        )))
                                        .await;
                                    continue;
                                }
                            }
                            let outcome = channel.dispatch(frame, &deps, out_tx.clone()).await;
                            if let Some(code) = self.apply_outcome(&mut channel, &out_tx, conn_id, outcome).await {
                                break code;
                            }
                        }
                        Ok(Message::Pong(_)) => heartbeat.on_pong(),
                        Ok(Message::Close(_)) => break 1000,
                        Ok(Message::Binary(_)) => {
                            let _ = out_tx.send(OutboundMsg::Frame(
                                unsupported_error(&ProtoError::DirectionRejected(
                                    "binary not supported".into())))).await;
                        }
                        Ok(_) => {}
                        Err(e) => {
                            debug!(conn_id, error = ?e, "ws read error");
                            break 1011;
                        }
                    }
                }
                _ = ticker.tick() => {
                    let valid = if let Some(sid) = browser_session.as_deref() {
                        self.auth.lock().await.validate_browser_session(sid, peer).map(|_| ())
                    } else {
                        self.auth.lock().await.revalidate_client_identity(&ctx.token_id, ctx.role)
                    };
                    if valid.is_err() { break 4502; }
                    // keep_alive（§4.7：每 interval 下发；pong 超时 → 4501）。
                    let now = std::time::Instant::now();
                    if heartbeat.should_send_keepalive(now) {
                        heartbeat.note_sent();
                        let _ = out_tx.send(OutboundMsg::Frame(Frame::KeepAlive(
                            acp_hub_proto::conn::KeepAlive {}))).await;
                    }
                    if heartbeat.check_timeout(now) {
                        warn!(conn_id, "keep_alive pong timeout");
                        break 4501;
                    }
                }
                out = out_rx.recv() => {
                    match out {
                        Some(OutboundMsg::Frame(f)) => {
                            if send_frame(&mut ws_sink, &f).await.is_err() {
                                break 1011;
                            }
                        }
                        Some(OutboundMsg::JsonRpc(_)) => break 1011,
                        Some(OutboundMsg::Close(code)) => break code,
                        None => break 1011,
                    }
                }
            }
        };

        self.finish_connection(
            conn_id,
            &mut ws_sink,
            close_code,
            "client connection closed",
        )
        .await;
    }

    /// 分派结果副作用执行（gateway 侧：打开 doc + 快照 + ready + flush）。
    /// 返回 Some(close_code) = 连接需关闭。
    async fn apply_outcome(
        &self,
        channel: &mut ChatChannel,
        out_tx: &mpsc::Sender<OutboundMsg>,
        conn_id: ConnId,
        outcome: DispatchOutcome,
    ) -> Option<u16> {
        match outcome {
            DispatchOutcome::Send(msgs) => {
                for m in msgs {
                    if out_tx.send(m).await.is_err() {
                        return Some(1011);
                    }
                }
                None
            }
            DispatchOutcome::Subscribe { docs, first } => {
                let mut versions = std::collections::HashMap::new();
                for doc in &docs {
                    // 打开/恢复 Doc（§4.6 步骤 2：DocManager::open_chat 幂等；
                    // instance_id/title 来自 ChatRegistry，未登记 → 空值）。
                    if let Some(cid) = doc_cid(doc) {
                        let (instance_id, title) = match self.deps.chats.entry(cid).await {
                            Some(e) => (e.instance_id.clone(), e.title.clone()),
                            None => (String::new(), String::new()),
                        };
                        if let Err(e) = self
                            .doc
                            .open_chat(cid, &instance_id, Some(&title), None, None)
                            .await
                        {
                            warn!(conn_id, chat_id = cid, error = ?e, "open chat failed");
                        }
                    }
                    // 全量快照（§4.6 步骤 3：StoreSink 镜像 + projection_version）。
                    if let Some((state, version)) = self.sink.snapshot(doc).await {
                        let frame = Frame::YsyncUpdate(acp_hub_proto::ysync::YsyncUpdate {
                            doc: doc.clone(),
                            update: base64::engine::general_purpose::STANDARD.encode(&state),
                            projection_version: Some(version),
                        });
                        if out_tx.send(OutboundMsg::Frame(frame)).await.is_err() {
                            return Some(1011);
                        }
                        versions.insert(doc.clone(), version);
                    }
                    // broadcaster 接线（§4.2 订阅）。
                    if let Err(e) = self
                        .deps
                        .broadcast
                        .subscribe(conn_id, vec![doc.clone()], out_tx.clone())
                        .await
                    {
                        warn!(conn_id, error = ?e, "broadcast subscribe failed");
                    }
                }
                if first {
                    // ready 握手（§4.6 步骤 4）→ mark_ready → flush 缓冲。
                    let ready = Frame::Ready(acp_hub_proto::conn::Ready {
                        projection_versions: versions,
                        negotiated_capabilities: channel.negotiated_capabilities(),
                    });
                    if out_tx.send(OutboundMsg::Frame(ready)).await.is_err() {
                        return Some(1011);
                    }
                    let flushed = channel.mark_ready();
                    // flush 缓冲 Action（§4.6 步骤 4；ready 后正常 submit 路径）。
                    for action in flushed {
                        match channel
                            .dispatch(Frame::Action(action), &self.deps, out_tx.clone())
                            .await
                        {
                            DispatchOutcome::Send(msgs) => {
                                for m in msgs {
                                    if out_tx.send(m).await.is_err() {
                                        return Some(1011);
                                    }
                                }
                            }
                            DispatchOutcome::Disconnect(code) => return Some(code),
                            _ => {}
                        }
                    }
                }
                None
            }
            DispatchOutcome::Unsubscribe { docs } => {
                self.deps.broadcast.unsubscribe(conn_id, docs).await;
                None
            }
            DispatchOutcome::Disconnect(code) => Some(code),
            DispatchOutcome::None => None,
        }
    }

    /// instance 连接：双向认证 → hello 注册 → 帧循环。
    async fn handle_instance_connection(
        &self,
        conn_id: ConnId,
        mut ws_sink: futures::stream::SplitSink<WebSocketStream<TcpStream>, Message>,
        mut ws_stream: futures::stream::SplitStream<WebSocketStream<TcpStream>>,
        peer: SocketAddr,
        hello: InstanceHello,
    ) {
        let (out_tx, mut out_rx) = mpsc::channel::<OutboundMsg>(256);
        // 双向认证（§9.2 步骤 1–2）。
        let (ctx, auth_response) = {
            let mut auth_service = self.auth.lock().await;
            match auth_service.authenticate_instance(&hello, peer).await {
                Ok(ok) => (ok.ctx, ok.response),
                Err(e) => {
                    warn!(peer = %peer, error = ?e, "instance auth failed");
                    let _ = ws_sink
                        .send(Message::Close(Some(CloseFrame {
                            code: close_code(4502),
                            reason: "instance authentication failed".into(),
                        })))
                        .await;
                    self.conns.unregister(conn_id);
                    return;
                }
            }
        };
        self.conns.upgrade(conn_id, ctx.clone());
        // 下发 auth_response（§9.2 步骤 2：server 身份证明；instance 校验通过
        // 前不执行任何 spawn/kill）。
        if send_frame(&mut ws_sink, &Frame::AuthResponse(auth_response))
            .await
            .is_err()
        {
            debug!(conn_id, "auth_response send failed");
            self.conns.unregister(conn_id);
            return;
        }
        let instance_id = ctx.name.clone();
        // hello 注册（§4.5 幂等替换：fencing 旧连接）。
        let outcome = self
            .deps
            .instance
            .on_hello(
                &instance_id,
                &ctx.token_id,
                InstanceConn { tx: out_tx.clone() },
                &hello,
            )
            .await;
        audit(
            "instance.hello",
            None,
            Some(&ctx.token_id),
            "ok",
            Duration::ZERO,
            None,
        );
        info!(
            conn_id, instance_id = %instance_id, hostname = %hello.hostname,
            fenced = outcome.fenced_previous,
            "instance connected"
        );
        // hello 注册成功 → Registry instances 视图 upsert（§7.1/§12.4：机器
        // 列表唯一权威源；registered_at 以本次 hello 时刻为准，后续心跳
        // 复用首值）。
        let registered_at = chrono::Utc::now().to_rfc3339();
        let view = InstanceView {
            id: instance_id.clone(),
            hostname: hello.hostname.clone(),
            status: InstanceStatus::Online,
            token_id: ctx.token_id.clone(),
            registered_at: registered_at.clone(),
            last_heartbeat: registered_at.clone(),
            chat_count: 0,
        };
        if let Err(e) = self.registry.upsert_instance(view).await {
            warn!(instance_id = %instance_id, error = ?e, "registry instance upsert failed (hello)");
        }
        // 孤儿清理钩子（§7.5：已中断/终态但 instance 声称存活 → 补发 kill）。
        self.deps
            .instance
            .cleanup_orphans(&instance_id, &outcome)
            .await;
        // §8.4.1 不变量 4：instance 重连（hello）对账后开门——Restarting →
        // Healthy（或 Degraded，若其他条件仍活跃；幂等）。
        if let Err(e) = self.registry.clear_restarting().await {
            warn!(instance_id = %instance_id, error = ?e, "clear_restarting failed (registry write)");
        }

        let mut auth_ticker = tokio::time::interval(self.heartbeat_interval);
        auth_ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        auth_ticker.tick().await;

        let close_code = loop {
            tokio::select! {
                _ = auth_ticker.tick() => {
                    if self.auth.lock().await.revalidate_instance_identity(&ctx.token_id).is_err() { break 4502; }
                }
                msg = ws_stream.next() => {
                    let Some(msg) = msg else { break 1011 };
                    match msg {
                        Ok(Message::Text(text)) => {
                            let frame = match Frame::parse(&text) {
                                Ok(f) => f,
                                Err(e) => {
                                    warn!(instance_id = %instance_id, error = ?e, "malformed instance frame");
                                    continue;
                                }
                            };
                            if !matches!(m1_check(frame.tag(), Role::Instance, Direction::Inbound),
                                acp_hub_proto::whitelist::M1Check::Allowed)
                            {
                                warn!(instance_id = %instance_id, tag = %frame.tag(),
                                    "instance frame rejected (whitelist)");
                                continue;
                            }
                            match frame {
                                Frame::InstanceEvent(ev) => {
                                    let r = self.relay.on_instance_event(&instance_id, &ev).await;
                                    trace_consume(&r);
                                }
                                Frame::InstanceBufferSync(sync) => {
                                    let r = self.relay.on_buffer_sync(&instance_id, &sync).await;
                                    trace_consume(&r);
                                }
                                Frame::InstanceHeartbeat(hb) => {
                                    if let Err(e) = self.deps.instance.on_heartbeat(&instance_id, &hb).await {
                                        debug!(instance_id = %instance_id, error = ?e, "heartbeat rejected");
                                    }
                                    // 心跳 → Registry instances 视图更新（§4.5：
                                    // last_heartbeat 刷新 + 存活会话计数；失败仅降级
                                    // 记录，不打断帧循环）。
                                    let now = chrono::Utc::now().to_rfc3339();
                                    let view = InstanceView {
                                        id: instance_id.clone(),
                                        hostname: hello.hostname.clone(),
                                        status: InstanceStatus::Online,
                                        token_id: ctx.token_id.clone(),
                                        registered_at: registered_at.clone(),
                                        last_heartbeat: now,
                                        chat_count: hb.alive_sessions.len() as u32,
                                    };
                                    if let Err(e) = self.registry.upsert_instance(view).await {
                                        debug!(instance_id = %instance_id, error = ?e, "registry heartbeat upsert failed");
                                    }
                                }
                                Frame::InstanceSpawnAck(ack) => {
                                    let cid = ack.command_id.clone();
                                    self.deps.instance.on_ack(&instance_id, &cid,
                                        InstanceAck::Spawn(ack)).await;
                                }
                                Frame::InstanceKillAck(ack) => {
                                    let cid = ack.command_id.clone();
                                    self.deps.instance.on_ack(&instance_id, &cid,
                                        InstanceAck::Kill(ack)).await;
                                }
                                Frame::InstanceForwardAck(ack) => {
                                    let cid = ack.command_id.clone();
                                    self.deps.instance.on_ack(&instance_id, &cid,
                                        InstanceAck::Forward(ack)).await;
                                }
                                Frame::InstanceProcessExit(exit) => {
                                    let r = self.relay.on_process_exit(&instance_id, &exit).await;
                                    trace_consume(&r);
                                }
                                _ => {
                                    warn!(instance_id = %instance_id, tag = %frame.tag(),
                                        "unexpected instance inbound frame");
                                }
                            }
                        }
                        Ok(Message::Close(_)) => break 1000,
                        Ok(_) => {}
                        Err(e) => {
                            debug!(conn_id, error = ?e, "instance ws read error");
                            break 1011;
                        }
                    }
                }
                out = out_rx.recv() => {
                    match out {
                        Some(OutboundMsg::Frame(f)) => {
                            if send_frame(&mut ws_sink, &f).await.is_err() {
                                break 1011;
                            }
                        }
                        Some(OutboundMsg::JsonRpc(v)) => {
                            // 透传 JSON-RPC（prompt/cancel/resolve/initialize/
                            // session/new；instance 保持 dumb，§4.5）。
                            let bytes = v.to_string().into();
                            if ws_sink.send(Message::Text(bytes)).await.is_err() {
                                break 1011;
                            }
                        }
                        Some(OutboundMsg::Close(code)) => break code,
                        None => break 1011,
                    }
                }
            }
        };

        // 断链语义（§8.2 matrix instance 行）：立即 OFFLINE + 断链清理。
        // conn 句柄比对：hello fencing 后旧连接滞后断开不触碰新连接状态
        // （§4.5 幂等替换）。
        let was_online = self
            .deps
            .instance
            .on_disconnect(&instance_id, &InstanceConn { tx: out_tx.clone() })
            .await;
        if was_online {
            if let Err(e) = self.relay.on_instance_disconnect(&instance_id).await {
                warn!(instance_id = %instance_id, error = ?e, "instance disconnect cleanup failed");
            }
        }
        audit(
            "conn.close",
            None,
            Some(&ctx.token_id),
            "ok",
            Duration::ZERO,
            None,
        );
        self.finish_connection(
            conn_id,
            &mut ws_sink,
            close_code,
            "instance connection closed",
        )
        .await;
    }

    /// 连接收尾：关闭 ws + 释放配额 + 广播订阅清理。
    async fn finish_connection(
        &self,
        conn_id: ConnId,
        ws_sink: &mut futures::stream::SplitSink<WebSocketStream<TcpStream>, Message>,
        code: u16,
        reason: &str,
    ) {
        if let Some(ctx) = self.conns.ctx(conn_id) {
            audit(
                "conn.close",
                None,
                Some(&ctx.token_id),
                "ok",
                Duration::ZERO,
                None,
            );
        }
        let _ = ws_sink
            .send(Message::Close(Some(CloseFrame {
                code: close_code(code),
                reason: reason.to_string().into(),
            })))
            .await;
        self.deps.broadcast.unsubscribe_all(conn_id).await;
        self.conns.unregister(conn_id);
        info!(conn_id, code, reason, "connection closed");
    }
}

/// 发送业务帧（serde JSON → 文本消息）。
async fn send_frame(
    ws_sink: &mut futures::stream::SplitSink<WebSocketStream<TcpStream>, Message>,
    frame: &Frame,
) -> Result<(), ()> {
    let text = serde_json::to_string(frame).map_err(|_| ())?;
    ws_sink
        .send(Message::Text(text.into()))
        .await
        .map_err(|_| ())
}

/// u16 关闭码 → tungstenite CloseCode（§4.7 应用码 4500–4502 属保留区）。
fn close_code(code: u16) -> tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode {
    use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode as C;
    match code {
        1000 => C::Normal,
        1011 => C::Error,
        1013 => C::Again,
        n => C::Reserved(n),
    }
}

/// DocId → cid 提取（`chat:{cid}` / `session:{cid}`）。
///
/// `hub:registry` 不是 chat doc，不得提取 cid（否则订阅 registry 会误开
/// 一个名为 "registry" 的假 chat 并污染 Registry Doc）。`control:` 为死前缀
/// （代码实际无 `DocId::control` 构造，#4 前缀面统一为 session:）。
fn doc_cid(doc: &acp_hub_proto::conn::DocId) -> Option<&str> {
    let s = doc.as_str();
    if !(s.starts_with("chat:") || s.starts_with("session:")) {
        return None;
    }
    s.split_once(':').map(|(_, cid)| cid)
}

/// Degraded/Restarting 期间拒绝新 committed 承诺（§17.2/§8.4：与落盘失败
/// 语义同源，retryable；§9.3 脱敏——不回显 payload）。
fn action_error_committed_rejected(action: &ActionEnvelope) -> ActionError {
    let command_id = match action {
        ActionEnvelope::ProjectCreate { command_id, .. }
        | ActionEnvelope::ProjectArchive { command_id, .. }
        | ActionEnvelope::ProjectRestore { command_id, .. }
        | ActionEnvelope::ProjectRename { command_id, .. }
        | ActionEnvelope::PersistedSessionCreate { command_id, .. }
        | ActionEnvelope::PersistedSessionOpen { command_id, .. }
        | ActionEnvelope::PersistedSessionRename { command_id, .. }
        | ActionEnvelope::PersistedSessionArchive { command_id, .. }
        | ActionEnvelope::PersistedSessionRestore { command_id, .. }
        | ActionEnvelope::PersistedSessionImport { command_id, .. }
        | ActionEnvelope::PersistedSessionDiscover { command_id, .. }
        | ActionEnvelope::PersistedSessionPromptStatus { command_id, .. }
        | ActionEnvelope::Create { command_id, .. }
        | ActionEnvelope::Load { command_id, .. }
        | ActionEnvelope::Close { command_id, .. }
        | ActionEnvelope::Prompt { command_id, .. }
        | ActionEnvelope::SessionNew { command_id, .. }
        | ActionEnvelope::Cancel { command_id, .. }
        | ActionEnvelope::ResolvePermission { command_id, .. }
        | ActionEnvelope::SubscribeEvents { command_id, .. }
        | ActionEnvelope::UnsubscribeEvents { command_id, .. }
        | ActionEnvelope::WorkspaceCreate { command_id, .. }
        | ActionEnvelope::WorkspaceRemove { command_id, .. }
        | ActionEnvelope::SessionList { command_id, .. } => command_id.clone(),
    };
    ActionError {
        command_id,
        code: ErrorCode::AgentUnavailable,
        message: "server degraded/restarting; retry later".to_string(),
        retryable: true,
        retry_after_ms: None,
    }
}

/// 未知/畸形帧 → UNSUPPORTED_FRAME error（§4.8 不静默；脱敏：不回显正文）。
fn unsupported_error(e: &ProtoError) -> Frame {
    Frame::ActionError(acp_hub_proto::ack::ActionError {
        command_id: String::new(),
        code: ErrorCode::UnsupportedFrame,
        message: match e {
            ProtoError::Unsupported(t) => format!("unsupported frame type: {t}"),
            ProtoError::DirectionRejected(t) => format!("frame rejected: {t}"),
            ProtoError::Malformed(_) => "malformed frame".to_string(),
        },
        retryable: false,
        retry_after_ms: None,
    })
}

/// 消费结果脱敏日志（§9.3：只记 kind/seq/reason）。
fn trace_consume(r: &crate::channel::relay_event_handler::ConsumeResult) {
    use crate::channel::relay_event_handler::ConsumeResult as C;
    match r {
        C::Delivered {
            chat_id,
            kind,
            seq,
            applied,
        } => debug!(chat_id, kind, seq, applied, "instance event consumed"),
        C::RpcConfirmed { command_id, .. } => debug!(command_id, "rpc confirmed (L3)"),
        C::Dropped { reason } | C::BatchRejected { reason } => {
            debug!(reason, "instance frame dropped")
        }
        C::PersistFailed { chat_id } => warn!(chat_id, "event persist failed (degraded)"),
    }
}

#[cfg(test)]
#[path = "gateway_test.rs"]
mod gateway_test;
