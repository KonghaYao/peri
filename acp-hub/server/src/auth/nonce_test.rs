//! NonceRegistry 单测（`docs/plans/f2-auth-config.md` §6.3 N1–N4）。
//!
//! 全部用例注入 `now: Instant`，无真实 sleep（N4）。

use std::time::{Duration, Instant};

use acp_hub_proto::hmac::{generate_challenge_nonce, NONCE_TTL};

use crate::auth::{NonceRegistry, NonceVerdict};

/// N1：单次使用——check_and_mark 两次 → Accepted, Replay。
#[test]
fn n1_single_use() {
    let mut reg = NonceRegistry::new();
    let nonce = generate_challenge_nonce();
    let t0 = Instant::now();
    assert_eq!(reg.check_and_mark(&nonce, t0), NonceVerdict::Accepted);
    assert_eq!(reg.check_and_mark(&nonce, t0), NonceVerdict::Replay);
    assert_eq!(reg.len(), 1);
}

/// N2：TTL——窗口内 Accepted；`now + 30s` 后同 nonce → Accepted（新窗口语义，
/// §4.4）。
#[test]
fn n2_ttl_window() {
    let mut reg = NonceRegistry::new();
    let nonce = generate_challenge_nonce();
    let t0 = Instant::now();
    assert_eq!(reg.check_and_mark(&nonce, t0), NonceVerdict::Accepted);

    // 窗口内重提 → Replay
    let t_in = t0 + NONCE_TTL - Duration::from_millis(1);
    assert_eq!(reg.check_and_mark(&nonce, t_in), NonceVerdict::Replay);

    // 越过 30s → 视为新 nonce（Accepted，重新登记）
    let t_expired = t0 + NONCE_TTL + Duration::from_millis(1);
    assert_eq!(
        reg.check_and_mark(&nonce, t_expired),
        NonceVerdict::Accepted,
        "过期后同 nonce 按新 nonce 处理"
    );
    // 新窗口内再提 → Replay（重新登记的窗口生效）
    let t_in2 = t_expired + Duration::from_millis(100);
    assert_eq!(reg.check_and_mark(&nonce, t_in2), NonceVerdict::Replay);
}

/// N3：sweep——过期条目清除后 `len` 归零；sweep 幂等。
#[test]
fn n3_sweep() {
    let mut reg = NonceRegistry::new();
    let t0 = Instant::now();
    let n1 = generate_challenge_nonce();
    let n2 = generate_challenge_nonce();
    assert_eq!(reg.check_and_mark(&n1, t0), NonceVerdict::Accepted);
    assert_eq!(reg.check_and_mark(&n2, t0), NonceVerdict::Accepted);
    assert_eq!(reg.len(), 2);

    // 窗口内 sweep：无清除
    reg.sweep(t0 + NONCE_TTL - Duration::from_secs(1));
    assert_eq!(reg.len(), 2);

    // 全部过期后 sweep：归零
    reg.sweep(t0 + NONCE_TTL + Duration::from_secs(1));
    assert_eq!(reg.len(), 0);
    assert!(reg.is_empty());

    // sweep 幂等
    reg.sweep(t0 + NONCE_TTL + Duration::from_secs(60));
    assert_eq!(reg.len(), 0);
}

/// N4：时钟注入——全部用例经参数传入 `now`，无真实 sleep。
#[test]
fn n4_clock_injection() {
    let mut reg = NonceRegistry::new();
    let nonce = generate_challenge_nonce();
    let t0 = Instant::now();

    // 旧 nonce 过期后，新 nonce 不受影响
    assert_eq!(reg.check_and_mark(&nonce, t0), NonceVerdict::Accepted);
    let other = generate_challenge_nonce();
    let t_far = t0 + NONCE_TTL + NONCE_TTL;
    assert_eq!(reg.check_and_mark(&other, t_far), NonceVerdict::Accepted);

    // 不同 nonce 互不干扰
    assert_eq!(reg.check_and_mark(&other, t_far), NonceVerdict::Replay);
    assert_eq!(reg.len(), 2);
}
