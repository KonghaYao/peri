//! Gateway 集成测试（设计稿 §16 测试 20–23、31–33 的 ws 级子集）。
//!
//! 端到端：fake client（TUI）+ fake instance 均以真实 tokio-tungstenite ws
//! 客户端连接本地 Gateway，验证：认证时序、快照 → ready → 缓冲 flush、
//! hello 双向认证（auth_response HMAC 校验）、spawn → initialize →
//! session/new → binding → prompt → 事件流 → 客户端收到 y-sync 广播、
//! 断链语义（instance 断开 → turn interrupted + session gap）。
//!
//! epoch 约定：fake instance 上报 epoch=0（F4 聚合器 stream.epoch 以 0 起始；
//! 真实 instance 的 epoch=1 首事件问题为 F4 已知缺口，见输出遗留问题）。

use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use futures::{SinkExt as _, StreamExt as _};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::connect_async;
use yrs::updates::decoder::Decode as _;
use yrs::{Map, ReadTxn, Transact};

use acp_hub_proto::frame::Frame;
use acp_hub_proto::hmac::{derive_mac_key, generate_challenge_nonce, mac_input, verify_mac};
use acp_hub_proto::version::PROTOCOL_VERSION;

use crate::auth::{AuthService, TokenRole, TokenStore, TOKENS_FILE};

use crate::config::Config;
use crate::control::Hub;
use crate::persist::{PersistConfig, Store};
use crate::state::factory::ROOT;

/// 测试装配：临时目录 + 双 token（instance/full）+ Hub。
struct TestServer {
    addr: std::net::SocketAddr,
    instance_token: String,
    client_token: String,
    _tmp: tempfile::TempDir,
    _hub_keep: Arc<Hub>,
    _task: tokio::task::JoinHandle<()>,
}

async fn start_server() -> TestServer {
    let tmp = tempfile::tempdir().unwrap();
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_test_writer()
        .try_init();
    let mut cfg = Config::defaults();
    cfg.data_dir = tmp.path().join("data");
    cfg.config_dir = tmp.path().join("config");
    std::fs::create_dir_all(&cfg.config_dir).expect("create config dir (token store)");
    // 心跳间隔放宽（60s）：e2e 客户端不回 pong（§4.7 契约——pong 超时
    // 3×interval 即 4501 关闭），短间隔会在多步业务流中误触发断开；心跳
    // 超时语义由 heartbeat_test 单测覆盖。
    cfg.heartbeat_interval = Duration::from_secs(60);
    cfg.offline_timeout = Duration::from_secs(1);
    cfg.spawn_timeout = Duration::from_secs(5);
    cfg.initialize_timeout = Duration::from_secs(5);
    cfg.binding_timeout = Duration::from_secs(5);

    let mut token_store = TokenStore::load(&cfg.config_dir.join(TOKENS_FILE)).unwrap();
    let instance_rec = token_store
        .generate(TokenRole::Instance, "local")
        .unwrap();
    let client_rec = token_store.generate(TokenRole::Full, "tui").unwrap();
    let auth = Arc::new(tokio::sync::Mutex::new(AuthService::new(token_store)));

    let persist_cfg = PersistConfig {
        data_dir: cfg.data_dir.clone(),
        ..Default::default()
    };
    let store = Arc::new(Store::open(&persist_cfg).unwrap());
    store.recover().await;

    let hub = Arc::new(Hub::assemble(&cfg, store, auth).await.unwrap());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let gateway = hub.gateway.clone();
    let task = tokio::spawn(async move {
        let _ = gateway.run(listener).await;
    });
    TestServer {
        addr,
        instance_token: instance_rec.token,
        client_token: client_rec.token,
        _tmp: tmp,
        _hub_keep: hub,
        _task: task,
    }
}

/// 工具：ws 连接（split stream）上的下一帧（文本）。
type SplitWs<'a> = futures::stream::SplitStream<
    tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
>;

/// 工具：等待下一帧 ActionAck（跳过心跳 keep_alive 与状态广播——投影
/// 广播先于 committed 到达（§6.4 提交点纪律：user entry 投影在 L3 前），
/// 测试心跳间隔短于业务流，防止时序抖动）。
async fn next_action_ack(stream: &mut SplitWs<'_>) -> Frame {
    loop {
        match next_frame(stream).await {
            ack @ Frame::ActionAck(_) => return ack,
            Frame::KeepAlive(_) | Frame::Pong(_) | Frame::YsyncUpdate(_) => continue,
            other => return other,
        }
    }
}

