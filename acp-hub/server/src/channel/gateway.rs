//! Gateway：ws 生命周期（架构 §4.6/§4.7/§9.2/§9.5，设计稿
//! `f5-channel-control.md` §10）。
//!
//! ws 入口：accept → 配额/回环检查 → 认证 → 角色分派 → 快照时序（client）/
//! 机器会话（machine）→ 心跳接线 → 断链清理。
//!
//! 客户端时序（§4.6）：配额 → `auth` → 订阅（`ysync.subscribe`）→ 打开/恢复
//! Doc（`DocManager::open_session` 幂等）→ 推全量快照（StoreSink 镜像，
//! 携带 `projection_version`）→ `ready` 握手 → `mark_ready` → flush 缓冲
//! Action → 帧循环 + 心跳（`HeartbeatDriver`，pong 超时 4501）。
//!
//! machine 时序（§9.2/§4.5）：首帧 `machine/hello` → 双向认证 →
//! `auth_response` 下发 → `MachineRegistry::on_hello`（fencing + 注册）→
//! 帧循环（event/buffer_sync/heartbeat/ack/process_exit）→ 断开 →
//! `on_disconnect` + `RelayEventHandler::on_machine_disconnect`（§8.2）。

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use futures::{SinkExt as _, StreamExt as _};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, Mutex};
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;
use tracing::{debug, info, warn};

use acp_hub_proto::ack::{ActionError, ErrorCode};
use acp_hub_proto::action::ActionEnvelope;
use acp_hub_proto::conn::Auth;
use acp_hub_proto::frame::{Frame, ProtoError};
use acp_hub_proto::machine::MachineHello;
use acp_hub_proto::whitelist::{Direction, Role, m1_allows_action_type, m1_check};

use crate::auth::audit::audit;
use crate::auth::AuthService;
use crate::channel::OutboundMsg;
use crate::channel::{ConnectionRegistry, ConnId};
use crate::channel::RelayEventHandler;
use crate::channel::{ChannelDeps, DispatchOutcome, SessionChannel};
use crate::config::Config;
use crate::control::HeartbeatDriver;
use crate::control::StoreSink;
use crate::control::{MachineAck, MachineConn};

use crate::state::doc_manager::DocManager;
use crate::state::registry::RegistryState;

