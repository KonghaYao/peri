//! machine daemon 主循环（F6，§4.2）：MachineConfig + Sessions 会话表 +
//! 帧转发调度 + 补推协调 + 心跳。
//!
//! 取代旧 stdio 主循环（IDE stdin 读/写、child_msg 转发 stdout 已删除）：收
//! spawn/kill（**认证通过前不执行**，§9.2 步骤 3）、上报 event/process_exit/
//! heartbeat、断线缓冲 + 重连补推（§4.5/§4.5.1/§7.1/§8.5）。
//!
//! 职责要点：
//! - **seq/epoch 分配**（§4.5.1）：session 新开 epoch=1、首帧 seq=1（§5 依据：
//!   f3-persist §6 无日志时 `(0,0)`，`from_seq = last_seq+1 = 1`）；进程重建 /
//!   daemon 重启后 `epoch = 水位 + 1`、seq 重置 1；
//! - **spawn/kill 幂等**（§4.5/§7）：同 session_id 二次 spawn 不二次起进程；
//!   kill 目标不存在/已退出视为已达成；
//! - **转发调度**：在线（认证通过且未缓冲）→ `machine/event`（`send_acked`
//!   写成功后推进 `last_sent_seq` 并写环形滑窗）；断线 → `buffer::push`；
//!   单帧超限跳过 + gap（§8.5，seq 消耗以保持流完整）；
//! - **补推协调**：Authenticated 后启动补推任务——对每个 `buffered` session
//!   分批 `machine/buffer_sync`（256 帧 / 512KB【决策】），pending 清空才转
//!   实时（§8.5 补推纪律）；发送中断 → rollback（from_seq 不变，重连重发）；
//! - **心跳**：每 `heartbeat_interval` 发 `machine/heartbeat { load,
//!   alive_sessions }`（load = min(100, alive×20)【决策】，§17.1 无精确语义）；
//! - **孤儿清理三层**（§8）：kill_on_drop（Drop）→ 进程组 kill（kill 指令）→
//!   启动时水位 pgid SIGKILL + buffer/ 目录删除（崩溃路径）。

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use acp_hub_proto::machine::{
    MachineBufferSync, MachineEvent, MachineHeartbeat, MachineHello, MachineKill, MachineKillAck,
    MachineProcessExit, MachineSpawn, MachineSpawnAck,
};
use acp_hub_proto::protocol::Defaults;
use acp_hub_proto::Frame;
use futures::future::join_all;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::auth::{AuthClient, AuthSession, HelloCtx};
use crate::buffer::{Buffer, RingBuffer, Watermark};
use crate::child::{self, AcpProcess, ChildOutput};
use crate::transport::{
    self, SendError, StoppedReason, TransportConfig, TransportEvent, TransportHandle,
};

// ---------------------------------------------------------------------------
// 配置
// ---------------------------------------------------------------------------

/// machine daemon 配置（§4.2；默认值对齐 §10/proto::Defaults）。
#[derive(Debug, Clone)]
pub struct MachineConfig {
    /// server ws 地址（`ws://host:port/machine`）。
    pub server_url: String,
    /// machine token（从 token 文件读入，不落日志）。
    pub token: String,
    /// 数据目录（`~/.local/share/acp-hub/machine/`，0600）。
    pub data_dir: PathBuf,
    /// 心跳间隔（proto::Defaults::HEARTBEAT_INTERVAL，5s）。
    pub heartbeat_interval: Duration,
    /// 重连退避起点（§7.1，1s）。
    pub reconnect_base: Duration,
    /// 重连退避上限（§7.1，60s）。
    pub reconnect_max: Duration,
    /// 握手超时（10s【决策】）。
    pub auth_timeout: Duration,
    /// 缓冲合计上限字节（proto::Defaults::BUFFER_LIMIT_BYTES，10MB）。
    pub buffer_limit_bytes: usize,
    /// 缓冲合计上限条数（proto::Defaults::BUFFER_LIMIT_FRAMES，万条）。
    pub buffer_limit_frames: usize,
    /// 内存段字节预算（5MB【决策】= 合计口径半区，§8.5）。
    pub mem_buffer_bytes: usize,
    /// 单帧上限（proto::Defaults::MAX_FRAME_BYTES，1MB；超限跳过 + gap，§8.5）。
    pub max_frame_bytes: usize,
    /// 环形滑窗容量（proto::Defaults::RING_BUFFER_CAPACITY，500）。
    pub ring_capacity: usize,
    /// kill 宽限（3s【决策】；server 可经 `machine/kill.grace` 覆盖）。
    pub kill_grace: Duration,
}

