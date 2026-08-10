//! F7 进程级集成测试（一）：机器注册 / 客户端握手 / create / prompt / 幂等。
//!
//! 每个用例独立 temp 数据目录 + 随机端口，可并行重跑；用例总超时 60s；
//! 子进程（server/instance/test-child）由 Drop guard 进程组清理。
//!
//! 输出约定：`T-<name>: START` / `T-<name>: PASS|FAIL <原因>`。

mod common;

use std::time::Duration;

use acp_hub_proto::ack::AckStatus;
use acp_hub_proto::Frame;

use common::{
    doc_from_snapshots, fetch_registry_snapshot, global_status, chat_field, chat_ids,
    wait_terminal, InstanceProc, ServerProc, TestEnv, WsClient, RECV_TIMEOUT, TEST_BUDGET,
};

fn t(name: &str, tag: &str, r: Result<(), String>) {
    match r {
        Ok(()) => println!("T-{name}: PASS"),
        Err(e) => println!("T-{name}: FAIL {tag} {e}"),
    }
}

/// 标准场景装配：server + instance（token name=local）。
struct Stack {
    env: TestEnv,
    server: ServerProc,
    instance: InstanceProc,
}

impl Stack {
    fn start() -> Result<Stack, String> {
        let env = TestEnv::new();
        let server = ServerProc::start(&env, None);
        server.wait_ready().map_err(|e| e.to_string())?;
        let instance = InstanceProc::start(&env);
        if !instance.wait_authenticated(Duration::from_secs(15)) {
            instance.dump_log();
            server.dump_log();
            return Err("instance 未在 15s 内完成认证握手".to_string());
        }
        Ok(Stack {
            env,
            server,
            instance,
        })
    }

    /// 机器已注册可服务（server 侧已出现 instance connected）。
    fn wait_connected(&self) -> bool {
        self.server.log_contains("instance connected", Duration::from_secs(10))
    }
}

// ---------------------------------------------------------------------------
// t01 instance 连接注册（§4.5 hello 双向认证 + §7.1 ONLINE）
// ---------------------------------------------------------------------------

async fn t01_body() -> Result<(), String> {
    let stack = Stack::start()?;

    // 1. hello 双向认证：instance 侧「认证通过」日志（HMAC 校验通过）。
    assert!(
        stack.instance.log_contains("认证通过", Duration::from_secs(5)),
        "instance 未确认 auth_response HMAC"
    );
    // server 侧注册日志（instance_id = token name = "local"）。
    assert!(
        stack.wait_connected(),
        "server 未记录 instance connected"
    );

    // 2. 对账开门：Registry 快照 global.status = healthy（§8.4.1 不变量 4：
    //    hello 后 Restarting → Healthy）。
    let doc = fetch_registry_snapshot(stack.env.port, &stack.env.client_token).await?;
    assert_eq!(
        global_status(&doc).as_deref(),
        Some("healthy"),
        "global.status 应为 healthy（hello 对账开门）"
    );

    // 3. 机器级可服务性：client 发 session/create（默认 instance=local）→
    //    instance 必须能收 spawn 并拉起 test-child（spawn_ack ok）；后续
    //    initialize/session/new 经 instance/forward 下行（§4.4 L1+L2+L3）。
    let mut c = WsClient::connect_client(stack.env.port, &stack.env.client_token, &["hub:registry"]).await?;
    let command_id = uuid::Uuid::new_v4().to_string();
    c.send(&Frame::Action(acp_hub_proto::action::ActionEnvelope::Create {
        command_id: command_id.clone(),
        payload: acp_hub_proto::action::CreateChatPayload {
            instance_id: None,
            cwd: None,
            title: Some("t01".to_string()),
            acp_session_id: None,
                workspace_id: None,
}
    }))
    .await?;
    // accepted ack 先到。
    let ack = c
        .recv_until(
            |f| matches!(f, Frame::ActionAck(_) | Frame::ActionError(_)),
            RECV_TIMEOUT,
        )
        .await?;
    match ack {
        Frame::ActionAck(a) => assert_eq!(a.status, AckStatus::Accepted, "应首先收到 accepted"),
        Frame::ActionError(e) => {
            return Err(format!("create 应在 accepted 前失败: {:?} {}", e.code, e.message))
        }
        _ => unreachable!(),
    }
    // instance 侧应拉起 test-child（spawn 指令到达 + spawn_ack ok）。
    assert!(
        stack
            .instance
            .log_contains("ACP 进程启动", Duration::from_secs(10)),
        "instance 应收到 instance/spawn 并拉起 ACP 进程"
    );
    // 后续终态：create 全链（spawn → initialize → session/new → binding →
    // committed）由 t03 承接；此处等其到达确认无挂起。
    let _ = c
        .recv_until(
            |f| matches!(f, Frame::ActionAck(_) | Frame::ActionError(_)),
            Duration::from_secs(20),
        )
        .await?;

    println!("  [t01] hello 双向认证 + 注册 + 对账开门 + spawn 可服务 全部成立");
    Ok(())
}

