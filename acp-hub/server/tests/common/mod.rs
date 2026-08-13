//! F7 集成测试基建（真实进程全链路）。
//!
//! 职责：
//! - 二进制路径解析（`CARGO_BIN_EXE_acp-hub-server` 推导同 target 目录下的
//!   `acp-instance` / `test-child`，需先 `cargo build --workspace`）；
//! - 随机端口（bind :0 后读出）、独立 temp 数据/配置目录；
//! - tokens.toml 直接构造（§9.2.1：TokenStore 文件格式，与 CLI `token
//!   generate` 等价，避免污染用户 `~/.config/acp-hub`）；
//! - server / instance / test-child 子进程 spawn 与 Drop 清理（测试自身持有的
//!   进程组句柄直接 SIGKILL；instance 崩溃残留由生产启动所有权校验清理）；
//! - ws 客户端 helper（tokio-tungstenite + 认证握手 + 读帧/发帧/自动 pong）；
//! - yjs 快照解析 helper（base64 update → yrs::Doc）。
//!
//! 场景库语义：部分 helper（yjs 断言函数、进程重启、断线注入等）为多场景
//! 文件的共享基建，当前用例未全用——统一 allow(dead_code)（后续场景启用）。

#![allow(dead_code)]

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use base64::Engine as _;
use rand::Rng as _;

/// 单个测试的总时间预算（任务要求 ≤ 60s）。
pub const TEST_BUDGET: Duration = Duration::from_secs(60);
/// 单次网络读等待（默认）。
pub const RECV_TIMEOUT: Duration = Duration::from_secs(8);

// ---------------------------------------------------------------------------
// 二进制路径
// ---------------------------------------------------------------------------

/// server 二进制（cargo 注入的编译期绝对路径）。
pub fn server_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_acp-hub-server"))
}

/// 从 server bin 所在目录（target/<profile>/）推导同 workspace 的其余 bin。
/// 要求先 `cargo build --workspace`（任务验证流程已约定）。
fn target_bin_dir() -> PathBuf {
    server_bin()
        .parent()
        .expect("server bin 必有父目录")
        .to_path_buf()
}

/// instance daemon 二进制。
pub fn instance_bin() -> PathBuf {
    let p = target_bin_dir().join("acp-instance");
    assert!(
        p.exists(),
        "acp-instance 未构建：请先 `cargo build --workspace`（期望路径 {}）",
        p.display()
    );
    p
}

/// 假 ACP 进程二进制（instance 包的 test-child bin）。
pub fn test_child_bin() -> PathBuf {
    let p = target_bin_dir().join("test-child");
    assert!(
        p.exists(),
        "test-child 未构建：请先 `cargo build --workspace`（期望路径 {}）",
        p.display()
    );
    p
}

// ---------------------------------------------------------------------------
// 随机端口 / temp 环境
// ---------------------------------------------------------------------------

/// 随机空闲端口：绑定 :0 后读出再释放（有 TOCTOU，单机测试可接受）。
pub fn pick_free_port() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind :0");
    l.local_addr().expect("local_addr").port()
}

/// 32B CSPRNG → base64（44 字符，§9.2.1 token 形态）。
pub fn fresh_token() -> String {
    let mut b = [0u8; 32];
    rand::rng().fill_bytes(&mut b);
    base64::engine::general_purpose::STANDARD.encode(b)
}

/// 测试环境：独立 temp 目录 + token 文件 + fake `peri` 可执行（server 默认
/// ACP 启动命令是 `["peri", "acp"]`——§11 常量，M1 不可配置，测试以 PATH
/// 注入 fake `peri` 指向 test-child）。
pub struct TestEnv {
    pub tmp: tempfile::TempDir,
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
    pub port: u16,
    /// instance token（name = "local"，与 server 默认 instance_id 对齐）。
    pub instance_token: String,
    /// full 角色 client token。
    pub client_token: String,
    /// fake `peri` 所在目录（PATH 注入用）。
    pub fake_bin_dir: PathBuf,
    /// instance 数据目录（水位/缓冲）。
    pub instance_data_dir: PathBuf,
}

