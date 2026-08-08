//! HMAC 双向认证测试：§4.8 向量 12（字节级向量 + 重放/角色/版本拒绝 + 常量时间路径）。
//!
//! 主向量（`token`/`challenge`/`context`/`version`/`role` 固定输入 → 期望 MAC）
//! 由实现时一次性脚本按 `docs/plans/f1-proto.md` §10 公式计算后固化，跨实现
//! 可验证（架构 §9.2 顾问3：「测试向量必须以字节级定义」）。

use crate::hmac::{
    compute_mac, derive_mac_key, generate_challenge_nonce, generate_session_context,
    mac_input, verify_mac, HmacError, SeenNonces, CHALLENGE_NONCE_LEN, HMAC_OUTPUT_LEN,
    SESSION_CONTEXT_LEN,
};
use base64::Engine as _;

/// 测试专用固定 machine_token（32B）。
const TOKEN: [u8; 32] = *b"0123456789abcdef0123456789abcdef";
/// 固定 challenge_nonce。
const CHALLENGE: [u8; 32] = [0xAA; 32];
/// 固定 session_context。
const CONTEXT: [u8; 32] = [0xBB; 32];
const VERSION: &str = "1";
const ROLE: &str = "machine";

/// 向量 12 主向量：固定输入 → 期望密钥/输入字节/MAC（固化值）。
#[test]
fn hmac_byte_level_vector() {
    // HKDF 派生密钥（salt 空、info = b"acp-hub-auth" ‖ role，32B 输出）
    let key = derive_mac_key(&TOKEN, ROLE);
    assert_eq!(
        hex(&key),
        "5cb74e37d930d41ff8c7a199c715af5a5527790b9ee4f917a917d18837c1a9b9"
    );

    // MAC 输入规范化：字段顺序 challenge‖context‖version‖role，u16 BE 长度前缀
    let input = mac_input(&CHALLENGE, &CONTEXT, VERSION, ROLE);
    assert_eq!(input.len(), 80, "2+32 + 2+32 + 2+1 + 2+7");
    assert_eq!(
        hex(&input),
        concat!(
            "0020", "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "0020", "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "0001", "31",
            "0007", "6d616368696e65",
        )
    );

    // HMAC-SHA256 输出（base64，RFC 4648 标准字母表 + padding）
    let mac = compute_mac(&key, &input);
    assert_eq!(b64(&mac), "zpu6t1CWDyExXI74bT4/p2PFdudXGAxR2oaOAaW0zZA=");

    // 完整校验路径：Ok
    assert_eq!(
        verify_mac(&key, &input, "zpu6t1CWDyExXI74bT4/p2PFdudXGAxR2oaOAaW0zZA="),
        Ok(())
    );
}

/// mac_input 规范化单测：长度前缀 u16 大端 + 字段顺序不可重排（§9.2 顾问3）。
#[test]
fn mac_input_field_layout() {
    let c = [0x01; 32];
    let ctx = [0x02; 32];

    // 单字段结构：u16 BE 前缀 + 原始字节
    let input = mac_input(&c, &ctx, "1", "machine");
    assert_eq!(&input[0..2], &0x0020u16.to_be_bytes());
    assert_eq!(&input[2..34], &c);
    assert_eq!(&input[34..36], &0x0020u16.to_be_bytes());
    assert_eq!(&input[36..68], &ctx);
    assert_eq!(&input[68..70], &0x0001u16.to_be_bytes());
    assert_eq!(&input[70..71], b"1");
    assert_eq!(&input[71..73], &0x0007u16.to_be_bytes());
    assert_eq!(&input[73..80], b"machine");

    // 字段顺序即文档顺序（challenge 在前，role 在最后）：重排产物不同
    let reordered = mac_input(&ctx, &c, "1", "machine");
    assert_ne!(input, reordered);
    // version/role 用其 UTF-8 表示
    let v2 = mac_input(&c, &ctx, "2", "machine");
    assert_ne!(input, v2);
    let other_role = mac_input(&c, &ctx, "1", "client");
    assert_ne!(input, other_role);
}

/// 错误 base64 → InvalidBase64。
#[test]
fn verify_mac_invalid_base64() {
    let key = derive_mac_key(&TOKEN, ROLE);
    let input = mac_input(&CHALLENGE, &CONTEXT, VERSION, ROLE);
    assert_eq!(
        verify_mac(&key, &input, "!!!not-base64!!!"),
        Err(HmacError::InvalidBase64)
    );
}

/// 错误 MAC → Mismatch（常量时间比较的失败路径）。
#[test]
fn verify_mac_mismatch() {
    let key = derive_mac_key(&TOKEN, ROLE);
    let input = mac_input(&CHALLENGE, &CONTEXT, VERSION, ROLE);
    let wrong = b64(&[0x00; HMAC_OUTPUT_LEN]);
    assert_eq!(verify_mac(&key, &input, &wrong), Err(HmacError::Mismatch));
}