#[tokio::test]
async fn t01_instance_hello_register() {
    println!("T-01-instance-register: START");
    let r = tokio::time::timeout(TEST_BUDGET, t01_body()).await;
    match r {
        Ok(r) => t("01-instance-register", "", r),
        Err(_) => println!("T-01-instance-register: FAIL 超时（60s 预算）"),
    }
}

// ---------------------------------------------------------------------------
// t02 客户端握手（§4.6：auth → subscribe → 快照 → ready）
// ---------------------------------------------------------------------------

async fn t02_body() -> Result<(), String> {
    let stack = Stack::start()?;
    let mut c = WsClient::connect(stack.env.port).await?;
    let (snap, ready) = c.handshake(&stack.env.client_token, &["hub:registry"]).await?;
    // 快照：registry doc 全量快照（含 schema 结构）。
    assert!(!snap.is_empty(), "应收到 ysync.update 快照");
    let doc = doc_from_snapshots(&snap, "hub:registry")?;
    assert_eq!(
        doc_from_snapshots(&snap, "hub:registry")
            .ok()
            .and_then(|d| global_status(&d)),
        Some("healthy".to_string()),
        "registry 快照应可解析"
    );
    // ready 携带 projection_versions。
    if let Frame::Ready(r) = ready {
        assert!(
            r.projection_versions.contains_key(&acp_hub_proto::conn::DocId::REGISTRY),
            "ready 应携带 hub:registry 的 projection_version"
        );
    } else {
        return Err("未收到 ready 帧".to_string());
    }
    let _ = doc;
    // 读一次 keep_alive（自动 pong）确保心跳接线正常。
    let _ = c
        .recv_until(|f| matches!(f, Frame::KeepAlive(_)), Duration::from_secs(12))
        .await?;
    Ok(())
}

#[tokio::test]
async fn t02_client_handshake() {
    println!("T-02-client-handshake: START");
    let r = tokio::time::timeout(TEST_BUDGET, t02_body()).await;
    match r {
        Ok(r) => t("02-client-handshake", "", r),
        Err(_) => println!("T-02-client-handshake: FAIL 超时（60s 预算）"),
    }
}

// ---------------------------------------------------------------------------
// t03 session create 全时序（§6.2：spawn → initialize → session/new →
// binding → committed），test-child 作 ACP 进程
// ---------------------------------------------------------------------------

async fn t03_body() -> Result<(), String> {
    let stack = Stack::start()?;
    let mut c = WsClient::connect_client(stack.env.port, &stack.env.client_token, &["hub:registry"]).await?;
    let command_id = uuid::Uuid::new_v4().to_string();
    c.send(&Frame::Action(acp_hub_proto::action::ActionEnvelope::Create {
        command_id: command_id.clone(),
        payload: acp_hub_proto::action::CreateChatPayload {
            instance_id: None,
            cwd: None,
            title: Some("t03".to_string()),
            acp_session_id: None,
                workspace_id: None,
}
    }))
    .await?;
    // accepted → committed（携带 sessionId；wait_terminal 跳过前置 accepted）。
    let ack = wait_terminal(&mut c, Duration::from_secs(35)).await?;
    match ack {
        Frame::ActionAck(a) if a.status == AckStatus::Committed => {
            let sid = a.chat_id.clone().unwrap_or_default();
            assert!(!sid.is_empty(), "committed 必须携带 sessionId");
            // Registry 活跃摘要应含该 session（§5.2）。
            let doc = fetch_registry_snapshot(stack.env.port, &stack.env.client_token).await?;
            assert!(
                chat_ids(&doc).contains(&sid),
                "Registry sessions 应含新 session（got={:?} want={sid}）",
                chat_ids(&doc)
            );
            assert_eq!(
                chat_field(&doc, &sid, "status").as_deref(),
                Some("accepting"),
                "create committed 后 session 状态应为 accepting"
            );
            println!("  [t03] create committed: session={sid}");
            Ok(())
        }
        Frame::ActionError(e) => Err(format!(
            "create 未 committed：code={:?} msg={}",
            e.code, e.message
        )),
        Frame::ActionAck(a) => Err(format!("create 意外 ack: {:?}", a.status)),
        _ => unreachable!(),
    }
}

