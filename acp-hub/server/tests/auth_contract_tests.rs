//! F7 进程级集成测试（二）：认证失败 / 未知帧 / 非回环拒绝 / 契约向量
//! （§4.8 向量 1/6/7 的进程级验证）。

mod common;

use std::time::Duration;

use acp_hub_proto::ack::ErrorCode;
use acp_hub_proto::Frame;
use base64::Engine as _;

use common::{
    fresh_token, wait_until, ServerProc, TestEnv, WsClient, RECV_TIMEOUT, TEST_BUDGET,
};

fn t(name: &str, tag: &str, r: Result<(), String>) {
    match r {
        Ok(()) => println!("T-{name}: PASS"),
        Err(e) => println!("T-{name}: FAIL {tag} {e}"),
    }
}

/// server-only 场景（无 instance）。
struct ServerOnly {
    env: TestEnv,
    server: ServerProc,
}

impl ServerOnly {
    fn start(config: Option<&str>) -> Result<ServerOnly, String> {
        Self::start_listen(config, "127.0.0.1")
    }

    /// 指定监听地址启动（§9.5 非回环用例）。
    fn start_listen(config: Option<&str>, listen: &str) -> Result<ServerOnly, String> {
        let env = TestEnv::new();
        let cfg = config.map(|body| env.write_config(body));
        let server = ServerProc::start_listen(&env, cfg.as_deref(), listen);
        server.wait_ready()?;
        Ok(ServerOnly { env, server })
    }
}

// ---------------------------------------------------------------------------
// t06 坏 token → UNAUTHENTICATED 语义（关闭码 4502，§4.8 向量 1）
// ---------------------------------------------------------------------------

async fn t06_body() -> Result<(), String> {
    let s = ServerOnly::start(None)?;
    let mut c = WsClient::connect(s.env.port).await?;
    c.send(&Frame::Auth(acp_hub_proto::conn::Auth {
        token: "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=".to_string(),
    }))
    .await?;
    let code = c.recv_close(RECV_TIMEOUT).await?;
    assert_eq!(code, 4502, "坏 token 应以 4502 关闭");
    // 审计计数（server 日志应含 auth.client failed）。
    assert!(
        s.server
            .log_contains("auth.client", Duration::from_secs(5)),
        "server 应记录认证失败"
    );
    Ok(())
}

#[tokio::test]
async fn t06_bad_token_rejected() {
    println!("T-06-bad-token: START");
    let r = tokio::time::timeout(TEST_BUDGET, t06_body()).await;
    match r {
        Ok(r) => t("06-bad-token", "", r),
        Err(_) => println!("T-06-bad-token: FAIL 超时（60s 预算）"),
    }
}

// ---------------------------------------------------------------------------
// t07 未知帧类型 → UNSUPPORTED_FRAME（§4.8 向量 6，不静默、不 panic）
// ---------------------------------------------------------------------------

async fn t07_body() -> Result<(), String> {
    let s = ServerOnly::start(None)?;
    let mut c = WsClient::connect(s.env.port).await?;
    let (_snap, _ready) = c
        .handshake(&s.env.client_token, &["hub:registry"])
        .await?;
    // 未知 tag（不在注册表）。
    c.send(&Frame::YsyncAwareness(acp_hub_proto::ysync::YsyncAwareness {
        msg: "AAAA".to_string(),
    }))
    .await?;
    let e = c
        .recv_until(
            |f| matches!(f, Frame::ActionError(e) if e.code == ErrorCode::UnsupportedFrame),
            RECV_TIMEOUT,
        )
        .await?;
    let _ = e;
    // 已知 tag 但方向违反：客户端上行 ysync.update（§5.6 拒绝）。
    c.send(&Frame::YsyncUpdate(acp_hub_proto::ysync::YsyncUpdate {
        doc: "hub:registry".parse().unwrap(),
        update: "AAAA".to_string(),
        projection_version: None,
    }))
    .await?;
    let _ = c
        .recv_until(
            |f| matches!(f, Frame::ActionError(e) if e.code == ErrorCode::UnsupportedFrame),
            RECV_TIMEOUT,
        )
        .await?;
    // 连接仍存活（未 panic、未断开）。
    let _ = c
        .recv_until(|f| matches!(f, Frame::KeepAlive(_)), Duration::from_secs(12))
        .await?;
    Ok(())
}