async fn next_frame(stream: &mut SplitWs<'_>) -> Frame {
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(5), stream.next())
            .await
            .expect("frame timeout")
            .expect("stream alive")
            .expect("no ws error");
        match msg {
            Message::Text(t) => return Frame::parse(t.as_str()).expect("parse frame"),
            Message::Ping(_) => continue,
            other => panic!("unexpected ws message: {other:?}"),
        }
    }
}

#[tokio::test]
async fn client_handshake_snapshot_then_ready() {
    let server = start_server().await;
    let url = format!("ws://{}/", server.addr);
    let (ws, _) = connect_async(url).await.unwrap();
    let (mut sink, mut stream) = ws.split();

    // auth。
    sink.send(Message::Text(
        serde_json::to_string(&Frame::Auth(acp_hub_proto::conn::Auth {
            token: server.client_token.clone(),
        }))
        .unwrap()
        .into(),
    ))
    .await
    .unwrap();
    // subscribe registry。
    sink.send(Message::Text(
        serde_json::to_string(&Frame::YsyncSubscribe(
            acp_hub_proto::ysync::YsyncSubscribe {
                docs: vec![acp_hub_proto::conn::DocId::REGISTRY],
            },
        ))
        .unwrap()
        .into(),
    ))
    .await
    .unwrap();

    // 快照（ysync.update，带 projection_version）→ ready（§4.6 步骤 3/4）。
    let snap = next_frame(&mut stream).await;
    match snap {
        Frame::YsyncUpdate(u) => {
            assert_eq!(u.doc, acp_hub_proto::conn::DocId::REGISTRY);
            assert!(u.projection_version.is_some(), "快照必带 projection_version");
            // 快照可解码。
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(&u.update)
                .unwrap();
            assert!(!bytes.is_empty());
        }
        other => panic!("expected snapshot, got {other:?}"),
    }
    let ready = next_frame(&mut stream).await;
    match ready {
        Frame::Ready(r) => {
            assert!(r.projection_versions.contains_key(&acp_hub_proto::conn::DocId::REGISTRY));
        }
        other => panic!("expected ready, got {other:?}"),
    }
    drop(sink);
    let _ = stream;
}

#[tokio::test]
async fn client_bad_token_closed_no_data() {
    let server = start_server().await;
    let url = format!("ws://{}/", server.addr);
    let (ws, _) = connect_async(url).await.unwrap();
    let (mut sink, mut stream) = ws.split();
    sink.send(Message::Text(
        serde_json::to_string(&Frame::Auth(acp_hub_proto::conn::Auth {
            token: "bogus-token".into(),
        }))
        .unwrap()
        .into(),
    ))
    .await
    .unwrap();
    // 无任何业务数据（§9.2 失败语义：断开）。
    let msg = tokio::time::timeout(Duration::from_secs(5), stream.next())
        .await
        .expect("should close")
        .expect("stream")
        .expect("no error");
    match msg {
        Message::Close(_) => {}
        other => panic!("expected close, got {other:?}"),
    }
    drop(sink);
}