impl MachineConfig {
    /// 以必需项构建，其余取协议默认值（§10）。
    pub fn new(server_url: String, token: String, data_dir: PathBuf) -> Self {
        MachineConfig {
            server_url,
            token,
            data_dir,
            heartbeat_interval: Defaults::HEARTBEAT_INTERVAL,
            reconnect_base: Duration::from_secs(1),
            reconnect_max: Duration::from_secs(60),
            auth_timeout: Duration::from_secs(10),
            buffer_limit_bytes: Defaults::BUFFER_LIMIT_BYTES,
            buffer_limit_frames: Defaults::BUFFER_LIMIT_FRAMES,
            mem_buffer_bytes: Defaults::BUFFER_LIMIT_BYTES / 2,
            max_frame_bytes: Defaults::MAX_FRAME_BYTES,
            ring_capacity: Defaults::RING_BUFFER_CAPACITY,
            kill_grace: Duration::from_secs(3),
        }
    }
}

// ---------------------------------------------------------------------------
// 状态
// ---------------------------------------------------------------------------

/// 会话条目（§4.2）。
struct SessionEntry {
    /// None = 进程已退出但会话状态保留（供重建 epoch+1）。
    acp: Option<Arc<AcpProcess>>,
    /// 流纪元（§4.5.1）。
    epoch: u64,
    /// 下一帧 seq（首帧 1）。
    next_seq: u64,
    /// 已确认送达的最大 seq（在线 = 写成功；补推 from_seq = last_sent_seq+1）。
    last_sent_seq: u64,
    /// 有待补推（断线缓冲或补推进行中）。
    buffered: bool,
}

/// daemon 共享状态（std Mutex 保护——临界区均为同步短操作，不跨 await）。
struct HubState {
    sessions: StdMutex<HashMap<String, SessionEntry>>,
    buffer: StdMutex<Buffer>,
    rings: StdMutex<HashMap<String, RingBuffer>>,
    watermark: StdMutex<Watermark>,
    /// 启动清理是否发生缓冲丢失（重启后 true，§7.5）。
    buffer_lost: bool,
    /// 机器 hostname（hello 字段）。
    hostname: String,
    /// 子进程事件汇聚通道（各 session forward 任务的终点）。
    child_tx: mpsc::UnboundedSender<ChildOutput>,
    /// 无法提取 sessionId 的帧计数（§3.3 本地缺口）。
    dropped_no_sid: AtomicU64,
    /// 单帧超限跳过计数（§8.5 gap）。
    oversize_gaps: AtomicU64,
    /// 认证通过前收到 spawn/kill 的丢弃计数（§9.2 步骤 3）。
    pre_auth_dropped: AtomicU64,
    /// env 白名单追加项（`ACP_HUB_ENV_ALLOWLIST`，§9.6 双端校验）。
    env_allowlist: Vec<String>,
}

// ---------------------------------------------------------------------------
// 启动清理（§8 第三层：崩溃路径）
// ---------------------------------------------------------------------------

/// 启动清理：残留 pgid SIGKILL（ESRCH 忽略）+ buffer/ 目录删除（§3.3 缓冲不
/// 跨重启）。返回 (水位, buffer_lost)——`buffer_lost` = 发现上代运行残留
/// （水位非空或 buffer 目录存在），首次运行 false。
pub fn startup_cleanup(config: &MachineConfig) -> anyhow::Result<(Watermark, bool)> {
    fs::create_dir_all(&config.data_dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&config.data_dir, fs::Permissions::from_mode(0o700));
    }
    let watermark = Watermark::load(&config.data_dir)?;
    let buffer_dir = config.data_dir.join("buffer");
    let buffer_lost = !watermark.pgids().is_empty() || buffer_dir.exists();

    for pgid in watermark.pgids() {
        // §8 步骤 2：对水位记录的上代残留进程组 SIGKILL（pid 重用风险 M1 接受，
        // 权威对账在 server 侧【决策】）。
        child::sys::kill_group(pgid, child::sys::SIGKILL);
        tracing::info!(target: "acp_hub::machine", pgid, "启动清理：残留进程组 SIGKILL");
    }
    if buffer_dir.exists() {
        fs::remove_dir_all(&buffer_dir)?;
        tracing::info!(target: "acp_hub::machine", "启动清理：删除残留缓冲目录");
    }
    Ok((watermark, buffer_lost))
}

// ---------------------------------------------------------------------------
// env 白名单（§9.6 双端校验的 machine 侧）
// ---------------------------------------------------------------------------

/// 基集（§9.6：默认空 = 仅继承白名单基集）。
const ENV_BASE_ALLOWLIST: [&str; 4] = ["PATH", "HOME", "LANG", "SHELL"];

/// env 值长度上限（【决策】4096）。
const ENV_VALUE_MAX_LEN: usize = 4096;

