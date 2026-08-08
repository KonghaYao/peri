//! hub 集成测试：fake ws server 驱动完整链路（T2 心跳 / T6 seq 单调 / T7 spawn
//! 幂等 / T9 process_exit / T10 buffer_sync 补推 / T11 认证前拒绝 / T13 epoch）。

use super::*;
use std::time::Duration;

use acp_hub_proto::hmac::{
    compute_mac, derive_mac_key, mac_input, CHALLENGE_NONCE_LEN,
};
use acp_hub_proto::machine::MachineHello;
use base64::Engine as _;
use futures::{SinkExt, StreamExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;

const TOKEN: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";

fn token_bytes() -> [u8; CHALLENGE_NONCE_LEN] {
    base64::engine::general_purpose::STANDARD
        .decode(TOKEN)
        .unwrap()
        .try_into()
        .unwrap()
}

/// 以 server 侧逻辑构造合法 auth_response。
fn valid_auth_response(hello: &MachineHello) -> acp_hub_proto::conn::AuthResponse {
    let nonce: [u8; CHALLENGE_NONCE_LEN] = base64::engine::general_purpose::STANDARD
        .decode(&hello.nonce)
        .unwrap()
        .try_into()
        .unwrap();
    let context = acp_hub_proto::hmac::generate_session_context();
    let key = derive_mac_key(&token_bytes(), "machine");
    let input = mac_input(
        &nonce,
        &context,
        &acp_hub_proto::version::PROTOCOL_VERSION.to_string(),
        "machine",
    );
    let mac = compute_mac(&key, &input);
    acp_hub_proto::conn::AuthResponse {
        session_context: base64::engine::general_purpose::STANDARD.encode(context),
        hmac: base64::engine::general_purpose::STANDARD.encode(mac),
    }
}

/// 测试配置（短超时/短间隔加速）。
fn test_config(addr: std::net::SocketAddr, data_dir: &Path) -> MachineConfig {
    let mut c = MachineConfig::new(format!("ws://{addr}/machine"), TOKEN.to_string(), data_dir.to_path_buf());
    c.heartbeat_interval = Duration::from_millis(300);
    c.reconnect_base = Duration::from_millis(200);
    c.reconnect_max = Duration::from_secs(1);
    c.auth_timeout = Duration::from_secs(2);
    c.kill_grace = Duration::from_millis(300);
    c
}

fn spawn_frame(command_id: &str, session_id: &str, script: &str) -> Frame {
    Frame::MachineSpawn(MachineSpawn {
        command_id: command_id.to_string(),
        session_id: session_id.to_string(),
        cmd: vec!["sh".to_string(), "-c".to_string(), script.to_string()],
        cwd: ".".to_string(),
        env: None,
    })
}

fn kill_frame(command_id: &str, session_id: &str, grace_ms: Option<u64>) -> Frame {
    Frame::MachineKill(MachineKill {
        command_id: command_id.to_string(),
        session_id: session_id.to_string(),
        grace: grace_ms,
    })
}

type Ws = WebSocketStream<TcpStream>;
type SplitSink = futures::stream::SplitSink<Ws, Message>;
type SplitStream = futures::stream::SplitStream<Ws>;

async fn accept_ws(listener: &TcpListener) -> Ws {
    let (stream, _) = listener.accept().await.unwrap();
    tokio_tungstenite::accept_async(stream).await.unwrap()
}

/// 服务端握手：读 hello → 回合法 auth_response → 返回 (sink, stream, hello)。
async fn handshake_server(ws: Ws) -> (SplitSink, SplitStream, MachineHello) {
    let mut ws = ws;
    let hello = loop {
        match ws.next().await {
            Some(Ok(Message::Text(t))) => {
                if let Ok(Frame::MachineHello(h)) = Frame::parse(&t) {
                    break h;
                }
            }
            other => panic!("等待 hello 失败: {other:?}"),
        }
    };
    let resp = valid_auth_response(&hello);
    ws.send(Message::Text(
        serde_json::to_string(&Frame::AuthResponse(resp)).unwrap().into(),
    ))
    .await
    .unwrap();
    let (sink, stream) = ws.split();
    (sink, stream, hello)
}

async fn send_frame(sink: &mut SplitSink, frame: &Frame) {
    sink.send(Message::Text(
        serde_json::to_string(frame).unwrap().into(),
    ))
    .await
    .unwrap();
}

/// 读取下一帧（忽略未知/畸形帧；流结束 → panic）。
async fn next_frame(stream: &mut SplitStream) -> Frame {
    loop {
        match stream.next().await {
            Some(Ok(Message::Text(t))) => {
                if let Ok(f) = Frame::parse(&t) {
                    return f;
                }
            }
            other => panic!("ws 流意外结束: {other:?}"),
        }
    }
}

/// 读取下一帧（跳过 heartbeat——心跳是周期帧，与断言目标无关）。
async fn next_frame_skipping_hb(stream: &mut SplitStream) -> Frame {
    loop {
        match next_frame(stream).await {
            Frame::MachineHeartbeat(_) => continue,
            f => return f,
        }
    }
}

/// 读取下一帧（限时；超时返回 None）。
async fn next_frame_timeout(stream: &mut SplitStream, d: Duration) -> Option<Frame> {
    tokio::time::timeout(d, next_frame(stream)).await.ok()
}

// ---------------------------------------------------------------------------
// 全链路：hello → auth → spawn（幂等）→ event（seq 单调）→ heartbeat → kill
// → process_exit（T2/T6/T7/T9）
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_full_flow_spawn_event_heartbeat_kill_exit() {
    let dir = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let config = test_config(addr, dir.path());

    let hub = tokio::spawn(run(config));
    let (mut sink, mut stream, hello) = handshake_server(accept_ws(&listener).await).await;
    assert_eq!(hello.buffered, Some(false), "无缓冲时 hello.buffered=false");

    // spawn：sh 输出 3 帧（带 sessionId）后挂起。
    let script = r#"echo '{"jsonrpc":"2.0","method":"m","params":{"sessionId":"s1","n":1}}'; echo '{"jsonrpc":"2.0","method":"m","params":{"sessionId":"s1","n":2}}'; echo '{"jsonrpc":"2.0","method":"m","params":{"sessionId":"s1","n":3}}'; sleep 30"#;
    send_frame(&mut sink, &spawn_frame("c1", "s1", script)).await;

    match next_frame_skipping_hb(&mut stream).await {
        Frame::MachineSpawnAck(a) => {
            assert!(a.ok, "spawn 应成功");
            assert_eq!(a.session_id, "s1");
            assert_eq!(a.command_id, "c1");
        }
        other => panic!("期待 spawn_ack，收到 {other:?}"),
    }

    // 3 帧 machine/event：seq 单调 1..3，epoch=1（T6）。
    for i in 1..=3u64 {
        match next_frame_skipping_hb(&mut stream).await {
            Frame::MachineEvent(e) => {
                assert_eq!(e.session_id, "s1");
                assert_eq!(e.epoch, 1);
                assert_eq!(e.seq, i, "seq 必须单调递增");
            }
            other => panic!("期待 machine/event，收到 {other:?}"),
        }
    }

    // spawn 幂等（T7）：同 session 再次 spawn → ack ok，且不二次起进程
    // （无额外 event 帧——若有新进程会立即输出 3 帧）。
    send_frame(&mut sink, &spawn_frame("c2", "s1", script)).await;
    match next_frame_skipping_hb(&mut stream).await {
        Frame::MachineSpawnAck(a) => {
            assert!(a.ok, "幂等 spawn 必须 ack ok");
            assert_eq!(a.command_id, "c2");
        }
        other => panic!("期待幂等 spawn_ack，收到 {other:?}"),
    }
    // 若幂等失败（二次起进程），此处会收到 event 帧 → panic。
    match next_frame_timeout(&mut stream, Duration::from_millis(400)).await {
        Some(Frame::MachineHeartbeat(_)) => {}
        Some(other) => panic!("幂等 spawn 后不应有事件帧，收到 {other:?}"),
        None => panic!("幂等 spawn 后应收到 heartbeat"),
    }

    // heartbeat（T2）：alive_sessions 含 s1。
    match next_frame(&mut stream).await {
        Frame::MachineHeartbeat(h) => {
            assert!(h.alive_sessions.contains(&"s1".to_string()), "alive 应含 s1");
            assert_eq!(h.load, 20, "load = min(100, alive×20)");
        }
        other => panic!("期待 heartbeat，收到 {other:?}"),
    }

    // kill → kill_ack → process_exit（T9）。
    send_frame(&mut sink, &kill_frame("c3", "s1", Some(200))).await;
    match next_frame_skipping_hb(&mut stream).await {
        Frame::MachineKillAck(a) => {
            assert!(a.ok);
            assert_eq!(a.session_id, "s1");
        }
        other => panic!("期待 kill_ack，收到 {other:?}"),
    }
    match next_frame_skipping_hb(&mut stream).await {
        Frame::MachineProcessExit(e) => {
            assert_eq!(e.session_id, "s1");
        }
        other => panic!("期待 process_exit，收到 {other:?}"),
    }

    drop(stream);
    drop(sink);
    hub.abort();
    let _ = hub.await;
}

// ---------------------------------------------------------------------------
// T11：认证通过前收到 spawn → 不执行（握手阶段业务帧被 transport 视为协议
// 违反而断开重连；spawn 从未执行/从未 ack）
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_spawn_before_auth_is_dropped() {
    let dir = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let config = test_config(addr, dir.path());

    let hub = tokio::spawn(run(config));

    // 连接 1：读 hello，但**不回 auth_response**，直接发 spawn（模拟未经
    // 认证的指令注入）。
    let mut ws = accept_ws(&listener).await;
    loop {
        match ws.next().await {
            Some(Ok(Message::Text(t))) => {
                if let Ok(Frame::MachineHello(_)) = Frame::parse(&t) {
                    break;
                }
            }
            other => panic!("等待 hello 失败: {other:?}"),
        }
    }
    ws.send(Message::Text(
        serde_json::to_string(&spawn_frame("c1", "s1", "echo x; sleep 30")).unwrap().into(),
    ))
    .await
    .unwrap();

    // 认证通过前 spawn 被丢弃：不 ack、不执行（§9.2 步骤 3）——machine 视
    // 握手阶段业务帧为协议违反，断开重连，且从未下发 spawn_ack。
    let got = tokio::time::timeout(Duration::from_millis(600), async {
        let mut frames = Vec::new();
        while let Some(msg) = ws.next().await {
            if let Ok(Message::Text(t)) = msg {
                if let Ok(f) = Frame::parse(&t) {
                    frames.push(f);
                }
            }
        }
        frames
    })
    .await;
    let frames = got.unwrap_or_default();
    assert!(
        frames.is_empty(),
        "认证通过前 spawn 必须被丢弃（无 ack/无任何帧），收到 {frames:?}"
    );

    // 连接 2（重连后）：正常握手 → 认证通过 → daemon 正常运行（heartbeat），
    // 认证后 spawn 正常工作。
    let (mut sink2, mut s2, _hello2) = handshake_server(accept_ws(&listener).await).await;
    // 认证通过后 spawn 正常执行（此前注入的 spawn 未执行/未缓冲）。
    send_frame(&mut sink2, &spawn_frame("c2", "s2", "echo x; sleep 30")).await;
    match next_frame_skipping_hb(&mut s2).await {
        Frame::MachineSpawnAck(a) => {
            assert!(a.ok);
            assert_eq!(a.command_id, "c2");
            assert_eq!(a.session_id, "s2");
        }
        other => panic!("认证后 spawn 应正常 ack，收到 {other:?}"),
    }
    // 清理。
    send_frame(&mut sink2, &kill_frame("k1", "s2", Some(100))).await;
    match next_frame_skipping_hb(&mut s2).await {
        Frame::MachineKillAck(a) => assert!(a.ok),
        other => panic!("期待 kill_ack，收到 {other:?}"),
    }
    match next_frame_skipping_hb(&mut s2).await {
        Frame::MachineProcessExit(_) => {}
        other => panic!("期待 process_exit，收到 {other:?}"),
    }

    drop(sink2);
    drop(s2);
    hub.abort();
    let _ = hub.await;
}

// ---------------------------------------------------------------------------
// T10：断线缓冲 → 重连 → buffer_sync 补推（from_seq=1、seq 升序）→ 转实时
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_buffer_sync_resync() {
    let dir = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let mut config = test_config(addr, dir.path());
    // 退避 500ms：保证断线期帧（脚本 0.2s 后输出）先入缓冲，重连 hello 才能
    // 上报 buffered=true。
    config.reconnect_base = Duration::from_millis(500);

    let hub = tokio::spawn(run(config));

    // --- 连接 1：auth → spawn（脚本：0.2s 后输出 3 帧，再 1.5s 后输出第 4 帧）→ 断线 ---
    let script = r#"sleep 0.2; echo '{"jsonrpc":"2.0","method":"m","params":{"sessionId":"s1","n":1}}'; echo '{"jsonrpc":"2.0","method":"m","params":{"sessionId":"s1","n":2}}'; echo '{"jsonrpc":"2.0","method":"m","params":{"sessionId":"s1","n":3}}'; sleep 1.5; echo '{"jsonrpc":"2.0","method":"m","params":{"sessionId":"s1","n":4}}'"#;
    let (mut sink1, mut s1, _hello1) = handshake_server(accept_ws(&listener).await).await;
    send_frame(&mut sink1, &spawn_frame("c1", "s1", script)).await;
    match next_frame_skipping_hb(&mut s1).await {
        Frame::MachineSpawnAck(a) => assert!(a.ok),
        other => panic!("期待 spawn_ack，收到 {other:?}"),
    }
    drop(sink1);
    drop(s1); // 断线 → machine 进入缓冲模式

    // --- 连接 2：hello 携带缓冲水位 → auth → buffer_sync 补推 → 实时 event ---
    let (sink2, mut s2, hello2) = handshake_server(accept_ws(&listener).await).await;
    assert_eq!(hello2.buffered, Some(true), "有缓冲时必须上报 buffered=true");
    let epochs = hello2.stream_epochs.as_ref().unwrap();
    assert_eq!(epochs.get("s1"), Some(&1), "stream_epochs 应含存活 session 的 epoch");

    // buffer_sync：from_seq=1（last_sent_seq+1）、frames seq 升序 1..3、epoch=1。
    let sync = loop {
        match next_frame(&mut s2).await {
            Frame::MachineBufferSync(b) => break b,
            Frame::MachineHeartbeat(_) => continue,
            other => panic!("期待 buffer_sync，收到 {other:?}"),
        }
    };
    assert_eq!(sync.session_id, "s1");
    assert_eq!(sync.epoch, 1);
    assert_eq!(sync.from_seq, 1, "from_seq = last_sent_seq+1 = 1");
    let seqs: Vec<u64> = sync.frames.iter().map(|f| f.seq).collect();
    assert_eq!(seqs, vec![1, 2, 3], "补推帧按 seq 升序连续");

    // 补推完成后转实时：第 4 帧以 machine/event 到达（seq=4，同一序列）。
    let evt = loop {
        match next_frame(&mut s2).await {
            Frame::MachineEvent(e) if e.seq == 4 => break e,
            Frame::MachineHeartbeat(_) | Frame::MachineBufferSync(_) => continue,
            other => panic!("期待实时 event(seq=4)，收到 {other:?}"),
        }
    };
    assert_eq!(evt.epoch, 1);
    assert_eq!(evt.session_id, "s1");

    // 脚本自然退出 → process_exit（在线路径）。
    match next_frame(&mut s2).await {
        Frame::MachineProcessExit(e) => assert_eq!(e.session_id, "s1"),
        Frame::MachineHeartbeat(_) => {
            match next_frame(&mut s2).await {
                Frame::MachineProcessExit(e) => assert_eq!(e.session_id, "s1"),
                other => panic!("期待 process_exit，收到 {other:?}"),
            }
        }
        other => panic!("期待 process_exit，收到 {other:?}"),
    }

    drop(sink2);
    drop(s2);
    hub.abort();
    let _ = hub.await;
}

// ---------------------------------------------------------------------------
// T6/T13：进程重建 → epoch+1、seq 重置 1；重连 hello.stream_epochs 反映新纪元
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_epoch_increment_on_rebuild() {
    let dir = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let config = test_config(addr, dir.path());

    let hub = tokio::spawn(run(config));
    let script = r#"echo '{"jsonrpc":"2.0","method":"m","params":{"sessionId":"s1","n":1}}'; sleep 30"#;

    // 连接 1：spawn（epoch=1）→ event(seq=1) → kill → process_exit → 重建 spawn
    // （epoch=2）→ event(seq=1, epoch=2)。
    let (mut sink1, mut s1, _h1) = handshake_server(accept_ws(&listener).await).await;
    send_frame(&mut sink1, &spawn_frame("c1", "s1", script)).await;
    match next_frame_skipping_hb(&mut s1).await {
        Frame::MachineSpawnAck(a) => assert!(a.ok),
        other => panic!("期待 spawn_ack，收到 {other:?}"),
    }
    match next_frame_skipping_hb(&mut s1).await {
        Frame::MachineEvent(e) => {
            assert_eq!((e.epoch, e.seq), (1, 1), "新 session：epoch=1、首帧 seq=1");
        }
        other => panic!("期待 event，收到 {other:?}"),
    }

    send_frame(&mut sink1, &kill_frame("c2", "s1", Some(200))).await;
    match next_frame_skipping_hb(&mut s1).await {
        Frame::MachineKillAck(a) => assert!(a.ok),
        other => panic!("期待 kill_ack，收到 {other:?}"),
    }
    match next_frame_skipping_hb(&mut s1).await {
        Frame::MachineProcessExit(_) => {}
        other => panic!("期待 process_exit，收到 {other:?}"),
    }

    // 重建（同 session_id 再次 spawn）：epoch = 水位 + 1 = 2，seq 重置 1（§4.5.1）。
    send_frame(&mut sink1, &spawn_frame("c3", "s1", script)).await;
    match next_frame_skipping_hb(&mut s1).await {
        Frame::MachineSpawnAck(a) => assert!(a.ok),
        other => panic!("期待重建 spawn_ack，收到 {other:?}"),
    }
    match next_frame_skipping_hb(&mut s1).await {
        Frame::MachineEvent(e) => {
            assert_eq!((e.epoch, e.seq), (2, 1), "重建：epoch+1、seq 重置 1");
        }
        other => panic!("期待重建 event，收到 {other:?}"),
    }
    drop(sink1);
    drop(s1);

    // 连接 2：hello.stream_epochs 反映新纪元 {s1: 2}。
    let (_sink2, _s2, hello2) = handshake_server(accept_ws(&listener).await).await;
    let epochs = hello2.stream_epochs.as_ref().unwrap();
    assert_eq!(epochs.get("s1"), Some(&2), "重建后 stream_epochs 必须为 2");

    // 水位持久化：epoch=2 已落盘（T13）。
    let wm = Watermark::load(dir.path()).unwrap();
    assert_eq!(wm.epoch_of("s1"), Some(2));

    drop(_sink2);
    drop(_s2);
    hub.abort();
    let _ = hub.await;
}

// ---------------------------------------------------------------------------
// T3 集成级：双 session 分桶互不串扰（各自 seq/epoch）
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_two_sessions_isolated_seqs() {
    let dir = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let config = test_config(addr, dir.path());

    let hub = tokio::spawn(run(config));
    let (mut sink, mut stream, _hello) = handshake_server(accept_ws(&listener).await).await;

    let script = r#"echo '{"jsonrpc":"2.0","method":"m","params":{"sessionId":"s1","n":1}}'; echo '{"jsonrpc":"2.0","method":"m","params":{"sessionId":"s1","n":2}}'; sleep 30"#;
    let script2 = r#"echo '{"jsonrpc":"2.0","method":"m","params":{"sessionId":"s2","n":1}}'; sleep 30"#;
    send_frame(&mut sink, &spawn_frame("c1", "s1", script)).await;
    send_frame(&mut sink, &spawn_frame("c2", "s2", script2)).await;

    // 两个 spawn_ack。
    for _ in 0..2 {
        match next_frame_skipping_hb(&mut stream).await {
            Frame::MachineSpawnAck(a) => assert!(a.ok),
            other => panic!("期待 spawn_ack，收到 {other:?}"),
        }
    }

    // 各 session 独立 seq：s1 有 1,2；s2 有 1（互不串扰）。
    let mut s1_seqs = Vec::new();
    let mut s2_seqs = Vec::new();
    for _ in 0..4 {
        match next_frame_timeout(&mut stream, Duration::from_millis(1500)).await {
            Some(Frame::MachineEvent(e)) => {
                if e.session_id == "s1" {
                    s1_seqs.push(e.seq);
                } else if e.session_id == "s2" {
                    s2_seqs.push(e.seq);
                } else {
                    panic!("未知 session {}", e.session_id);
                }
            }
            Some(Frame::MachineHeartbeat(_)) => continue,
            other => {
                if s1_seqs.len() == 2 && s2_seqs.len() == 1 {
                    break;
                }
                panic!("等待事件失败: {other:?}");
            }
        }
    }
    assert_eq!(s1_seqs, vec![1, 2]);
    assert_eq!(s2_seqs, vec![1]);

    // 清理：2×kill_ack + 2×process_exit（顺序不定——kill_ack 由 hub 直发、
    // process_exit 由子进程 wait 完成触发，两者竞态）。
    send_frame(&mut sink, &kill_frame("k1", "s1", Some(100))).await;
    send_frame(&mut sink, &kill_frame("k2", "s2", Some(100))).await;
    let mut acks = 0;
    let mut exits = 0;
    for _ in 0..4 {
        match next_frame_skipping_hb(&mut stream).await {
            Frame::MachineKillAck(a) => {
                assert!(a.ok);
                acks += 1;
            }
            Frame::MachineProcessExit(_) => exits += 1,
            other => panic!("期待 kill_ack/process_exit，收到 {other:?}"),
        }
    }
    assert_eq!(acks, 2);
    assert_eq!(exits, 2);

    drop(stream);
    drop(sink);
    hub.abort();
    let _ = hub.await;
}