#[tokio::test]
async fn t07_unknown_frame_unsupported() {
    println!("T-07-unknown-frame: START");
    let r = tokio::time::timeout(TEST_BUDGET, t07_body()).await;
    match r {
        Ok(r) => t("07-unknown-frame", "", r),
        Err(_) => println!("T-07-unknown-frame: FAIL 超时（60s 预算）"),
    }
}

// ---------------------------------------------------------------------------
// t08 非回环 peer 拒绝（§9.5：默认拒绝；allow_non_loopback=true 放行）
// ---------------------------------------------------------------------------

/// 本机非回环出口 IP（UDP 连接探测，不发包）。
fn local_non_loopback_ip() -> Option<std::net::IpAddr> {
    let udp = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    udp.connect("8.8.8.8:80").ok()?;
    udp.local_addr().ok().map(|a| a.ip())
}

async fn t08_body() -> Result<(), String> {
    let ip = local_non_loopback_ip().ok_or_else(|| {
        "无法获取本机非回环 IP（无出网路由）——用例 SKIP（不判定）".to_string()
    })?;
    if ip.is_loopback() || ip.is_unspecified() {
        return Err("出口 IP 为回环/未指定地址——用例 SKIP（不判定）".to_string());
    }

    // 1. 默认配置（allow_non_loopback=false）：监听 0.0.0.0，非回环源连接被拒
    //    （§9.5：server 记录拒绝并 drop，握手无响应 → 客户端连接失败）。
    let s = ServerOnly::start_listen(None, "0.0.0.0")?;
    let url = format!("ws://{ip}:{}/", s.env.port);
    let rejected = tokio::time::timeout(Duration::from_secs(8), async {
        tokio_tungstenite::connect_async(&url).await.is_err()
    })
    .await
    .map_err(|_| "非回环连接探测超时（8s）".to_string())?;
    assert!(
        rejected,
        "非回环连接应在默认配置下被拒（握手无响应）"
    );
    // server 日志记录拒绝。
    assert!(
        s.server
            .log_contains("connection rejected: non-loopback", Duration::from_secs(5)),
        "server 应记录非回环拒绝"
    );
    drop(s);

    // 2. 显式配置 allow_non_loopback = true → 接受（认证后正常握手）。
    let config = "allow_non_loopback = true\n";
    let s2 = ServerOnly::start_listen(Some(config), "0.0.0.0")?;
    let mut c = WsClient::connect_url(&format!("ws://{ip}:{}/", s2.env.port)).await?;
    // 放行后认证应成功（快照 + ready）。
    let (snap, _ready) = c.handshake(&s2.env.client_token, &["hub:registry"]).await?;
    assert!(!snap.is_empty(), "放行后应收到快照");
    Ok(())
}

#[tokio::test]
async fn t08_non_loopback_rejected() {
    println!("T-08-non-loopback: START");
    let r = tokio::time::timeout(TEST_BUDGET, t08_body()).await;
    match r {
        Ok(r) => t("08-non-loopback", "", r),
        Err(_) => println!("T-08-non-loopback: FAIL 超时（60s 预算）"),
    }
}

// ---------------------------------------------------------------------------
// t12 契约层（§4.8 向量 1/6/7 进程级）：无 token 连接拒绝 / 白名单外 action /
// 角色不匹配
// ---------------------------------------------------------------------------