impl TestEnv {
    /// 组装环境：目录 + token 文件 + fake peri 脚本。
    pub fn new() -> TestEnv {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let config_dir = tmp.path().join("config");
        let data_dir = tmp.path().join("data");
        let instance_data_dir = tmp.path().join("instance-data");
        fs::create_dir_all(&config_dir).unwrap();
        fs::create_dir_all(&data_dir).unwrap();
        fs::create_dir_all(&instance_data_dir).unwrap();

        let instance_token = fresh_token();
        let client_token = fresh_token();
        let read_only_token = fresh_token();
        write_tokens_file(
            &config_dir.join("tokens.toml"),
            &[
                ("local".into(), "instance", instance_token.clone()),
                ("test-client".into(), "full", client_token.clone()),
                ("ro-panel".into(), "read-only", read_only_token),
            ],
        );

        let fake_bin_dir = tmp.path().join("fake-bin");
        fs::create_dir_all(&fake_bin_dir).unwrap();
        write_fake_peri(&fake_bin_dir, &test_child_bin());

        TestEnv {
            tmp,
            config_dir,
            data_dir,
            port: pick_free_port(),
            instance_token,
            client_token,
            fake_bin_dir,
            instance_data_dir,
        }
    }

    /// 写一个 config.toml（§16 配置项子集；Duration 为可读字符串形态）。
    /// 返回 --config 路径。调用前应已生成（用于短心跳等场景）。
    pub fn write_config(&self, toml_body: &str) -> PathBuf {
        let p = self.config_dir.join("config.toml");
        fs::write(&p, toml_body).unwrap();
        p
    }
}

/// TokenStore 文件格式（server/src/auth/mod.rs `TokensFile`：version + tokens；
/// TokenRole kebab-case）。
fn write_tokens_file(path: &Path, records: &[(String, &str, String)]) {
    let mut body = String::from("version = 1\n");
    for (name, role, token) in records {
        body.push_str(&format!(
            "\n[[tokens]]\nid = \"{}\"\nrole = \"{}\"\nname = \"{}\"\ntoken = \"{}\"\ncreated_at = \"2026-08-07T00:00:00Z\"\nrevoked = false\n",
            uuid::Uuid::new_v4(),
            role,
            name,
            token
        ));
    }
    fs::write(path, body).expect("write tokens.toml");
}

/// fake `peri`：server 默认 ACP 命令 `peri acp`（M1 常量），instance 经 PATH
/// 查找；脚本 exec test-child（argv 透传，test-child 忽略未知参数）。
fn write_fake_peri(dir: &Path, test_child: &Path) {
    let script = format!("#!/bin/sh\nexec '{}' \"$@\"\n", test_child.display());
    let p = dir.join("peri");
    fs::write(&p, script).expect("write fake peri");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
    }
}

// ---------------------------------------------------------------------------
// Unix 进程组 kill（FFI 自声明 libc kill，instance/src/child.rs 同源做法）
// ---------------------------------------------------------------------------

#[cfg(unix)]
pub mod sys {
    unsafe extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }

    /// 向进程组发信号：`kill(-pgid, sig)`。ESRCH 幂等。
    pub fn kill_group(pgid: i32, sig: i32) -> bool {
        unsafe { kill(-pgid, sig) == 0 }
    }
}

// ---------------------------------------------------------------------------
// server 子进程
// ---------------------------------------------------------------------------

/// server 进程句柄（Drop = SIGKILL 进程组）。
pub struct ServerProc {
    child: Option<Child>,
    pub port: u16,
    pub stderr_log: PathBuf,
}

impl ServerProc {
    /// 启动 server：`run --listen 127.0.0.1 --port <p> --data-dir <d> --config-dir <c>`。
    /// 可选 `--config`（config.toml 覆盖默认，如短心跳）。
    pub fn start(env: &TestEnv, config_file: Option<&Path>) -> ServerProc {
        Self::start_listen(env, config_file, "127.0.0.1")
    }