/// `ACP_HUB_ENV_ALLOWLIST`（逗号分隔键名追加，§9.6【决策】）。
fn env_allowlist_extra() -> Vec<String> {
    std::env::var("ACP_HUB_ENV_ALLOWLIST")
        .map(|v| {
            v.split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// 键名白名单 + 值校验（UTF-8 天然满足；长度 ≤ 4096【决策】）。
fn validate_env(env: &HashMap<String, String>, extra: &[String]) -> Result<(), &'static str> {
    for (k, v) in env {
        if !ENV_BASE_ALLOWLIST.contains(&k.as_str()) && !extra.contains(k) {
            return Err("env_rejected");
        }
        if v.len() > ENV_VALUE_MAX_LEN {
            return Err("env_rejected");
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// 主循环
// ---------------------------------------------------------------------------

/// 启动 machine daemon 主循环（阻塞直到 transport 停止 / ctrl_c / 错误）。
pub async fn run(config: MachineConfig) -> anyhow::Result<()> {
    // 0. 启动清理（§8 第三层）→ 水位 + buffer_lost。
    let (watermark, buffer_lost) = startup_cleanup(&config)?;

    // 1. 认证客户端（token fail-fast）。
    let auth_client = AuthClient::new(config.token.clone())?;

    // 2. transport。
    let (events_tx, mut events_rx) = mpsc::channel::<TransportEvent>(256);
    let (handle, cancel_rx) = TransportHandle::new(1024);
    let t_config = TransportConfig {
        url: config.server_url.clone(),
        auth_timeout: config.auth_timeout,
        reconnect_base: config.reconnect_base,
        reconnect_max: config.reconnect_max,
    };

    // 3. 共享状态 + 子进程事件汇聚通道。
    let (child_tx, mut child_rx) = mpsc::unbounded_channel::<ChildOutput>();
    let hostname = std::env::var("HOSTNAME").unwrap_or_else(|_| "unknown".to_string());
    let state = Arc::new(HubState {
        sessions: StdMutex::new(HashMap::new()),
        buffer: StdMutex::new(Buffer::new(
            config.mem_buffer_bytes,
            config.buffer_limit_frames / 2,
            config.buffer_limit_bytes,
            config.buffer_limit_frames,
            config.max_frame_bytes,
            config.data_dir.join("buffer"),
        )),
        rings: StdMutex::new(HashMap::new()),
        watermark: StdMutex::new(watermark),
        buffer_lost,
        hostname,
        child_tx,
        dropped_no_sid: AtomicU64::new(0),
        oversize_gaps: AtomicU64::new(0),
        pre_auth_dropped: AtomicU64::new(0),
        env_allowlist: env_allowlist_extra(),
    });

    let make_hello = {
        let state = state.clone();
        let auth = auth_client.clone();
        move || build_hello(&state, &auth)
    };
    tokio::spawn(transport::run(
        t_config,
        make_hello,
        events_tx,
        handle.clone(),
        cancel_rx,
    ));

    // 4. 主事件循环（§4.2）。
    let mut heartbeat = tokio::time::interval(config.heartbeat_interval);
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut resync: Option<JoinHandle<()>> = None;
    let mut authenticated = false;

    loop {
        tokio::select! {
            evt = events_rx.recv() => {
                let Some(evt) = evt else {
                    tracing::warn!(target: "acp_hub::machine", "transport 任务退出，daemon 结束");
                    break;
                };
                match evt {
                    TransportEvent::Connected => {
                        tracing::info!(target: "acp_hub::machine", "ws 已建立，等待认证");
                    }
                    TransportEvent::Authenticated => {
                        authenticated = true;
                        tracing::info!(target: "acp_hub::machine", "认证通过，开始补推");
                        // 中止旧补推任务：断线→快速重连窗口内旧任务可能仍在
                        // 20ms 空转或持 in-flight 批次，双任务并发 drain 同一
                        // session 会错乱 from_seq 并可能丢帧（§6.1/§6.2）。
                        if let Some(h) = resync.take() {
                            h.abort();
                        }
                        state
                            .buffer
                            .lock()
                            .expect("buffer mutex poisoned")
                            .rollback_all();
                        let s = state.clone();
                        let h = handle.clone();
                        let c = config.clone();
                        resync = Some(tokio::spawn(async move {
                            resync_loop(&s, &h, &c).await;
                        }));
                    }
                    TransportEvent::Disconnected => {
                        authenticated = false;
                        mark_all_buffered(&state);
                        if let Some(h) = resync.take() {
                            h.abort();
                        }
                        // 中断的 resync 可能遗留 in-flight 批次 → 回置 pending。
                        state
                            .buffer
                            .lock()
                            .expect("buffer mutex poisoned")
                            .rollback_all();
                        tracing::info!(target: "acp_hub::machine", "断线：进入缓冲模式");
                    }
                    TransportEvent::AuthTimeout => {
                        tracing::warn!(target: "acp_hub::machine", "握手超时（随后停止重连）");
                    }
                    TransportEvent::Stopped(reason) => {
                        let reason_str = match reason {
                            StoppedReason::AuthFailed => "认证失败（审计计数）",
                            StoppedReason::ConfigFatal => "server 以 4502 关闭（配置性失败）",
                            StoppedReason::Shutdown => "优雅关闭",
                        };
                        tracing::error!(target: "acp_hub::machine", reason = reason_str,
                            "transport 停止自动重连，daemon 结束");
                        // §8 第一层：daemon 结束前进程组 kill 全部会话——仅靠
                        // kill_on_drop 只杀直接子进程，孙进程（shell/工具）会
                        // 孤儿残留到下次启动清理（Shutdown 分支已 kill，幂等）。
                        shutdown_all(&state, &config).await;
                        break;
                    }
                    TransportEvent::Frame(frame) => {
                        handle_inbound(&state, &handle, &config, *frame, authenticated).await;
                    }
                }
            }
            out = child_rx.recv() => {
                let Some(out) = out else { continue };
                forward_child_output(&state, &handle, &config, out, authenticated).await;
            }
            _ = heartbeat.tick() => {
                if handle.is_authenticated() {
                    send_heartbeat(&state, &handle).await;
                }
            }
            _ = tokio::signal::ctrl_c() => {
                tracing::info!(target: "acp_hub::machine", "收到 SIGINT，优雅退出");
                shutdown_all(&state, &config).await;
                handle.shutdown();
                break;
            }
        }
    }

    handle.shutdown();
    if let Some(h) = resync {
        h.abort();
    }
    tracing::info!(target: "acp_hub::machine", "daemon 退出");
    Ok(())
}

/// hello 构造（每次连接调用：新 nonce，§9.2；会话状态实时读取）。
fn build_hello(state: &HubState, auth: &AuthClient) -> (AuthSession, MachineHello) {
    let session = auth.begin();
    let ctx = {
        let sessions = state.sessions.lock().expect("sessions mutex poisoned");
        let buffer = state.buffer.lock().expect("buffer mutex poisoned");
        let stream_epochs = sessions
            .iter()
            .filter(|(_, e)| e.acp.is_some())
            .map(|(sid, e)| (sid.clone(), e.epoch))
            .collect();
        HelloCtx {
            hostname: state.hostname.clone(),
            buffered: buffer.has_any_pending(),
            buffer_lost: state.buffer_lost,
            stream_epochs,
        }
    };
    let hello = session.build_hello(&ctx);
    (session, hello)
}

/// 断线：所有存活 session 置缓冲模式（重连补推判定依据）。
fn mark_all_buffered(state: &HubState) {
    let mut sessions = state.sessions.lock().expect("sessions mutex poisoned");
    for entry in sessions.values_mut() {
        if entry.acp.is_some() {
            entry.buffered = true;
        }
    }
}

// ---------------------------------------------------------------------------
// 入站帧分发（§4.2 spawn/kill 幂等处理）
// ---------------------------------------------------------------------------

async fn handle_inbound(
    state: &HubState,
    handle: &TransportHandle,
    config: &MachineConfig,
    frame: Frame,
    authenticated: bool,
) {
    match frame {
        Frame::MachineSpawn(spawn) => {
            if !authenticated {
                state.pre_auth_dropped.fetch_add(1, Ordering::Relaxed);
                tracing::warn!(target: "acp_hub::machine", session_id = %spawn.session_id,
                    "认证通过前收到 machine/spawn（丢弃，不执行）");
                return;
            }
            handle_spawn(state, handle, spawn).await;
        }
        Frame::MachineKill(kill) => {
            if !authenticated {
                state.pre_auth_dropped.fetch_add(1, Ordering::Relaxed);
                tracing::warn!(target: "acp_hub::machine", session_id = %kill.session_id,
                    "认证通过前收到 machine/kill（丢弃，不执行）");
                return;
            }
            handle_kill(state, handle, config, kill).await;
        }
        // 下行 ACP JSON-RPC 透传（冲突 1 裁决后接入）：写 ACP stdin（§4.4 L2），
        // 成功/失败回 `machine/forward_ack`（L1+L2 合并确认）。
        Frame::MachineForward(fwd) => {
            if !authenticated {
                state.pre_auth_dropped.fetch_add(1, Ordering::Relaxed);
                tracing::warn!(target: "acp_hub::machine", session_id = %fwd.session_id,
                    "认证通过前收到 machine/forward（丢弃，不执行）");
                return;
            }
            let ok = handle_downlink(state, &fwd.session_id, &fwd.frame).await;
            let ack = Frame::MachineForwardAck(acp_hub_proto::machine::MachineForwardAck {
                command_id: fwd.command_id.clone(),
                session_id: fwd.session_id.clone(),
                ok,
                error: if ok { None } else { Some("stdin_write_failed".to_string()) },
            });
            if let Err(e) = handle.send(ack).await {
                tracing::warn!(target: "acp_hub::machine", session_id = %fwd.session_id,
                    error = ?e, "forward_ack 发送失败（连接可能已断）");
            }
        }
        other => {
            tracing::warn!(target: "acp_hub::machine", tag = %other.tag(),
                "入站帧不在 machine 处理面（丢弃并计数）");
        }
    }
}

/// 下行 ACP 指令接入（冲突 1 裁决后）：写 ACP stdin（§4.4 L2）。
///
/// 写失败（进程已退出/管道关闭）→ 返回 `false`，调用方回
/// `machine/forward_ack { ok: false }`（server 侧映射 retryable 失败）。
async fn handle_downlink(state: &HubState, session_id: &str, frame: &serde_json::Value) -> bool {
    let acp = {
        let sessions = state.sessions.lock().expect("sessions mutex poisoned");
        sessions.get(session_id).and_then(|e| e.acp.clone())
    };
    match acp {
        Some(acp) => acp.write_line(frame).await.is_ok(),
        None => {
            tracing::warn!(target: "acp_hub::machine", session_id,
                "下行指令写入失败：session 不存在或进程已退出");
            false
        }
    }
}

/// `machine/spawn`（§4.5/§7）：按 session_id 幂等；env 白名单 + cwd 校验；
/// 不二次起进程；epoch = 水位 + 1（新 session 为 1）。
async fn handle_spawn(state: &HubState, handle: &TransportHandle, spawn: MachineSpawn) {
    let sid = spawn.session_id.clone();
    let command_id = spawn.command_id.clone();

    // 前置校验（§7：env 双端白名单、cwd 存在性【决策】；失败 → 脱敏类别 ack）。
    if let Some(env) = &spawn.env {
        let extra = state.env_allowlist.clone();
        if let Err(cat) = validate_env(env, &extra) {
            tracing::warn!(target: "acp_hub::machine", session_id = %sid, reason = cat,
                "spawn env 校验失败");
            send_spawn_ack(handle, &command_id, &sid, false, Some(cat)).await;
            return;
        }
    }
    if !Path::new(&spawn.cwd).is_dir() {
        tracing::warn!(target: "acp_hub::machine", session_id = %sid, "spawn cwd 不存在");
        send_spawn_ack(handle, &command_id, &sid, false, Some("cwd_not_found")).await;
        return;
    }

    // 幂等：会话已存在且进程存活 → 直接 ok（不二次起进程，§4.5）。
    let idempotent_hit = {
        let sessions = state.sessions.lock().expect("sessions mutex poisoned");
        sessions.get(&sid).is_some_and(|e| e.acp.is_some())
    };
    if idempotent_hit {
        tracing::info!(target: "acp_hub::machine", session_id = %sid,
            "spawn 幂等命中：session 已存在，直接 ack");
        send_spawn_ack(handle, &command_id, &sid, true, None).await;
        return;
    }

    // epoch：水位记录 + 1（新 session 无记录 → 1，§4.5.1/§5）。
    let epoch = {
        let wm = state.watermark.lock().expect("watermark mutex poisoned");
        wm.epoch_of(&sid).map_or(1, |e| e + 1)
    };

    // spawn（进程组 + kill_on_drop；stdout 事件经 forward 任务汇聚到主循环）。
    let (child_tx, mut child_rx) = mpsc::unbounded_channel::<ChildOutput>();
    let hub_tx = state.child_tx.clone();
    tokio::spawn(async move {
        while let Some(out) = child_rx.recv().await {
            if hub_tx.send(out).is_err() {
                break;
            }
        }
    });
    match child::spawn(&spawn.cmd, &spawn.cwd, spawn.env.as_ref(), &sid, child_tx).await {
        Ok(acp) => {
            {
                let mut sessions = state.sessions.lock().expect("sessions mutex poisoned");
                sessions.insert(
                    sid.clone(),
                    SessionEntry {
                        acp: Some(acp.clone()),
                        epoch,
                        next_seq: 1,
                        last_sent_seq: 0,
                        buffered: false,
                    },
                );
            }
            // 水位：epoch 变更写盘（§4.4.3 更新时机）。
            let pgid = acp.pgid();
            {
                let mut wm = state.watermark.lock().expect("watermark mutex poisoned");
                if let Err(e) = wm.record(&sid, epoch, 0, pgid) {
                    tracing::error!(target: "acp_hub::machine", session_id = %sid, error = %e,
                        "水位写入失败");
                }
            }
            tracing::info!(target: "acp_hub::machine", session_id = %sid, epoch, pgid,
                "ACP 进程启动");
            send_spawn_ack(handle, &command_id, &sid, true, None).await;
        }
        Err(e) => {
            // §9.3 脱敏：日志/ack 不含 cmd/cwd/env 值。
            tracing::error!(target: "acp_hub::machine", session_id = %sid, error = %e,
                "spawn 失败（进程未启动）");
            send_spawn_ack(handle, &command_id, &sid, false, Some("spawn_failed")).await;
        }
    }
}

/// `machine/kill`（§4.5/§7）：组级 kill（grace 可被 server 覆盖）；目标不存在
/// /已退出 → 视为已达成（幂等，`kill_ack{ok:true}`）。
async fn handle_kill(
    state: &HubState,
    handle: &TransportHandle,
    config: &MachineConfig,
    kill: MachineKill,
) {
    let sid = kill.session_id.clone();
    let command_id = kill.command_id.clone();
    let acp = {
        let sessions = state.sessions.lock().expect("sessions mutex poisoned");
        sessions.get(&sid).and_then(|e| e.acp.clone())
    };
    let grace = kill
        .grace
        .map(Duration::from_millis)
        .unwrap_or(config.kill_grace);
    match acp {
        Some(acp) => {
            let _ = acp.kill(grace).await;
            tracing::info!(target: "acp_hub::machine", session_id = %sid, grace_ms = grace.as_millis(),
                "kill 完成（进程组）");
        }
        None => {
            tracing::info!(target: "acp_hub::machine", session_id = %sid,
                "kill 幂等：目标不存在/已退出，视为已达成");
        }
    }
    let ack = Frame::MachineKillAck(MachineKillAck {
        command_id,
        session_id: sid,
        ok: true,
    });
    let _ = handle.send(ack).await;
}

async fn send_spawn_ack(
    handle: &TransportHandle,
    command_id: &str,
    session_id: &str,
    ok: bool,
    error: Option<&'static str>,
) {
    let ack = Frame::MachineSpawnAck(MachineSpawnAck {
        command_id: command_id.to_string(),
        session_id: session_id.to_string(),
        ok,
        error: error.map(ToOwned::to_owned),
    });
    let _ = handle.send(ack).await;
}

// ---------------------------------------------------------------------------
// 转发调度（§4.2：在线实时 / 断线缓冲）
// ---------------------------------------------------------------------------

async fn forward_child_output(
    state: &HubState,
    handle: &TransportHandle,
    config: &MachineConfig,
    out: ChildOutput,
    authenticated: bool,
) {
    match out {
        ChildOutput::Frame(evt) => {
            let sid = evt.session_id;
            // seq 分配（锁内同步段，不跨 await；超限帧同样消耗 seq 保持流完整，
            // 缺口由 server 侧 gap 呈现——「不假装完整」，§8.5）。
            let (epoch, seq, buffered) = {
                let mut sessions = state.sessions.lock().expect("sessions mutex poisoned");
                let Some(entry) = sessions.get_mut(&sid) else {
                    tracing::debug!(target: "acp_hub::machine", session_id = %sid,
                        "帧属于未知 session（丢弃）");
                    return;
                };
                if entry.acp.is_none() {
                    tracing::debug!(target: "acp_hub::machine", session_id = %sid,
                        "帧属于已退出 session（丢弃）");
                    return;
                }
                let seq = entry.next_seq;
                entry.next_seq += 1;
                (entry.epoch, seq, entry.buffered)
            };

            let online = authenticated && !buffered;
            // 单帧超限检查（在线/断线统一，§8.5：超限跳过 + gap，不做截断）。
            let payload = if online {
                serde_json::to_vec(&Frame::MachineEvent(MachineEvent {
                    session_id: sid.clone(),
                    epoch,
                    seq,
                    frame: evt.frame.clone(),
                }))
            } else {
                serde_json::to_vec(&acp_hub_proto::machine::BufferedFrame {
                    seq,
                    frame: evt.frame.clone(),
                })
            };
            let payload = payload.unwrap_or_default();
            if payload.len() > config.max_frame_bytes {
                state.oversize_gaps.fetch_add(1, Ordering::Relaxed);
                tracing::warn!(target: "acp_hub::machine", session_id = %sid, seq,
                    bytes = payload.len(), max = config.max_frame_bytes,
                    "帧超单帧上限，跳过（gap 计数）");
                return;
            }

            if online {
                let frame = Frame::MachineEvent(MachineEvent {
                    session_id: sid.clone(),
                    epoch,
                    seq,
                    frame: evt.frame.clone(),
                });
                match handle.send_acked(frame).await {
                    Ok(()) => {
                        // 写成功：推进 last_sent_seq + 写环形滑窗（§4.2）。
                        let mut sessions = state.sessions.lock().expect("sessions mutex poisoned");
                        if let Some(entry) = sessions.get_mut(&sid) {
                            entry.last_sent_seq = seq;
                        }
                        ring_push(state, &sid, seq, evt.frame, config.ring_capacity);
                    }
                    Err(SendError::Stopped) => {
                        // daemon 关闭中：丢弃。
                    }
                    Err(_) => {
                        // 断线瞬间：帧未发出 → 入缓冲（seq 保持，补推不丢）。
                        buffer_push(state, &sid, seq, evt.frame.clone());
                        ring_push(state, &sid, seq, evt.frame, config.ring_capacity);
                    }
                }
            } else {
                // 断线缓冲（§8.3）。
                buffer_push(state, &sid, seq, evt.frame.clone());
                ring_push(state, &sid, seq, evt.frame, config.ring_capacity);
            }
        }
        ChildOutput::Exit { session_id, code } => {
            let sid = session_id;
            // 水位更新（epoch 保留供重建 +1；last_seq 诊断；pgid 置 0，§4.4.3）。
            {
                let mut sessions = state.sessions.lock().expect("sessions mutex poisoned");
                if let Some(entry) = sessions.get_mut(&sid) {
                    entry.acp = None;
                    let epoch = entry.epoch;
                    let last_seq = entry.last_sent_seq;
                    let mut wm = state.watermark.lock().expect("watermark mutex poisoned");
                    if let Err(e) = wm.record(&sid, epoch, last_seq, 0) {
                        tracing::error!(target: "acp_hub::machine", session_id = %sid,
                            error = %e, "水位写入失败");
                    }
                }
            }
            // §8.5：session 结束同步删除缓冲文件与内存段；滑窗清理。
            state.buffer.lock().expect("buffer mutex poisoned").remove(&sid);
            state.rings.lock().expect("rings mutex poisoned").remove(&sid);
            tracing::info!(target: "acp_hub::machine", session_id = %sid, code,
                "ACP 进程退出（会话条目保留，供重建 epoch+1）");

            if authenticated {
                let frame = Frame::MachineProcessExit(MachineProcessExit {
                    session_id: sid.clone(),
                    code,
                });
                let _ = handle.send(frame).await;
            } else {
                // 断线期间不缓冲 process_exit【决策】：终态由重连后 hello 的
                // alive_sessions 对账呈现（§7.5 对账语义），缓冲补推只承载 ACP 帧。
                tracing::debug!(target: "acp_hub::machine", session_id = %sid, code,
                    "断线期间进程退出（重连后由对账呈现）");
            }
        }
        ChildOutput::DroppedNoSessionId => {
            let count = state.dropped_no_sid.fetch_add(1, Ordering::Relaxed) + 1;
            tracing::warn!(target: "acp_hub::machine", count, "帧无法提取 sessionId，丢弃（本地缺口计数）");
        }
    }
}

fn ring_push(state: &HubState, sid: &str, seq: u64, frame: serde_json::Value, cap: usize) {
    let mut rings = state.rings.lock().expect("rings mutex poisoned");
    let ring = rings
        .entry(sid.to_string())
        .or_insert_with(|| RingBuffer::new(cap));
    ring.push(acp_hub_proto::machine::BufferedFrame { seq, frame });
}

fn buffer_push(state: &HubState, sid: &str, seq: u64, frame: serde_json::Value) {
    let mut buffer = state.buffer.lock().expect("buffer mutex poisoned");
    match buffer.push(sid, seq, frame) {
        crate::buffer::PushOutcome::Buffered => {}
        crate::buffer::PushOutcome::Oversize => {
            state.oversize_gaps.fetch_add(1, Ordering::Relaxed);
        }
    }
    let (bytes, frames) = buffer.water_level();
    if bytes > 0 {
        tracing::debug!(target: "acp_hub::machine", session_id = sid, seq,
            buffer_bytes = bytes, buffer_frames = frames, "帧入缓冲");
    }
}

// ---------------------------------------------------------------------------
// 补推协调（§6.1/§6.2：先排空 buffer_sync 再恢复实时转发）
// ---------------------------------------------------------------------------

/// buffer_sync 单批参数（§6.2【决策】：512KB / 256 帧，先达者）。
const SYNC_BATCH_MAX_FRAMES: usize = 256;
const SYNC_BATCH_MAX_BYTES: usize = 512 * 1024;

/// 补推循环：对每个 `buffered` session 从 pending 首帧起分批发
/// `machine/buffer_sync`（`from_seq = last_sent_seq + 1`）；发送成功 → commit
/// 并推进 `last_sent_seq`；发送中断 → rollback（重连后 from_seq 不变重发）。
/// 全部 pending 清空（或发送失败）→ 退出。
async fn resync_loop(state: &HubState, handle: &TransportHandle, _config: &MachineConfig) {
    loop {
        // 取一批（锁内同步段：清空标志 / 选 session / drain）。
        let job = {
            let mut sessions = state.sessions.lock().expect("sessions mutex poisoned");
            let mut buffer = state.buffer.lock().expect("buffer mutex poisoned");
            let mut job = None;
            for (sid, entry) in sessions.iter_mut() {
                if !entry.buffered {
                    continue;
                }
                if !buffer.has_pending(sid) {
                    entry.buffered = false; // 补推完成 → 该 session 转实时
                    continue;
                }
                if let Some((from_seq, frames)) =
                    buffer.drain_batch(sid, SYNC_BATCH_MAX_FRAMES, SYNC_BATCH_MAX_BYTES)
                {
                    job = Some((sid.clone(), from_seq, frames, entry.epoch));
                    break;
                }
            }
            job
        };

        let Some((sid, from_seq, frames, epoch)) = job else {
            // 无更多可推帧：若全部 session 已清 buffered → 退出（不持锁跨 await）。
            let all_clear = {
                let sessions = state.sessions.lock().expect("sessions mutex poisoned");
                sessions.values().all(|e| !e.buffered)
            };
            if all_clear {
                tracing::info!(target: "acp_hub::machine", "补推完成，全部 session 转实时");
                return;
            }
            // 存在 buffered 但 pending 空（补推清空后新帧尚未到达）：短暂等待后重试。
            tokio::time::sleep(Duration::from_millis(20)).await;
            continue;
        };

        let frame = Frame::MachineBufferSync(MachineBufferSync {
            session_id: sid.clone(),
            epoch,
            from_seq,
            frames: frames.clone(),
        });
        match handle.send_acked(frame).await {
            Ok(()) => {
                state.buffer.lock().expect("buffer mutex poisoned").commit(&sid);
                let last = frames.last().map(|bf| bf.seq).unwrap_or(from_seq);
                let mut sessions = state.sessions.lock().expect("sessions mutex poisoned");
                if let Some(entry) = sessions.get_mut(&sid) {
                    entry.last_sent_seq = last;
                }
                tracing::debug!(target: "acp_hub::machine", session_id = %sid, from_seq,
                    frames = frames.len(), "buffer_sync 批次已发");
            }
            Err(_) => {
                // 断线：未确认帧回置 pending（from_seq 不变，重连重发，§6.2）。
                state.buffer.lock().expect("buffer mutex poisoned").rollback(&sid);
                tracing::warn!(target: "acp_hub::machine", session_id = %sid,
                    "buffer_sync 发送中断（断线），帧回置 pending");
                return;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 心跳 / 关闭
// ---------------------------------------------------------------------------

/// 心跳：`machine/heartbeat { load, alive_sessions }`（§4.5）。
/// load【决策】= min(100, alive×20)（§17.1 无精确语义）。
async fn send_heartbeat(state: &HubState, handle: &TransportHandle) {
    let alive_sessions = {
        let sessions = state.sessions.lock().expect("sessions mutex poisoned");
        sessions
            .iter()
            .filter(|(_, e)| e.acp.is_some())
            .map(|(sid, _)| sid.clone())
            .collect::<Vec<_>>()
    };
    let load = (alive_sessions.len() * 20).min(100) as u32;
    let frame = Frame::MachineHeartbeat(MachineHeartbeat {
        load,
        alive_sessions,
    });
    let _ = handle.send(frame).await;
}

/// 优雅关闭：组级 kill 全部存活 session（并行，§8 三层语义第一/二层）。
async fn shutdown_all(state: &HubState, config: &MachineConfig) {
    let acps: Vec<Arc<AcpProcess>> = {
        let sessions = state.sessions.lock().expect("sessions mutex poisoned");
        sessions
            .values()
            .filter_map(|e| e.acp.clone())
            .collect()
    };
    if acps.is_empty() {
        return;
    }
    let grace = config.kill_grace;
    let tasks = acps
        .into_iter()
        .map(|acp| tokio::spawn(async move { let _ = acp.kill(grace).await; }));
    join_all(tasks).await;
}

// ---------------------------------------------------------------------------
// 查询接口（备用 / 测试）
// ---------------------------------------------------------------------------

/// 环形滑窗快照（冲突 2 预留：server 发现缺口请求滑窗重发时使用）。
#[allow(dead_code)]
fn ring_snapshot(state: &HubState, session_id: &str) -> Vec<acp_hub_proto::machine::BufferedFrame> {
    state
        .rings
        .lock()
        .expect("rings mutex poisoned")
        .get(session_id)
        .map(RingBuffer::snapshot)
        .unwrap_or_default()
}

#[cfg(test)]
#[path = "hub_test.rs"]
mod hub_test;