async fn t12_body() -> Result<(), String> {
    let s = ServerOnly::start(None)?;
    let port = s.env.port;

    // a. 无 token：首帧直接 ysync.subscribe（非 auth/hello）→ 1011 断开
    //    （§4.6 首帧纪律）。
    let mut c = WsClient::connect(port).await?;
    c.send(&Frame::YsyncSubscribe(acp_hub_proto::ysync::YsyncSubscribe {
        docs: vec!["hub:registry".parse().unwrap()],
    }))
    .await?;
    let code = c.recv_close(RECV_TIMEOUT).await?;
    assert_eq!(code, 1011, "无 token 首帧应为 1011");

    // b. 空 token：auth {token: ""} → 4502。
    let mut c = WsClient::connect(port).await?;
    c.send(&Frame::Auth(acp_hub_proto::conn::Auth {
        token: String::new(),
    }))
    .await?;
    let code = c.recv_close(RECV_TIMEOUT).await?;
    assert_eq!(code, 4502, "空 token 应为 4502");

    // c. action 方法面白名单外（session/load，M2）→ UNSUPPORTED_FRAME
    //    （§4.8 向量 6：白名单外不静默）。
    let mut c = WsClient::connect_client(port, &s.env.client_token, &["hub:registry"]).await?;
    c.send(&Frame::Action(acp_hub_proto::action::ActionEnvelope::Load {
        command_id: uuid::Uuid::new_v4().to_string(),
        payload: acp_hub_proto::action::LoadChatPayload {
            chat_id: uuid::Uuid::new_v4().to_string(),
        },
    }))
    .await?;
    let e = c
        .recv_until(
            |f| matches!(f, Frame::ActionError(e) if e.code == ErrorCode::UnsupportedFrame),
            RECV_TIMEOUT,
        )
        .await?;
    let _ = e;

    // d. 角色不匹配（§4.8 向量 8 进程级）：instance/hello 提交 client token →
    //    RoleMismatch → 4502（§9.2 失败语义）。
    let mut c = WsClient::connect(port).await?;
    let hello = acp_hub_proto::instance::InstanceHello {
        token: s.env.client_token.clone(),
        hostname: "bad-role".to_string(),
        caps: serde_json::json!({}),
        buffered: Some(false),
        buffer_lost: None,
        stream_epochs: None,
        nonce: base64::engine::general_purpose::STANDARD
            .encode([0u8; 32]),
    };
    c.send(&Frame::InstanceHello(hello)).await?;
    let code = c.recv_close(RECV_TIMEOUT).await?;
    assert_eq!(code, 4502, "client token 冒充 instance hello 应为 4502");

    // e. 未知 token 的 hello → 4502（向量 7 同源）。
    let mut c = WsClient::connect(port).await?;
    let hello = acp_hub_proto::instance::InstanceHello {
        token: fresh_token(),
        hostname: "unknown".to_string(),
        caps: serde_json::json!({}),
        buffered: Some(false),
        buffer_lost: None,
        stream_epochs: None,
        nonce: base64::engine::general_purpose::STANDARD
            .encode([1u8; 32]),
    };
    c.send(&Frame::InstanceHello(hello)).await?;
    let code = c.recv_close(RECV_TIMEOUT).await?;
    assert_eq!(code, 4502, "未知 token 的 hello 应为 4502");

    // f. 连接后等待首帧超时（10s，§4.6）→ 1011（向量 1 的建立失败路径）。
    let mut c = WsClient::connect(port).await?;
    let code = c.recv_close(Duration::from_secs(13)).await?;
    assert_eq!(code, 1011, "首帧超时应以 1011 关闭");

    Ok(())
}

#[tokio::test]
async fn t12_contract_layer() {
    println!("T-12-contract-layer: START");
    let r = tokio::time::timeout(TEST_BUDGET, t12_body()).await;
    match r {
        Ok(r) => t("12-contract-layer", "", r),
        Err(_) => println!("T-12-contract-layer: FAIL 超时（60s 预算）"),
    }
}

// 保留 wait_until 引用（避免未使用告警）。
#[allow(dead_code)]
async fn _unused(port: u16, token: &str) {
    let _ = wait_until(|| false, Duration::from_secs(1), "x").await;
    let _ = (port, token);
}