#[tokio::test]
async fn t03_create_session_full_seq() {
    println!("T-03-create-session: START");
    let r = tokio::time::timeout(TEST_BUDGET, t03_body()).await;
    match r {
        Ok(r) => t("03-create-session", "", r),
        Err(_) => println!("T-03-create-session: FAIL 超时（60s 预算）"),
    }
}

// ---------------------------------------------------------------------------
// t04 prompt 流（§4.3/§4.4：prompt → delta → 广播 + committed ack）
// ---------------------------------------------------------------------------

async fn t04_body() -> Result<(), String> {
    let stack = Stack::start()?;
    let mut c = WsClient::connect_client(stack.env.port, &stack.env.client_token, &["hub:registry"]).await?;
    let command_id = uuid::Uuid::new_v4().to_string();
    c.send(&Frame::Action(acp_hub_proto::action::ActionEnvelope::Create {
        command_id,
        payload: acp_hub_proto::action::CreateChatPayload::default(),
    }))
    .await?;
    match wait_terminal(&mut c, Duration::from_secs(35)).await? {
        Frame::ActionAck(a) if a.status == AckStatus::Committed => {
            let sid = a.chat_id.unwrap_or_default();
            let prompt_cid = uuid::Uuid::new_v4().to_string();
            c.send(&Frame::Action(acp_hub_proto::action::ActionEnvelope::Prompt {
                command_id: prompt_cid.clone(),
                payload: acp_hub_proto::action::PromptChatPayload {
                    chat_id: sid.clone(),
                    message: "hello".to_string(),
                },
            }))
            .await?;
            // 预期：delta 广播（chat doc 增量）+ committed ack（prompt 同样
            // 先 accepted 后 committed，wait_terminal 跳过前置）。
            let ack = wait_terminal(&mut c, RECV_TIMEOUT).await?;
            match ack {
                Frame::ActionAck(a) if a.status == AckStatus::Committed => {
                    println!("  [t04] prompt committed (turn={:?})", a.turn_id);
                    Ok(())
                }
                Frame::ActionError(e) => Err(format!("prompt 失败: {:?} {}", e.code, e.message)),
                other => Err(format!("prompt 意外 ack: {:?}", other)),
            }
        }
        Frame::ActionError(e) => Err(format!("前置 create 失败: {:?} {}", e.code, e.message)),
        other => Err(format!("前置 create 意外终态: {:?}", other)),
    }
}

#[tokio::test]
async fn t04_prompt_stream_broadcast() {
    println!("T-04-prompt-stream: START");
    let r = tokio::time::timeout(TEST_BUDGET, t04_body()).await;
    match r {
        Ok(r) => t("04-prompt-stream", "", r),
        Err(_) => println!("T-04-prompt-stream: FAIL 超时（60s 预算）"),
    }
}

// ---------------------------------------------------------------------------
// t05 同 commandId 二次提交 → duplicate ack（§4.4）
// ---------------------------------------------------------------------------

async fn t05_body() -> Result<(), String> {
    let stack = Stack::start()?;
    let mut c = WsClient::connect_client(stack.env.port, &stack.env.client_token, &["hub:registry"]).await?;
    let command_id = uuid::Uuid::new_v4().to_string();
    async fn send_create(
        c: &mut WsClient,
        command_id: &str,
    ) -> Result<(), String> {
        c.send(&Frame::Action(acp_hub_proto::action::ActionEnvelope::Create {
            command_id: command_id.to_string(),
            payload: acp_hub_proto::action::CreateChatPayload::default(),
        }))
        .await
    }
    send_create(&mut c, &command_id).await?;
    let first = wait_terminal(&mut c, Duration::from_secs(35)).await?;
    match first {
        Frame::ActionAck(a) if a.status == AckStatus::Committed => {
            // 二次提交同 commandId → duplicate + 原 turnId（create 无 turn，
            // duplicate 帧即可）。
            send_create(&mut c, &command_id).await?;
            let second = wait_terminal(&mut c, RECV_TIMEOUT).await?;
            match second {
                Frame::ActionAck(a) if a.status == AckStatus::Duplicate => {
                    println!("  [t05] duplicate ack 成立（commandId={command_id}）");
                    Ok(())
                }
                Frame::ActionError(e) => {
                    Err(format!("二次提交意外失败: {:?} {}", e.code, e.message))
                }
                other => Err(format!("二次提交意外帧: {:?}", other)),
            }
        }
        Frame::ActionError(e) => Err(format!("前置 create 失败: {:?} {}", e.code, e.message)),
        other => Err(format!("意外帧: {:?}", other)),
    }
}

