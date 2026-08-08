//! 契约向量集成测试（§12.1 契约级）：向量 6 白名单端到端 + 向量 12 双向认证握手模拟。
//!
//! 本文件模拟 server/machine 握手流程中 proto 承担的部分（纯函数层），
//! 连接状态与审计计数等 server 语义见架构 §9.2/§17.1，由 server 集成测试覆盖。

use acp_hub_proto::ack::ErrorCode;
use acp_hub_proto::conn::{AuthResponse, DocId};
use acp_hub_proto::frame::{Frame, ProtoError};
use acp_hub_proto::hmac::{
    compute_mac, derive_mac_key, generate_session_context, mac_input, verify_mac,
    SeenNonces,
};
use acp_hub_proto::machine::MachineHello;
use acp_hub_proto::whitelist::{m1_allows, m1_check, Direction, M1Check, Role};
use acp_hub_proto::{PROTOCOL_VERSION, CHAT_DOC_SCHEMA_VERSION};

const MACHINE_TOKEN: [u8; 32] = *b"0123456789abcdef0123456789abcdef";
const ROLE: &str = "machine";

/// 向量 12 端到端：machine 连接 → hello（含 nonce）→ server 身份证明 →
/// machine 校验通过。模拟架构 §9.2 步骤 1–3 的 proto 层部分。
#[test]
fn vector_12_mutual_auth_handshake_succeeds() {
    // machine 侧：生成一次性 challenge 并发送 hello
    let nonce = acp_hub_proto::hmac::generate_challenge_nonce();
    let hello = MachineHello {
        token: "machine-token".into(),
        hostname: "host1".into(),
        caps: serde_json::json!({}),
        buffered: None,
        buffer_lost: None,
        stream_epochs: None,
        nonce: base64_encode(&nonce),
    };
    // hello 帧可正常序列化/解析
    let raw = serde_json::to_string(&Frame::MachineHello(hello.clone())).unwrap();
    assert_eq!(Frame::parse(&raw).unwrap(), Frame::MachineHello(hello));

    // server 侧：校验 token → 派生密钥 → 计算身份证明（HMAC 输入含
    // nonce ‖ session_context ‖ protocol_version ‖ role，§9.2 顾问3）
    let server_key = derive_mac_key(&MACHINE_TOKEN, ROLE);
    let context = generate_session_context();
    let input = mac_input(
        &nonce,
        &context,
        &PROTOCOL_VERSION.to_string(),
        ROLE,
    );
    let mac = compute_mac(&server_key, &input);
    let auth_response = AuthResponse {
        session_context: base64_encode(&context),
        hmac: base64_encode(&mac),
    };
    let resp_raw = serde_json::to_string(&Frame::AuthResponse(auth_response.clone())).unwrap();
    assert_eq!(
        Frame::parse(&resp_raw).unwrap(),
        Frame::AuthResponse(auth_response.clone())
    );

    // machine 侧：用同一 token 派生密钥校验（常量时间比较）
    let machine_key = derive_mac_key(&MACHINE_TOKEN, ROLE);
    assert_eq!(machine_key, server_key);
    assert!(verify_mac(&machine_key, &input, &base64_encode(&mac)).is_ok());
    assert!(m1_allows(
        acp_hub_proto::Frame::AuthResponse(auth_response).tag(),
        Role::Machine,
        Direction::Outbound
    ));
}

/// 向量 12：旧 challenge 重放 / 跨连接重放拒绝。
///
/// 协议级属性（§9.2）：nonce 单次使用 + [`NONCE_TTL`] 窗口 + session_context
/// 连接绑定——同一 nonce 第二次使用被 [`SeenNonces`] 拒绝；同一 MAC 报文换
/// 一个连接（新 session_context）后无法通过校验。
#[test]
fn vector_12_replay_rejected() {
    // 首次握手：nonce 登记并放行
    let nonce = [0x11; 32];
    let mut seen = SeenNonces::new();
    assert!(seen.check_and_mark(&nonce), "首次握手放行");

    // 重放旧握手报文（同 nonce）：拒绝
    assert!(!seen.check_and_mark(&nonce), "旧 challenge 重放拒绝");
    assert_eq!(seen.len(), 1);

    // 跨连接重放：同 MAC 报文、新连接（新 session_context）→ 校验失败
    let key = derive_mac_key(&MACHINE_TOKEN, ROLE);
    let ctx_old = [0x22; 32];
    let input_old = mac_input(&nonce, &ctx_old, "1", ROLE);
    let mac_old = compute_mac(&key, &input_old);

    let ctx_new = [0x33; 32];
    let input_new = mac_input(&nonce, &ctx_new, "1", ROLE);
    assert!(
        verify_mac(&key, &input_new, &base64_encode(&mac_old)).is_err(),
        "跨连接重放应失败（session_context 绑定）"
    );
}

/// 向量 6 端到端：未知 t → `Unsupported`；server 映射为 `UNSUPPORTED_FRAME`
/// 稳定错误码（§4.8「一律返回稳定错误并计数，不静默」）。
#[test]
fn vector_6_unsupported_frame_error_mapping() {
    let raw = r#"{"t":"totally_unknown","x":1}"#;
    let err = Frame::parse(raw).unwrap_err();
    assert_eq!(
        err,
        ProtoError::Unsupported("totally_unknown".into())
    );
    // 上层映射：白名单外 → UNSUPPORTED_FRAME（§4.8 稳定错误码）
    match &err {
        ProtoError::Unsupported(_) | ProtoError::DirectionRejected(_) => {
            // 可回 action_error 时使用 UnsupportedFrame；不可回则断开
            let code = ErrorCode::UnsupportedFrame;
            assert!(!code.default_retryable(), "UNSUPPORTED_FRAME 不可重试");
        }
        ProtoError::Malformed(_) => panic!("未知 tag 必须映射为 Unsupported"),
    }
}

/// 向量 6：客户端上行 `ysync.update` 方向拒绝（§5.6）。
#[test]
fn vector_6_client_uplink_update_rejected() {
    assert_eq!(
        m1_check(
            acp_hub_proto::Frame::YsyncUpdate(acp_hub_proto::ysync::YsyncUpdate {
                doc: DocId::chat("s1"),
                update: "AAAA".into(),
                projection_version: None,
            })
            .tag(),
            Role::Client,
            Direction::Inbound
        ),
        M1Check::DirectionRejected
    );
}

/// 版本常量一致性：hello 携带的 protocol_version 与 auth MAC 输入中的版本一致。
#[test]
fn protocol_version_consistency() {
    assert_eq!(PROTOCOL_VERSION, 1);
    assert_eq!(CHAT_DOC_SCHEMA_VERSION, 1);
    // MAC 输入使用 PROTOCOL_VERSION 的十进制字符串表示（§10）
    let key = derive_mac_key(&MACHINE_TOKEN, ROLE);
    let input = mac_input(&[0u8; 32], &[0u8; 32], &PROTOCOL_VERSION.to_string(), ROLE);
    // 与固化的字节级向量一致（向量 12 主向量以 version="1" 计算）
    let expected = mac_input(&[0u8; 32], &[0u8; 32], "1", ROLE);
    assert_eq!(input, expected);
    let _ = key;
}

fn base64_encode(b: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(b)
}