/// fake instance：hello（nonce + 双向认证校验）→ 帧循环（spawn/JSON-RPC
/// 应答）。
async fn fake_instance_connect(server: &TestServer) -> (
    tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
) {
    let url = format!("ws://{}/", server.addr);
    let (ws, _) = connect_async(url).await.unwrap();
    let (mut sink, mut stream) = ws.split();
    let nonce = generate_challenge_nonce();
    let hello = Frame::InstanceHello(acp_hub_proto::instance::InstanceHello {
        token: server.instance_token.clone(),
        hostname: "local".into(),
        caps: serde_json::json!({}),
        buffered: None,
        buffer_lost: None,
        stream_epochs: None,
        nonce: base64::engine::general_purpose::STANDARD.encode(nonce),
    });
    sink.send(Message::Text(serde_json::to_string(&hello).unwrap().into()))
        .await
        .unwrap();
    // auth_response：校验 server 身份（§9.2 步骤 2；机器侧校验通过前不执行
    // 任何 spawn/kill——测试中以校验结果断言）。
    let resp = next_frame(&mut stream).await;
    match resp {
        Frame::AuthResponse(r) => {
            let token_bytes = base64::engine::general_purpose::STANDARD
                .decode(&server.instance_token)
                .unwrap();
            let key = derive_mac_key(
                &token_bytes.try_into().unwrap(),
                TokenRole::Instance.as_str(),
            );
            let ctx_bytes = base64::engine::general_purpose::STANDARD
                .decode(&r.connection_context)
                .unwrap();
            let input = mac_input(
                &nonce,
                &ctx_bytes.try_into().unwrap(),
                &PROTOCOL_VERSION.to_string(),
                TokenRole::Instance.as_str(),
            );
            verify_mac(&key, &input, &r.hmac).expect("server HMAC must verify");
        }
        other => panic!("expected auth_response, got {other:?}"),
    }
    // 校验完成：重新合并流并返回（调用方决定连接后续；不触发 unreachable）。
    match sink.reunite(stream) {
        Ok(ws) => (ws,),
        Err(_) => panic!("ws sink/stream reunite failed"),
    }
}

#[tokio::test]
async fn instance_handshake_hmac_verified() {
    let server = start_server().await;
    let _ = fake_instance_connect(&server).await;
}

/// 工具：y-sync v1 更新应用到本地 doc（hub.rs `apply_update` 同构，测试
/// 侧本地重放解码投影）。
fn apply_update(doc: &yrs::Doc, update: &[u8]) {
    match yrs::Update::decode_v1(update) {
        Ok(parsed) => {
            let mut txn = doc.transact_mut();
            if let Err(e) = txn.apply_update(parsed) {
                panic!("apply update failed: {e}");
            }
        }
        Err(e) => panic!("update decode failed: {e}"),
    }
}

/// 接线回归（F7 机器列表）：instance hello 注册成功后，`hub:registry` 的
/// `instances` 投影非空且字段正确（§5.5/§7.1）。曾因
/// `RegistryState::upsert_instance` 零调用者导致前端机器列表永远空白。
#[tokio::test]
async fn instance_hello_populates_registry_instances_projection() {
    let server = start_server().await;

    // ---- client：先订阅 hub:registry（快照时序 §4.6）----
    let url = format!("ws://{}/", server.addr);
    let (cws, _) = connect_async(url).await.unwrap();
    let (mut csink, mut cstream) = cws.split();
    csink
        .send(Message::Text(
            serde_json::to_string(&Frame::Auth(acp_hub_proto::conn::Auth {
                token: server.client_token.clone(),
            }))
            .unwrap()
            .into(),
        ))
        .await
        .unwrap();
    csink
        .send(Message::Text(
            serde_json::to_string(&Frame::YsyncSubscribe(
                acp_hub_proto::ysync::YsyncSubscribe {
                    docs: vec![acp_hub_proto::conn::DocId::REGISTRY],
                },
            ))
            .unwrap()
            .into(),
        ))
        .await
        .unwrap();

    // ---- fake instance hello（注册 + 双向认证）----
    let _ = fake_instance_connect(&server).await;

    // ---- 累积解码快照/增量（快照先于 hello 发出时 instances 为空，由
    // hello 后的广播增量补齐），直至 instances 投影出现 ----
    let doc = yrs::Doc::new();
    let mut instances_seen = false;
    for _ in 0..8 {
        match next_frame(&mut cstream).await {
            Frame::YsyncUpdate(u) if u.doc == acp_hub_proto::conn::DocId::REGISTRY => {
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(&u.update)
                    .unwrap();
                apply_update(&doc, &bytes);
                let txn = doc.transact();
                let root = txn.get_map(ROOT).unwrap();
                let instances = root
                    .get(&txn, "instances")
                    .and_then(|v| v.cast::<yrs::MapRef>().ok());
                if let Some(instances) = instances {
                    if let Some(mm) = instances
                        .get(&txn, "local")
                        .and_then(|v| v.cast::<yrs::MapRef>().ok())
                    {
                        // token name（机器名）即 instance_id（gateway 认证后
                        // ctx.name）；start_server 以 "local" 生成 instance token。
                        assert_eq!(mm.get(&txn, "id"), Some(yrs::Out::Any("local".into())));
                        assert_eq!(mm.get(&txn, "hostname"), Some(yrs::Out::Any("local".into())));
                        assert_eq!(mm.get(&txn, "status"), Some(yrs::Out::Any("online".into())));
                        assert_eq!(mm.get(&txn, "chat_count"), Some(yrs::Out::Any(0f64.into())));
                        instances_seen = true;
                        break;
                    }
                }
            }
            Frame::KeepAlive(_) | Frame::Pong(_) | Frame::Ready(_) => continue,
            other => panic!("unexpected frame while waiting for registry instances: {other:?}"),
        }
    }
    assert!(instances_seen, "hello 后 hub:registry instances 投影应非空（§7.1 接线）");
    drop(csink);
    let _ = cstream;
}