    /// 同 [`Self::start`]，但可指定监听地址（§9.5 非回环用例需监听
    /// `0.0.0.0` 才能收到非回环源连接）。
    pub fn start_listen(env: &TestEnv, config_file: Option<&Path>, listen: &str) -> ServerProc {
        let stderr_log = env.tmp.path().join("server.stderr.log");
        let mut cmd = Command::new(server_bin());
        // `--config` 是 clap 顶层参数，必须位于 `run` 子命令之前
        // （放在 run 后面会被 Run 子命令解析器拒绝）。
        if let Some(cfg) = config_file {
            cmd.args(["--config"]).arg(cfg);
        }
        cmd.arg("run")
            .args([
                "--listen",
                listen,
                "--listen-port",
                &env.port.to_string(),
                "--data-dir",
            ])
            .arg(&env.data_dir)
            .args(["--config-dir"])
            .arg(&env.config_dir)
            .args(["--log-level", "debug"]);
        let child = cmd
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::from(
                fs::File::create(&stderr_log).expect("create server log"),
            ))
            .spawn()
            .expect("spawn acp-hub-server");
        ServerProc {
            child: Some(child),
            port: env.port,
            stderr_log,
        }
    }

    /// 等待 TCP 监听就绪（轮询 connect；上限 20s）。
    pub fn wait_ready(&self) -> Result<(), String> {
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        while std::time::Instant::now() < deadline {
            if std::net::TcpStream::connect(("127.0.0.1", self.port)).is_ok() {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let log = fs::read_to_string(&self.stderr_log).unwrap_or_default();
        let lines: Vec<&str> = log.lines().collect();
        let skip = lines.len().saturating_sub(15);
        let tail = lines
            .iter()
            .skip(skip)
            .map(|l| l.to_string())
            .collect::<Vec<_>>()
            .join("\n  ");
        Err(format!(
            "server 未在 20s 内监听 127.0.0.1:{}；stderr 尾部:\n  {}",
            self.port, tail
        ))
    }

    /// server stderr 日志是否包含某子串（轮询等待）。
    pub fn log_contains(&self, needle: &str, timeout: Duration) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        while std::time::Instant::now() < deadline {
            if let Ok(content) = fs::read_to_string(&self.stderr_log) {
                if content.contains(needle) {
                    return true;
                }
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        false
    }

    /// 打印 stderr 尾部（诊断用，最多 40 行）。
    pub fn dump_log(&self) {
        if let Ok(content) = fs::read_to_string(&self.stderr_log) {
            let lines: Vec<&str> = content.lines().collect();
            let tail = lines.len().saturating_sub(40);
            for l in lines.iter().skip(tail) {
                eprintln!("[server] {l}");
            }
        }
    }

    /// 手动终止（进程组 SIGKILL）。
    pub fn kill(&mut self) {
        if let Some(mut c) = self.child.take() {
            #[cfg(unix)]
            {
                use std::os::unix::process::ExitStatusExt;
                let _ = sys::kill_group(c.id() as i32, 9);
                if let Ok(Some(status)) = c.try_wait() {
                    if status.signal() == Some(9) {
                        return;
                    }
                }
            }
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

impl Drop for ServerProc {
    fn drop(&mut self) {
        self.kill();
    }
}

// ---------------------------------------------------------------------------
// instance 子进程
// ---------------------------------------------------------------------------

/// instance daemon 进程句柄（Drop = SIGKILL 进程组 + watermark 残留清理）。
pub struct InstanceProc {
    child: Option<Child>,
    pub env_path: PathBuf,
    pub stderr_log: PathBuf,
}

impl InstanceProc {
    /// 启动 instance：`--server-url ws://127.0.0.1:<port>/instance --token-file
    /// <f> --data-dir <d>`。PATH 注入 fake-bin（含 `peri`），HOSTNAME 固定。
    pub fn start(env: &TestEnv) -> InstanceProc {
        let token_file = env.tmp.path().join("instance.token");
        fs::write(&token_file, env.instance_token.clone()).unwrap();
        let stderr_log = env.tmp.path().join("instance.stderr.log");

        let path = format!(
            "{}:{}",
            env.fake_bin_dir.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let mut cmd = Command::new(instance_bin());
        cmd.args([
            "--server-url",
            &format!("ws://127.0.0.1:{}/instance", env.port),
            "--token-file",
        ])
        .arg(&token_file)
        .args(["--data-dir"])
        .arg(&env.instance_data_dir)
        .args(["--log-level", "debug"])
        .env("PATH", &path)
        .env("HOSTNAME", "it-host");
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            cmd.process_group(0);
        }
        let child = cmd
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::from(
                fs::File::create(&stderr_log).expect("create instance log"),
            ))
            .spawn()
            .expect("spawn acp-instance");
        InstanceProc {
            child: Some(child),
            env_path: env.fake_bin_dir.clone(),
            stderr_log,
        }
    }

    /// 等待认证通过（instance stderr 出现「认证通过」或 server 侧「instance
    /// connected」由调用方轮询；这里轮询本进程日志）。
    pub fn wait_authenticated(&self, timeout: Duration) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        while std::time::Instant::now() < deadline {
            if let Ok(content) = fs::read_to_string(&self.stderr_log) {
                if content.contains("认证通过") || content.contains("instance connected") {
                    return true;
                }
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        false
    }

    /// instance stderr 是否含某子串（轮询）。
    pub fn log_contains(&self, needle: &str, timeout: Duration) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        while std::time::Instant::now() < deadline {
            if let Ok(content) = fs::read_to_string(&self.stderr_log) {
                if content.contains(needle) {
                    return true;
                }
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        false
    }

    /// 打印 stderr 尾部。
    pub fn dump_log(&self) {
        if let Ok(content) = fs::read_to_string(&self.stderr_log) {
            let lines: Vec<&str> = content.lines().collect();
            let tail = lines.len().saturating_sub(40);
            for l in lines.iter().skip(tail) {
                eprintln!("[instance] {l}");
            }
        }
    }

    /// 终止：进程组 SIGKILL。
    pub fn kill(&mut self) {
        if let Some(mut c) = self.child.take() {
            #[cfg(unix)]
            {
                use std::os::unix::process::ExitStatusExt;
                let _ = sys::kill_group(c.id() as i32, 9);
                if let Ok(Some(status)) = c.try_wait() {
                    if status.signal() == Some(9) {
                        return;
                    }
                }
            }
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

impl Drop for InstanceProc {
    fn drop(&mut self) {
        self.kill();
    }
}

// ---------------------------------------------------------------------------
// ws 客户端
// ---------------------------------------------------------------------------

use acp_hub_proto::Frame;
use futures::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;

/// 简单 ws 客户端：认证 + 订阅 + 读帧（自动回 pong，可关闭）/发帧。
pub struct WsClient {
    pub ws: WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    /// 收到 keep_alive 是否自动回 pong（keep_alive 超时用例关闭）。
    pub auto_pong: bool,
    /// 已收到 ready（relayReady，§4.6）。
    pub ready: bool,
    pub role: &'static str,
}

impl WsClient {
    /// 连接（不做任何帧交换）。
    pub async fn connect(port: u16) -> Result<WsClient, String> {
        Self::connect_url(&format!("ws://127.0.0.1:{port}/")).await
    }

    /// 连接任意地址（§9.5 非回环用例用出口 IP）。
    pub async fn connect_url(url: &str) -> Result<WsClient, String> {
        let (ws, _resp) = tokio_tungstenite::connect_async(url)
            .await
            .map_err(|e| format!("ws connect 失败: {e}"))?;
        Ok(WsClient {
            ws,
            auto_pong: true,
            ready: false,
            role: "client",
        })
    }

    /// 发送一帧（serde 序列化）。
    pub async fn send(&mut self, frame: &Frame) -> Result<(), String> {
        let text = serde_json::to_string(frame).map_err(|e| format!("序列化失败: {e}"))?;
        self.ws
            .send(Message::Text(text.into()))
            .await
            .map_err(|e| format!("send 失败: {e}"))
    }

    /// 读一帧（超时；自动回 pong；Close → 返回 None + 记录 close code）。
    ///
    /// 硬 deadline 语义：本函数总耗时不超过 `timeout`。每次循环迭代都以
    /// 剩余时间重新计时（而非重置为完整 `timeout`）——否则 keep_alive
    /// 每 5s 到达一次、auto_pong 后 continue 会反复重置计时器，导致
    /// `recv` 永不超时（t07 挂死 60s 的根因）。
    pub async fn recv(&mut self, timeout: Duration) -> Result<Option<Frame>, String> {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            let remain = deadline
                .checked_duration_since(std::time::Instant::now())
                .unwrap_or(Duration::from_millis(1));
            let msg = tokio::time::timeout(remain, self.ws.next())
                .await
                .map_err(|_| format!("recv 超时（{}s）", timeout.as_secs()))?
                .ok_or_else(|| "ws 流已结束".to_string())?
                .map_err(|e| format!("ws 读错误: {e}"))?;
            match msg {
                Message::Text(t) => {
                    let frame = Frame::parse(&t).map_err(|e| format!("帧解析失败: {e}"))?;
                    if matches!(frame, Frame::KeepAlive(_)) && self.auto_pong {
                        self.send(&Frame::Pong(acp_hub_proto::conn::Pong {}))
                            .await?;
                        // 保活回执后仍返回该帧（不吞掉）：等 KeepAlive 的
                        // 谓词（如 t07/t02）需要看到它；其他谓词不匹配时
                        // recv_until 自然跳过，无行为差异。
                    }
                    if matches!(frame, Frame::Ready(_)) {
                        self.ready = true;
                    }
                    return Ok(Some(frame));
                }
                Message::Ping(_) => continue,
                Message::Pong(_) => continue,
                Message::Close(_) | Message::Binary(_) => {
                    return Ok(None);
                }
                _ => continue,
            }
        }
    }

    /// 读到满足谓词的帧（跳过其余；超时）。
    pub async fn recv_until(
        &mut self,
        mut pred: impl FnMut(&Frame) -> bool,
        timeout: Duration,
    ) -> Result<Frame, String> {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            let remain = deadline
                .checked_duration_since(std::time::Instant::now())
                .unwrap_or(Duration::from_millis(1));
            match self.recv(remain).await? {
                Some(f) if pred(&f) => return Ok(f),
                Some(_) => continue,
                None => return Err("连接关闭（收到 Close）".to_string()),
            }
        }
    }

    /// 读到 ws Close 帧（返回关闭码）。连接断开（EOF）→ Err。
    ///
    /// 直接读底层流，不经过 [`Self::recv`]：recv 消费 Close 帧后 tungstenite
    /// 会自动回 Close 并 EOF，再从底层读就拿不到关闭码（并行时序下偶发
    /// "连接关闭但无法读取关闭码"）。此处硬 deadline + remain 递减，与
    /// keep_alive 高频到达时的计时重置问题同源防护。
    pub async fn recv_close(&mut self, timeout: Duration) -> Result<u16, String> {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            let remain = deadline
                .checked_duration_since(std::time::Instant::now())
                .unwrap_or(Duration::from_millis(1));
            match tokio::time::timeout(remain, self.ws.next()).await {
                Ok(Some(Ok(Message::Close(Some(cf))))) => {
                    return Ok(u16::from(cf.code));
                }
                Ok(Some(Ok(Message::Close(None)))) => {
                    return Err("close 帧无关闭码".to_string());
                }
                Ok(Some(Ok(Message::Text(t)))) => {
                    if let Ok(Frame::ActionError(e)) = Frame::parse(&t).as_ref() {
                        eprintln!("[ws] 收到 action_error: {:?} {}", e.code, e.message);
                    }
                    continue;
                }
                Ok(Some(Ok(_))) => continue,
                Ok(Some(Err(e))) => return Err(format!("ws 读错误: {e}")),
                Ok(None) => {
                    return Err("连接 EOF（未收到 Close 帧）".to_string());
                }
                Err(_) => {
                    return Err(format!("等待 Close 超时（{}s）", timeout.as_secs()));
                }
            }
        }
    }

    /// 认证 + 订阅（首帧纪律 §4.6）+ 收快照/ready，返回 (快照帧, ready 帧)。
    pub async fn handshake(
        &mut self,
        token: &str,
        docs: &[&str],
    ) -> Result<(Vec<Frame>, Frame), String> {
        self.send(&Frame::Auth(acp_hub_proto::conn::Auth {
            token: token.to_string(),
        }))
        .await?;
        self.send(&Frame::YsyncSubscribe(
            acp_hub_proto::ysync::YsyncSubscribe {
                docs: docs.iter().map(|d| d.parse().unwrap()).collect(),
            },
        ))
        .await?;
        let mut snapshots = Vec::new();
        let ready = self
            .recv_until(
                |f| {
                    if matches!(f, Frame::YsyncUpdate(_)) {
                        snapshots.push(f.clone());
                        false
                    } else {
                        matches!(f, Frame::Ready(_))
                    }
                },
                RECV_TIMEOUT,
            )
            .await?;
        Ok((snapshots, ready))
    }

    /// 连接 + 认证 + 订阅 + ready（一条龙；返回注册表快照帧如订阅了 registry）。
    pub async fn connect_client(port: u16, token: &str, docs: &[&str]) -> Result<WsClient, String> {
        let mut c = WsClient::connect(port).await?;
        let (_snap, _ready) = c.handshake(token, docs).await?;
        Ok(c)
    }
}

/// 等待 action 终态帧（自动跳过前置 `accepted`；§4.4：submit 同步回
/// Accepted、执行器异步回 committed/error——create/prompt/cancel/close
/// 全时序均为两段）。返回终态 `ActionAck`（committed/duplicate 等）或
/// `ActionError`，由调用方判定。
pub async fn wait_terminal(c: &mut WsClient, timeout: Duration) -> Result<Frame, String> {
    loop {
        match c
            .recv_until(
                |f| matches!(f, Frame::ActionAck(_) | Frame::ActionError(_)),
                timeout,
            )
            .await?
        {
            Frame::ActionAck(a) if a.status == acp_hub_proto::ack::AckStatus::Accepted => {
                continue;
            }
            f @ (Frame::ActionAck(_) | Frame::ActionError(_)) => return Ok(f),
            _ => unreachable!("谓词已限定"),
        }
    }
}

// ---------------------------------------------------------------------------
// yjs 快照解析
// ---------------------------------------------------------------------------

use yrs::updates::decoder::Decode;
use yrs::{Map, ReadTxn, Transact};

/// base64 yjs update → 应用到一个新 Doc（快照重建）。
pub fn doc_from_snapshot(update_b64: &str) -> Result<yrs::Doc, String> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(update_b64)
        .map_err(|e| format!("update base64 解码失败: {e}"))?;
    let update = yrs::Update::decode_v1(&bytes).map_err(|e| format!("update 解码失败: {e}"))?;
    let doc = yrs::Doc::new();
    doc.transact_mut()
        .apply_update(update)
        .map_err(|e| format!("update 应用失败: {e}"))?;
    Ok(doc)
}

/// 从快照帧列表取指定 doc 的 update 并重建 Doc（合并多帧增量）。
pub fn doc_from_snapshots(frames: &[Frame], doc_name: &str) -> Result<yrs::Doc, String> {
    let mut bytes: Vec<u8> = Vec::new();
    for f in frames {
        if let Frame::YsyncUpdate(u) = f {
            if u.doc.as_str() == doc_name {
                let b = base64::engine::general_purpose::STANDARD
                    .decode(&u.update)
                    .map_err(|e| format!("base64 解码失败: {e}"))?;
                bytes.extend_from_slice(&b);
            }
        }
    }
    if bytes.is_empty() {
        return Err(format!("快照中无 doc {doc_name}"));
    }
    let merged = yrs::merge_updates_v1(&[bytes]).map_err(|e| format!("merge updates 失败: {e}"))?;
    let update = yrs::Update::decode_v1(&merged).map_err(|e| format!("update 解码失败: {e}"))?;
    let doc = yrs::Doc::new();
    doc.transact_mut()
        .apply_update(update)
        .map_err(|e| format!("update 应用失败: {e}"))?;
    Ok(doc)
}

/// 根 Map 的字符串字段读取。
///
/// 只用只读事务（`txn.get_map`）；`doc.get_or_insert_map` 内部走 transact_mut
/// 写锁，与已持有的读事务互斥 → 死锁（async_lock 写优先）。
pub fn root_str(doc: &yrs::Doc, key: &str) -> Option<String> {
    let txn = doc.transact();
    let root = txn.get_map("root")?;
    root.get(&txn, key).and_then(|v| v.cast::<String>().ok())
}

/// root.instances/<instance_id>/<field> 字符串读取。
pub fn instance_field(doc: &yrs::Doc, instance_id: &str, field: &str) -> Option<String> {
    let txn = doc.transact();
    let root = txn.get_map("root")?;
    let instances = root.get(&txn, "instances")?.cast::<yrs::MapRef>().ok()?;
    let m = instances
        .get(&txn, instance_id)?
        .cast::<yrs::MapRef>()
        .ok()?;
    m.get(&txn, field).and_then(|v| v.cast::<String>().ok())
}

/// root.chats/<chat_id>/<field> 字符串读取。
pub fn chat_field(doc: &yrs::Doc, chat_id: &str, field: &str) -> Option<String> {
    let txn = doc.transact();
    let root = txn.get_map("root")?;
    let chats = root.get(&txn, "chats")?.cast::<yrs::MapRef>().ok()?;
    let s = chats.get(&txn, chat_id)?.cast::<yrs::MapRef>().ok()?;
    s.get(&txn, field).and_then(|v| v.cast::<String>().ok())
}

/// root.chats 的 chat_id 集合。
pub fn chat_ids(doc: &yrs::Doc) -> Vec<String> {
    let txn = doc.transact();
    let mut out = Vec::new();
    if let Some(root) = txn.get_map("root") {
        if let Some(m) = root
            .get(&txn, "chats")
            .and_then(|v| v.cast::<yrs::MapRef>().ok())
        {
            for k in m.keys(&txn) {
                out.push(k.to_string());
            }
        }
    }
    out
}

/// root.global/status。
pub fn global_status(doc: &yrs::Doc) -> Option<String> {
    let txn = doc.transact();
    let root = txn.get_map("root")?;
    let g = root.get(&txn, "global")?.cast::<yrs::MapRef>().ok()?;
    g.get(&txn, "status").and_then(|v| v.cast::<String>().ok())
}

/// chat doc 的 entry 数量。
pub fn chat_entry_count(doc: &yrs::Doc) -> usize {
    let txn = doc.transact();
    txn.get_map("root")
        .and_then(|r| r.get(&txn, "entries"))
        .and_then(|v| v.cast::<yrs::MapRef>().ok())
        .map(|m| m.len(&txn) as usize)
        .unwrap_or(0)
}

/// chat doc 指定 entry 的文本块内容拼接（blocks[*].text）。
pub fn chat_entry_text(doc: &yrs::Doc, entry_id: &str) -> Option<String> {
    let txn = doc.transact();
    let root = txn.get_map("root")?;
    let entries = root.get(&txn, "entries")?.cast::<yrs::MapRef>().ok()?;
    let e = entries.get(&txn, entry_id)?.cast::<yrs::MapRef>().ok()?;
    let blocks = e.get(&txn, "blocks")?.cast::<yrs::MapRef>().ok()?;
    let mut text = String::new();
    for k in blocks.keys(&txn) {
        let b = blocks.get(&txn, k)?.cast::<yrs::MapRef>().ok()?;
        if let Some(t) = b.get(&txn, "text").and_then(|v| v.cast::<String>().ok()) {
            text.push_str(&t);
        }
    }
    Some(text)
}

/// Registry 快照重新拉取（新建连接，订阅 hub:registry，返回重建 Doc）。
pub async fn fetch_registry_snapshot(port: u16, token: &str) -> Result<yrs::Doc, String> {
    let mut c = WsClient::connect(port).await?;
    let (snap, _ready) = c.handshake(token, &["hub:registry"]).await?;
    let doc = doc_from_snapshots(&snap, "hub:registry")?;
    let _ = c.ws.close(None).await;
    Ok(doc)
}

/// 等待条件（轮询，间隔 200ms）。
pub async fn wait_until(
    mut f: impl FnMut() -> bool,
    timeout: Duration,
    what: &str,
) -> Result<(), String> {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if f() {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    Err(format!("等待超时（{}s）：{what}", timeout.as_secs()))
}

/// HashMap 便捷构造。
pub fn map_of<K: std::hash::Hash + Eq, V>(pairs: Vec<(K, V)>) -> HashMap<K, V> {
    pairs.into_iter().collect()
}