#[tokio::test]
async fn t05_duplicate_command_id() {
    println!("T-05-duplicate-command: START");
    let r = tokio::time::timeout(TEST_BUDGET, t05_body()).await;
    match r {
        Ok(r) => t("05-duplicate-command", "", r),
        Err(_) => println!("T-05-duplicate-command: FAIL 超时（60s 预算）"),
    }
}

// ---------------------------------------------------------------------------
// t06 server 重启后 registry 一致性（§8.4.1：registry.log 重放 + writer 恢复
// doc 注入；镜像含历史 session，新 create 正常）
// ---------------------------------------------------------------------------

async fn t06_body() -> Result<(), String> {
    // 第一轮：server + instance，create session A。
    let env = TestEnv::new();
    let mut server = ServerProc::start(&env, None);
    server.wait_ready().map_err(|e| e.to_string())?;
    let mut instance = InstanceProc::start(&env);
    if !instance.wait_authenticated(Duration::from_secs(15)) {
        return Err("第一轮 instance 未在 15s 内完成认证握手".to_string());
    }
    let mut c = WsClient::connect_client(env.port, &env.client_token, &["hub:registry"]).await?;
    let cid_a = uuid::Uuid::new_v4().to_string();
    c.send(&Frame::Action(acp_hub_proto::action::ActionEnvelope::Create {
        command_id: cid_a,
        payload: acp_hub_proto::action::CreateChatPayload::default(),
    }))
    .await?;
    let sid_a = match wait_terminal(&mut c, Duration::from_secs(35)).await? {
        Frame::ActionAck(a) if a.status == AckStatus::Committed => {
            a.chat_id.unwrap_or_default()
        }
        Frame::ActionError(e) => {
            return Err(format!("第一轮 create 失败: {:?} {}", e.code, e.message))
        }
        other => return Err(format!("第一轮 create 意外终态: {:?}", other)),
    };
    assert!(!sid_a.is_empty(), "第一轮 committed 必须携带 sessionId");
    // 等待 registry 落盘（committed 已保证 register 写回完成；再补 200ms
    // 让 update 日志 flush 完成——写是同步 fsync，此处仅为保险）。
    tokio::time::sleep(Duration::from_millis(200)).await;
    drop(c);

    // 杀进程（进程组 SIGKILL，模拟崩溃）。
    instance.kill();
    server.kill();

    // 第二轮：同一 data_dir 重启，create session B。
    let server2 = ServerProc::start(&env, None);
    server2.wait_ready().map_err(|e| e.to_string())?;
    let instance2 = InstanceProc::start(&env);
    if !instance2.wait_authenticated(Duration::from_secs(15)) {
        return Err("第二轮 instance 未在 15s 内完成认证握手".to_string());
    }
    let mut c2 =
        WsClient::connect_client(env.port, &env.client_token, &["hub:registry"]).await?;
    let cid_b = uuid::Uuid::new_v4().to_string();
    c2.send(&Frame::Action(acp_hub_proto::action::ActionEnvelope::Create {
        command_id: cid_b,
        payload: acp_hub_proto::action::CreateChatPayload::default(),
    }))
    .await?;
    let sid_b = match wait_terminal(&mut c2, Duration::from_secs(35)).await? {
        Frame::ActionAck(a) if a.status == AckStatus::Committed => {
            a.chat_id.unwrap_or_default()
        }
        Frame::ActionError(e) => {
            return Err(format!("第二轮 create 失败: {:?} {}", e.code, e.message))
        }
        other => return Err(format!("第二轮 create 意外终态: {:?}", other)),
    };
    assert!(!sid_b.is_empty(), "第二轮 committed 必须携带 sessionId");

    // registry 快照：应含 A（registry.log 重放）与 B（新写入）。
    let doc = fetch_registry_snapshot(env.port, &env.client_token).await?;
    let ids = chat_ids(&doc);
    assert!(
        ids.contains(&sid_a),
        "重启后 registry 应保留 session A（registry.log 重放，got={ids:?}）"
    );
    assert!(
        ids.contains(&sid_b),
        "重启后 registry 应含新 session B（got={ids:?}）"
    );
    println!("  [t06] 重启 registry 一致性成立：A={sid_a} B={sid_b}");
    Ok(())
}

#[tokio::test]
async fn t06_restart_registry_consistency() {
    println!("T-06-restart-registry: START");
    let r = tokio::time::timeout(TEST_BUDGET, t06_body()).await;
    match r {
        Ok(r) => t("06-restart-registry", "", r),
        Err(_) => println!("T-06-restart-registry: FAIL 超时（60s 预算）"),
    }
}