/// 端到端：create → spawn → initialize → session/new → binding → prompt →
/// 事件流 → 客户端收到 y-sync 广播（设计稿 §16 测试 32 的核心闭环）。
#[tokio::test]
async fn e2e_create_prompt_event_broadcast() {
    let server = start_server().await;
    // ---- fake client ----
    let url = format!("ws://{}/", server.addr);
    let (cws, _) = connect_async(url).await.unwrap();
    let (mut csink, mut cstream) = cws.split();
    csink
        .send(Message::Text(
            serde_json::to_string(&Frame::Auth(acp_hub_proto::conn::Auth {
                token: server.client_token.clone(),
            }))
            .unwrap()
            .into(),
        ))
        .await
        .unwrap();

    // ---- fake instance ----
    let url = format!("ws://{}/", server.addr);
    let (mws, _) = connect_async(url).await.unwrap();
    let (mut msink, mut mstream) = mws.split();
    let nonce = generate_challenge_nonce();
    msink
        .send(Message::Text(
            serde_json::to_string(&Frame::InstanceHello(
                acp_hub_proto::instance::InstanceHello {
                    token: server.instance_token.clone(),
                    hostname: "local".into(),
                    caps: serde_json::json!({}),
                    buffered: None,
                    buffer_lost: None,
                    stream_epochs: None,
                    nonce: base64::engine::general_purpose::STANDARD.encode(nonce),
                },
            ))
            .unwrap()
            .into(),
        ))
        .await
        .unwrap();
    // instance 校验 auth_response（§9.2）。
    match next_frame(&mut mstream).await {
        Frame::AuthResponse(r) => {
            let token_bytes = base64::engine::general_purpose::STANDARD
                .decode(&server.instance_token)
                .unwrap();
            let key = derive_mac_key(
                &token_bytes.try_into().unwrap(),
                TokenRole::Instance.as_str(),
            );
            let ctx_bytes = base64::engine::general_purpose::STANDARD
                .decode(&r.connection_context)
                .unwrap();
            let input = mac_input(
                &nonce,
                &ctx_bytes.try_into().unwrap(),
                &PROTOCOL_VERSION.to_string(),
                TokenRole::Instance.as_str(),
            );
            verify_mac(&key, &input, &r.hmac).expect("HMAC verify");
        }
        other => panic!("expected auth_response, got {other:?}"),
    }

    // ---- client 订阅 chat doc（create 前先订阅，快照时序 §4.6）----
    // create 的 chat_id 未知，订阅 registry 建立 ready；create committed
    // 后再订阅 chat:{sid}。
    csink
        .send(Message::Text(
            serde_json::to_string(&Frame::YsyncSubscribe(
                acp_hub_proto::ysync::YsyncSubscribe {
                    docs: vec![acp_hub_proto::conn::DocId::REGISTRY],
                },
            ))
            .unwrap()
            .into(),
        ))
        .await
        .unwrap();
    // 快照 + ready。
    assert!(matches!(next_frame(&mut cstream).await, Frame::YsyncUpdate(_)));
    assert!(matches!(next_frame(&mut cstream).await, Frame::Ready(_)));

    // ---- client session/create ----
    let create_cid = uuid::Uuid::new_v4().to_string();
    csink
        .send(Message::Text(
            serde_json::to_string(&Frame::Action(
                acp_hub_proto::action::ActionEnvelope::Create {
                    command_id: create_cid.clone(),
                    payload: acp_hub_proto::action::CreateChatPayload {
                        instance_id: Some("local".into()),
                        cwd: None,
                        title: Some("e2e".into()),
                        acp_session_id: None,
                        workspace_id: None,
                    },
                },
            ))
            .unwrap()
            .into(),
        ))
        .await
        .unwrap();
    // accepted（create 执行器先写 Registry 摘要 → 订阅了 hub:registry 的
    // 客户端会先收到 registry 更新帧，§5.2 单写广播；跳过至 ActionAck）。
    let accepted = loop {
        match next_frame(&mut cstream).await {
            Frame::ActionAck(a) if a.command_id == create_cid => break a,
            Frame::YsyncUpdate(_) => continue, // registry 更新（accepting 摘要）
            other => panic!("expected accepted, got {other:?}"),
        }
    };
    assert_eq!(accepted.status, acp_hub_proto::ack::AckStatus::Accepted);
    // instance 收 instance/spawn → 回 spawn_ack。
    let chat_id = match next_frame(&mut mstream).await {
        Frame::InstanceSpawn(s) => {
            assert_eq!(s.command_id, create_cid);
            msink
                .send(Message::Text(
                    serde_json::to_string(&Frame::InstanceSpawnAck(
                        acp_hub_proto::instance::InstanceSpawnAck {
                            command_id: s.command_id.clone(),
                            chat_id: s.chat_id.clone(),
                            ok: true,
                            error: None,
                        },
                    ))
                    .unwrap()
                    .into(),
                ))
                .await
                .unwrap();
            s.chat_id
        }
        other => panic!("expected spawn, got {other:?}"),
    };
    // instance 收 initialize（instance/forward 帧）→ 回 forward_ack（L1+L2，
    // §4.4 M1 合并）→ 经 instance/event 回 JSON-RPC response（§4.5：机器上行
    // 统一帧面；initialize/session/new 响应在 binding 建立前到达，server 以
    // pending_rpc 匹配（rpc_id → command_id，§4.4 L3），无 binding 依赖）。
    let init = match next_frame(&mut mstream).await {
        Frame::InstanceForward(f) => f,
        other => panic!("expected forward(initialize), got {other:?}"),
    };
    assert_eq!(init.frame["method"], serde_json::json!("initialize"));
    msink
        .send(Message::Text(
            serde_json::to_string(&Frame::InstanceForwardAck(
                acp_hub_proto::instance::InstanceForwardAck {
                    command_id: init.command_id.clone(),
                    chat_id: init.chat_id.clone(),
                    ok: true,
                    error: None,
                },
            ))
            .unwrap()
            .into(),
        ))
        .await
        .unwrap();
    msink
        .send(Message::Text(
            serde_json::to_string(&Frame::InstanceEvent(
                acp_hub_proto::instance::InstanceEvent {
                    chat_id: String::new(),
                    epoch: 0,
                    seq: 1,
                    frame: serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": init.frame["id"].clone(),
                        "result": {"ok": true}
                    }),
                },
            ))
            .unwrap()
            .into(),
        ))
        .await
        .unwrap();
    // instance 收 session/new（instance/forward）→ forward_ack → response。
    let new = match next_frame(&mut mstream).await {
        Frame::InstanceForward(f) => f,
        other => panic!("expected forward(session/new), got {other:?}"),
    };
    assert_eq!(new.frame["method"], serde_json::json!("session/new"));
    msink
        .send(Message::Text(
            serde_json::to_string(&Frame::InstanceForwardAck(
                acp_hub_proto::instance::InstanceForwardAck {
                    command_id: new.command_id.clone(),
                    chat_id: new.chat_id.clone(),
                    ok: true,
                    error: None,
                },
            ))
            .unwrap()
            .into(),
        ))
        .await
        .unwrap();
    msink
        .send(Message::Text(
            serde_json::to_string(&Frame::InstanceEvent(
                acp_hub_proto::instance::InstanceEvent {
                    chat_id: String::new(),
                    epoch: 0,
                    seq: 2,
                    frame: serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": new.frame["id"].clone(),
                        "result": {"sessionId": "acp-e2e-1"}
                    }),
                },
            ))
            .unwrap()
            .into(),
        ))
        .await
        .unwrap();
    // client 收 committed{sessionId}（§6.2 binding 建立；跳过心跳帧）。
    match next_action_ack(&mut cstream).await {
        Frame::ActionAck(a) => {
            assert_eq!(a.status, acp_hub_proto::ack::AckStatus::Committed);
            assert_eq!(a.chat_id.as_deref(), Some(chat_id.as_str()));
        }
        other => panic!("expected committed, got {other:?}"),
    }

    // ---- client 订阅 chat doc + prompt ----
    csink
        .send(Message::Text(
            serde_json::to_string(&Frame::YsyncSubscribe(
                acp_hub_proto::ysync::YsyncSubscribe {
                    docs: vec![acp_hub_proto::conn::DocId::chat(&chat_id)],
                },
            ))
            .unwrap()
            .into(),
        ))
        .await
        .unwrap();
    // 二次订阅：非首订阅 → 快照（无 ready；跳过心跳 keep_alive 帧）。
    loop {
        match next_frame(&mut cstream).await {
            Frame::YsyncUpdate(_) => break,
            Frame::KeepAlive(_) => continue,
            other => panic!("expected snapshot, got {other:?}"),
        }
    }

    let prompt_cid = uuid::Uuid::new_v4().to_string();
    csink
        .send(Message::Text(
            serde_json::to_string(&Frame::Action(
                acp_hub_proto::action::ActionEnvelope::Prompt {
                    command_id: prompt_cid.clone(),
                    payload: acp_hub_proto::action::PromptChatPayload {
                        chat_id: chat_id.clone(),
                        message: "hello e2e".into(),
                    },
                },
            ))
            .unwrap()
            .into(),
        ))
        .await
        .unwrap();
    assert!(matches!(
        next_action_ack(&mut cstream).await,
        Frame::ActionAck(a) if a.status == acp_hub_proto::ack::AckStatus::Accepted
    ));
    // instance 收 prompt（instance/forward）→ forward_ack → 经 instance/event 回
    // response（L3，§4.4）。
    let prompt = match next_frame(&mut mstream).await {
        Frame::InstanceForward(f) => f,
        other => panic!("expected forward(prompt), got {other:?}"),
    };
    assert_eq!(prompt.frame["method"], serde_json::json!("session/prompt"));
    // agent-client-protocol（peri acp 实测）：prompt 为 ContentBlock 序列，
    // 无 turnId（宿主侧归位，§7.2）——事件帧走真实 peri 形态（session/update
    // 包裹），聚合器按 active_turn 归位。
    assert_eq!(
        prompt.frame["params"]["prompt"],
        serde_json::json!([{ "type": "text", "text": "hello e2e" }])
    );
    assert!(prompt.frame["params"].get("turnId").is_none());
    msink
        .send(Message::Text(
            serde_json::to_string(&Frame::InstanceForwardAck(
                acp_hub_proto::instance::InstanceForwardAck {
                    command_id: prompt.command_id.clone(),
                    chat_id: prompt.chat_id.clone(),
                    ok: true,
                    error: None,
                },
            ))
            .unwrap()
            .into(),
        ))
        .await
        .unwrap();
    // ---- instance 上报事件（epoch=0）→ client 收 y-sync 广播 ----
    // 信封 chat_id = 进程归属（hub session id，§4.5.1）；帧内 sessionId =
    // acp_session_id（可信 binding 校验键，§495）。
    // 顺序语义（真实 peri）：流式 chunk 先于 prompt 响应（stopReason）到达——
    // L3 确认会触发 turn 终态（Completed），晚到的增量将被终态守卫拒绝（§6.3）。
    msink
        .send(Message::Text(
            serde_json::to_string(&Frame::InstanceEvent(
                acp_hub_proto::instance::InstanceEvent {
                    chat_id: chat_id.clone(),
                    epoch: 0,
                    seq: 1,
                    frame: serde_json::json!({
                        "jsonrpc": "2.0",
                        "method": "session/update",
                        "params": {
                            "sessionId": "acp-e2e-1",
                            "update": {
                                "sessionUpdate": "agent_message_chunk",
                                "content": {"type": "text", "text": "streamed reply"}
                            }
                        }
                    }),
                },
            ))
            .unwrap()
            .into(),
        ))
        .await
        .unwrap();
    // L3 response（prompt 确认，§4.4）。
    msink
        .send(Message::Text(
            serde_json::to_string(&Frame::InstanceEvent(
                acp_hub_proto::instance::InstanceEvent {
                    chat_id: "acp-e2e-1".into(),
                    epoch: 0,
                    seq: 2,
                    frame: serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": prompt.frame["id"].clone(),
                        "result": {"ok": true}
                    }),
                },
            ))
            .unwrap()
            .into(),
        ))
        .await
        .unwrap();
    // client 收 committed + 广播（到达顺序不定：事件 flush 窗口 16ms 与 L3
    // 处理路径竞态——事件广播可能先于 committed 到达，被顺序断言吞掉）。
    let mut got_broadcast = false;
    let mut got_committed = false;
    for _ in 0..12 {
        match tokio::time::timeout(Duration::from_secs(5), cstream.next()).await {
            Ok(Some(Ok(Message::Text(t)))) => {
                match Frame::parse(&t) {
                    Ok(Frame::YsyncUpdate(u)) if u.doc == acp_hub_proto::conn::DocId::chat(&chat_id) => {
                        got_broadcast = true;
                    }
                    Ok(Frame::ActionAck(a)) if a.status == acp_hub_proto::ack::AckStatus::Committed => {
                        got_committed = true;
                    }
                    Ok(Frame::KeepAlive(_)) | Ok(Frame::Pong(_)) => {}
                    Ok(other) => {
                        panic!("unexpected frame during committed/broadcast wait: {other:?}");
                    }
                    Err(_) => continue,
                }
            }
            other => panic!("unexpected: {other:?}"),
        }
        if got_broadcast && got_committed {
            break;
        }
    }
    assert!(got_committed, "client should receive committed ack");
    assert!(got_broadcast, "client should receive y-sync broadcast");

    drop(csink);
    drop(msink);
    let _ = (cstream, mstream);
}