/// 首帧等待超时（§4.6 步骤 1 前：10s 无首帧断开）。
pub const FIRST_FRAME_TIMEOUT: Duration = Duration::from_secs(10);

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
        // 2. 配额检查（认证**前**占位，§8.6：防未认证连接占满配额）。
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

        // 3. ws 握手。
        let ws = match tokio_tungstenite::accept_async(stream).await {
            Ok(ws) => ws,
            Err(e) => {
                debug!(peer = %peer, error = ?e, "ws handshake failed");
                self.conns.unregister(conn_id);
                return;
            }
        };
        let (mut ws_sink, mut ws_stream) = ws.split();

        // 4. 首帧等待（10s 超时，§4.6）。
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

        // 5. 角色分派。
        match first {
            Frame::Auth(auth) => {
                self.handle_client_connection(conn_id, ws_sink, ws_stream, peer, auth)
                    .await;
            }
            Frame::MachineHello(hello) => {
                self.handle_machine_connection(conn_id, ws_sink, ws_stream, peer, hello)
                    .await;
            }
            _ => {
                self.finish_connection(conn_id, &mut ws_sink, 1011, "first frame must be auth or machine/hello")
                    .await;
            }
        }
    }

    /// 客户端连接：认证 → 快照时序 → 帧循环 + 心跳。
    async fn handle_client_connection(
        &self,
        conn_id: ConnId,
        mut ws_sink: futures::stream::SplitSink<WebSocketStream<TcpStream>, Message>,
        mut ws_stream: futures::stream::SplitStream<WebSocketStream<TcpStream>>,
        peer: SocketAddr,
        auth: Auth,
    ) {
        let (out_tx, mut out_rx) = mpsc::channel::<OutboundMsg>(256);
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
        self.conns.upgrade(conn_id, ctx.clone());
        audit("conn.open", None, Some(&ctx.token_id), "ok", Duration::ZERO, None);
        info!(conn_id, token_id = %ctx.token_id, peer = %peer, "client connected");

        let mut channel = SessionChannel::new(ctx);
        let mut heartbeat = HeartbeatDriver::new(self.heartbeat_interval, self.heartbeat_timeout);
        let mut ticker = tokio::time::interval(self.heartbeat_interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        ticker.tick().await;

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
                                        ActionEnvelope::Create { command_id, .. }
                                        | ActionEnvelope::Load { command_id, .. }
                                        | ActionEnvelope::Close { command_id, .. }
                                        | ActionEnvelope::Prompt { command_id, .. }
                                        | ActionEnvelope::Cancel { command_id, .. }
                                        | ActionEnvelope::ResolvePermission { command_id, .. }
                                        | ActionEnvelope::SubscribeEvents { command_id, .. }
                                        | ActionEnvelope::UnsubscribeEvents { command_id, .. } => {
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
                                if !self.can_accept_committed() {
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

        self.finish_connection(conn_id, &mut ws_sink, close_code, "client connection closed")
            .await;
    }

    /// 分派结果副作用执行（gateway 侧：打开 doc + 快照 + ready + flush）。
    /// 返回 Some(close_code) = 连接需关闭。
    async fn apply_outcome(
        &self,
        channel: &mut SessionChannel,
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
                    // 打开/恢复 Doc（§4.6 步骤 2：DocManager::open_session 幂等；
                    // machine_id/title 来自 SessionRegistry，未登记 → 空值）。
                    if let Some(sid) = doc_sid(doc) {
                        let (machine_id, title) = match self.deps.sessions.entry(sid).await {
                            Some(e) => (e.machine_id.clone(), e.title.clone()),
                            None => (String::new(), String::new()),
                        };
                        if let Err(e) = self.doc.open_session(sid, &machine_id, Some(&title)).await {
                            warn!(conn_id, session_id = sid, error = ?e, "open session failed");
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
                    });
                    if out_tx.send(OutboundMsg::Frame(ready)).await.is_err() {
                        return Some(1011);
                    }
                    let flushed = channel.mark_ready();
                    // flush 缓冲 Action（§4.6 步骤 4；ready 后正常 submit 路径）。
                    for action in flushed {
                        if let DispatchOutcome::Send(msgs) = channel
                            .dispatch(Frame::Action(action), &self.deps, out_tx.clone())
                            .await
                        {
                            for m in msgs {
                                if out_tx.send(m).await.is_err() {
                                    return Some(1011);
                                }
                            }
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

    /// machine 连接：双向认证 → hello 注册 → 帧循环。
    async fn handle_machine_connection(
        &self,
        conn_id: ConnId,
        mut ws_sink: futures::stream::SplitSink<WebSocketStream<TcpStream>, Message>,
        mut ws_stream: futures::stream::SplitStream<WebSocketStream<TcpStream>>,
        peer: SocketAddr,
        hello: MachineHello,
    ) {
        let (out_tx, mut out_rx) = mpsc::channel::<OutboundMsg>(256);
        // 双向认证（§9.2 步骤 1–2）。
        let (ctx, auth_response) = {
            let mut auth_service = self.auth.lock().await;
            match auth_service.authenticate_machine(&hello, peer).await {
                Ok(ok) => (ok.ctx, ok.response),
                Err(e) => {
                    warn!(peer = %peer, error = ?e, "machine auth failed");
                    let _ = ws_sink
                        .send(Message::Close(Some(CloseFrame {
                            code: close_code(4502),
                            reason: "machine authentication failed".into(),
                        })))
                        .await;
                    self.conns.unregister(conn_id);
                    return;
                }
            }
        };
        self.conns.upgrade(conn_id, ctx.clone());
        // 下发 auth_response（§9.2 步骤 2：server 身份证明；machine 校验通过
        // 前不执行任何 spawn/kill）。
        if send_frame(&mut ws_sink, &Frame::AuthResponse(auth_response))
            .await
            .is_err()
        {
            debug!(conn_id, "auth_response send failed");
            self.conns.unregister(conn_id);
            return;
        }
        let machine_id = ctx.name.clone();
        // hello 注册（§4.5 幂等替换：fencing 旧连接）。
        let outcome = self
            .deps
            .machine
            .on_hello(
                &machine_id,
                &ctx.token_id,
                MachineConn { tx: out_tx.clone() },
                &hello,
            )
            .await;
        audit("machine.hello", None, Some(&ctx.token_id), "ok", Duration::ZERO, None);
        info!(
            conn_id, machine_id = %machine_id, hostname = %hello.hostname,
            fenced = outcome.fenced_previous,
            "machine connected"
        );
        // 孤儿清理钩子（§7.5：已中断/终态但 machine 声称存活 → 补发 kill）。
        self.deps.machine.cleanup_orphans(&machine_id, &outcome).await;
        // §8.4.1 不变量 4：machine 重连（hello）对账后开门——Restarting →
        // Healthy（或 Degraded，若其他条件仍活跃；幂等）。
        if let Err(e) = self.registry.clear_restarting().await {
            warn!(machine_id = %machine_id, error = ?e, "clear_restarting failed (registry write)");
        }

        let close_code = loop {
            tokio::select! {
                msg = ws_stream.next() => {
                    let Some(msg) = msg else { break 1011 };
                    match msg {
                        Ok(Message::Text(text)) => {
                            let frame = match Frame::parse(&text) {
                                Ok(f) => f,
                                Err(e) => {
                                    warn!(machine_id = %machine_id, error = ?e, "malformed machine frame");
                                    continue;
                                }
                            };
                            if !matches!(m1_check(frame.tag(), Role::Machine, Direction::Inbound),
                                acp_hub_proto::whitelist::M1Check::Allowed)
                            {
                                warn!(machine_id = %machine_id, tag = %frame.tag(),
                                    "machine frame rejected (whitelist)");
                                continue;
                            }
                            match frame {
                                Frame::MachineEvent(ev) => {
                                    let r = self.relay.on_machine_event(&machine_id, &ev).await;
                                    trace_consume(&r);
                                }
                                Frame::MachineBufferSync(sync) => {
                                    let r = self.relay.on_buffer_sync(&machine_id, &sync).await;
                                    trace_consume(&r);
                                }
                                Frame::MachineHeartbeat(hb) => {
                                    if let Err(e) = self.deps.machine.on_heartbeat(&machine_id, &hb).await {
                                        debug!(machine_id = %machine_id, error = ?e, "heartbeat rejected");
                                    }
                                }
                                Frame::MachineSpawnAck(ack) => {
                                    let cid = ack.command_id.clone();
                                    self.deps.machine.on_ack(&machine_id, &cid,
                                        MachineAck::Spawn(ack)).await;
                                }
                                Frame::MachineKillAck(ack) => {
                                    let cid = ack.command_id.clone();
                                    self.deps.machine.on_ack(&machine_id, &cid,
                                        MachineAck::Kill(ack)).await;
                                }
                                Frame::MachineForwardAck(ack) => {
                                    let cid = ack.command_id.clone();
                                    self.deps.machine.on_ack(&machine_id, &cid,
                                        MachineAck::Forward(ack)).await;
                                }
                                Frame::MachineProcessExit(exit) => {
                                    let r = self.relay.on_process_exit(&machine_id, &exit).await;
                                    trace_consume(&r);
                                }
                                _ => {
                                    warn!(machine_id = %machine_id, tag = %frame.tag(),
                                        "unexpected machine inbound frame");
                                }
                            }
                        }
                        Ok(Message::Close(_)) => break 1000,
                        Ok(_) => {}
                        Err(e) => {
                            debug!(conn_id, error = ?e, "machine ws read error");
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
                            // session/new；machine 保持 dumb，§4.5）。
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

        // 断链语义（§8.2 matrix machine 行）：立即 OFFLINE + 断链清理。
        // conn 句柄比对：hello fencing 后旧连接滞后断开不触碰新连接状态
        // （§4.5 幂等替换）。
        let was_online = self
            .deps
            .machine
            .on_disconnect(&machine_id, &MachineConn { tx: out_tx.clone() })
            .await;
        if was_online {
            if let Err(e) = self.relay.on_machine_disconnect(&machine_id).await {
                warn!(machine_id = %machine_id, error = ?e, "machine disconnect cleanup failed");
            }
        }
        audit("conn.close", None, Some(&ctx.token_id), "ok", Duration::ZERO, None);
        self.finish_connection(conn_id, &mut ws_sink, close_code, "machine connection closed")
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
    ws_sink.send(Message::Text(text.into())).await.map_err(|_| ())
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

/// DocId → sid 提取（`chat:{sid}` / `session:{sid}`）。
///
/// `hub:registry` 不是 session doc，不得提取 sid（否则订阅 registry 会误开
/// 一个名为 "registry" 的假 session 并污染 Registry Doc）。
fn doc_sid(doc: &acp_hub_proto::conn::DocId) -> Option<&str> {
    let s = doc.as_str();
    if !(s.starts_with("chat:") || s.starts_with("session:")) {
        return None;
    }
    s.split_once(':').map(|(_, sid)| sid)
}

/// Degraded/Restarting 期间拒绝新 committed 承诺（§17.2/§8.4：与落盘失败
/// 语义同源，retryable；§9.3 脱敏——不回显 payload）。
fn action_error_committed_rejected(action: &ActionEnvelope) -> ActionError {
    let command_id = match action {
        ActionEnvelope::Create { command_id, .. }
        | ActionEnvelope::Load { command_id, .. }
        | ActionEnvelope::Close { command_id, .. }
        | ActionEnvelope::Prompt { command_id, .. }
        | ActionEnvelope::Cancel { command_id, .. }
        | ActionEnvelope::ResolvePermission { command_id, .. }
        | ActionEnvelope::SubscribeEvents { command_id, .. }
        | ActionEnvelope::UnsubscribeEvents { command_id, .. } => command_id.clone(),
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
            session_id,
            kind,
            seq,
            applied,
        } => debug!(session_id, kind, seq, applied, "machine event consumed"),
        C::RpcConfirmed { command_id, .. } => debug!(command_id, "rpc confirmed (L3)"),
        C::Dropped { reason } | C::BatchRejected { reason } => {
            debug!(reason, "machine frame dropped")
        }
        C::PersistFailed { session_id } => warn!(session_id, "event persist failed (degraded)"),
    }
}

#[cfg(test)]
#[path = "gateway_test.rs"]
mod gateway_test;