/// 长度防御：expected 解码后非 32B → BadLength（比较前防御，§10）。
#[test]
fn verify_mac_bad_length() {
    let key = derive_mac_key(&TOKEN, ROLE);
    let input = mac_input(&CHALLENGE, &CONTEXT, VERSION, ROLE);
    // 31B 与 33B 均拒绝
    assert_eq!(
        verify_mac(&key, &input, &b64(&[0u8; 31])),
        Err(HmacError::BadLength)
    );
    assert_eq!(
        verify_mac(&key, &input, &b64(&[0u8; 33])),
        Err(HmacError::BadLength)
    );
    // 空输入也走 base64 合法但长度拒绝
    assert_eq!(
        verify_mac(&key, &input, &b64(&[])),
        Err(HmacError::BadLength)
    );
}

/// 错误角色拒绝（§9.2 角色绑定）：用 `client` 角色派生的密钥校验 `machine`
/// 身份证明 → Mismatch。
#[test]
fn wrong_role_rejected() {
    let machine_key = derive_mac_key(&TOKEN, "machine");
    let input = mac_input(&CHALLENGE, &CONTEXT, VERSION, "machine");
    let mac = compute_mac(&machine_key, &input);

    let client_key = derive_mac_key(&TOKEN, "client");
    assert_ne!(client_key, machine_key, "角色必须进入派生上下文");
    assert_eq!(
        verify_mac(&client_key, &input, &b64(&mac)),
        Err(HmacError::Mismatch)
    );
}

/// 错误版本拒绝（§9.2 版本绑定）：`protocol_version` 不同 → 校验失败。
#[test]
fn wrong_version_rejected() {
    let key = derive_mac_key(&TOKEN, ROLE);
    let input_v1 = mac_input(&CHALLENGE, &CONTEXT, "1", ROLE);
    let mac_v1 = compute_mac(&key, &input_v1);

    // 校验方用 version=2 重放 v1 的 MAC → 失败
    let input_v2 = mac_input(&CHALLENGE, &CONTEXT, "2", ROLE);
    assert_eq!(
        verify_mac(&key, &input_v2, &b64(&mac_v1)),
        Err(HmacError::Mismatch)
    );
    // 校验方用 v1 校验 v2 的 MAC → 失败（双向）
    let mac_v2 = compute_mac(&key, &input_v2);
    assert_eq!(
        verify_mac(&key, &input_v1, &b64(&mac_v2)),
        Err(HmacError::Mismatch)
    );
}

/// 旧 challenge 重放拒绝（§9.2 挑战新鲜性）：nonce 单次使用，重放返回 false。
#[test]
fn replayed_challenge_rejected() {
    let mut seen = SeenNonces::new();
    let nonce = [0x42; 32];

    assert!(!seen.contains(&nonce));
    assert!(seen.check_and_mark(&nonce), "首次使用应放行");
    assert!(seen.contains(&nonce));
    assert_eq!(seen.len(), 1);

    // 重放同一 nonce → 拒绝
    assert!(!seen.check_and_mark(&nonce), "重放应拒绝");
    assert_eq!(seen.len(), 1, "重放不新增记录");

    // 不同 nonce 独立
    let other = [0x43; 32];
    assert!(seen.check_and_mark(&other));
    assert_eq!(seen.len(), 2);
}

/// 常量时间比较路径覆盖：`verify_slice` 的正确/失败两路径均已覆盖
/// （`verify_mac_byte_level_vector` 的 Ok 路径 + `verify_mac_mismatch` 的
/// Err 路径）；常量时间性质由 `hmac::Mac::verify_slice` 内建保证，
/// 无需新增 subtle 依赖（设计文档 §10）。
#[test]
fn constant_time_verify_paths() {
    let key = derive_mac_key(&TOKEN, ROLE);
    let input = mac_input(&CHALLENGE, &CONTEXT, VERSION, ROLE);
    let mac = compute_mac(&key, &input);

    // 通过路径
    assert!(verify_mac(&key, &input, &b64(&mac)).is_ok());
    // 失败路径（单比特翻转）
    let mut tampered = mac;
    tampered[0] ^= 0x01;
    assert_eq!(
        verify_mac(&key, &input, &b64(&tampered)),
        Err(HmacError::Mismatch)
    );
}

/// 生成器：32B 输出、CSPRNG（两次调用几乎必然不同）。
#[test]
fn generators_produce_distinct_32b() {
    let n1 = generate_challenge_nonce();
    let n2 = generate_challenge_nonce();
    let c1 = generate_session_context();
    assert_eq!(n1.len(), CHALLENGE_NONCE_LEN);
    assert_eq!(c1.len(), SESSION_CONTEXT_LEN);
    assert_ne!(n1, n2);
    assert_ne!(c1, generate_session_context());
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn b64(b: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(b)
}