/// 断链语义：instance 断开 → session 置 gap + 活动 turn interrupted（§8.2）。
#[tokio::test]
async fn instance_disconnect_gaps_chat() {
    let server = start_server().await;
    // 建立 instance 连接并完成一个 create（复用 e2e 前半段）→ 简化：直接
    // 通过注册表登记 session，再断开 instance 验证 relay 清理。
    // 端到端断言经 Registry Doc 不可行（内部），这里验证 relay 层行为已在
    // relay_event_handler_test 覆盖；本测试验证 gateway 断开路径触发
    // on_instance_disconnect（session 状态 Gap）。
    let url = format!("ws://{}/", server.addr);
    let (mws, _) = connect_async(url).await.unwrap();
    let (mut msink, mut mstream) = mws.split();
    let nonce = generate_challenge_nonce();
    msink
        .send(Message::Text(
            serde_json::to_string(&Frame::InstanceHello(
                acp_hub_proto::instance::InstanceHello {
                    token: server.instance_token.clone(),
                    hostname: "local".into(),
                    caps: serde_json::json!({}),
                    buffered: None,
                    buffer_lost: None,
                    stream_epochs: None,
                    nonce: base64::engine::general_purpose::STANDARD.encode(nonce),
                },
            ))
            .unwrap()
            .into(),
        ))
        .await
        .unwrap();
    // 等 auth_response。
    match next_frame(&mut mstream).await {
        Frame::AuthResponse(_) => {}
        other => panic!("expected auth_response, got {other:?}"),
    }
    // 用 coordinator 直接登记一个 session（hub 内部句柄）。
    let hub = server._hub_keep.clone();
    let sid = uuid::Uuid::new_v4().to_string();
    hub.chats.register(&sid, "local", Some("t"), "/", None).await.unwrap();
    hub.doc.open_chat(&sid, "local", Some("t"), None, None).await.unwrap();
    hub.chats.bind(&sid, "acp-disc").await.unwrap();
    hub.chats.set_active_turn(&sid, "t1").await;
    let _ = hub;

    // 断开 instance（Close 帧）→ gateway 触发 on_instance_disconnect。
    msink
        .send(Message::Close(None))
        .await
        .unwrap();
    // 等待清理完成（轮询 session 状态）。
    let mut gapped = false;
    for _ in 0..50 {
        tokio::time::sleep(Duration::from_millis(50)).await;
        if let Some(e) = server._hub_keep.chats.entry(&sid).await {
            if e.state == crate::control::ChatState::Gap {
                gapped = true;
                break;
            }
        }
    }
    assert!(gapped, "session should be gapped after instance disconnect (§8.2)");
    let _ = mstream;
}
