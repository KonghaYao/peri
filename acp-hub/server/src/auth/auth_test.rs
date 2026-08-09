//! auth 模块测试（`docs/plans/f2-auth-config.md` §6.2 T1–T8 / §6.4 H1–H10）。
//!
//! 握手流程不依赖 ws——直接调 AuthService + 用 proto 原语独立重算验证。
//! tracing 事件捕获用 `tracing::subscriber::with_default`（不污染全局）。

use std::io::Write;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use base64::Engine as _;
use futures::executor::block_on;
use serde_json::json;
use tempfile::tempdir;
use tracing_subscriber::fmt::MakeWriter;

use acp_hub_proto::conn::Auth;
use acp_hub_proto::hmac::{
    compute_mac, derive_mac_key, generate_challenge_nonce, mac_input, verify_mac, NONCE_TTL,
};
use acp_hub_proto::instance::InstanceHello;
use acp_hub_proto::version::PROTOCOL_VERSION;
use acp_hub_proto::whitelist::Role;

use crate::auth::{
    audit::audit, AuthError, AuthService, ConnectionCtx, TokenRole, TokenStore, UNKNOWN_TOKEN_ID,
};

// ---------------------------------------------------------------------------
// 测试工具
// ---------------------------------------------------------------------------

fn peer() -> SocketAddr {
    "127.0.0.1:40000".parse().unwrap()
}

fn make_hello(token: &str, nonce_b64: &str) -> InstanceHello {
    InstanceHello {
        token: token.to_string(),
        hostname: "test-host".to_string(),
        caps: json!({}),
        buffered: None,
        buffer_lost: None,
        stream_epochs: None,
        nonce: nonce_b64.to_string(),
    }
}

fn new_nonce_b64() -> String {
    base64::engine::general_purpose::STANDARD.encode(generate_challenge_nonce())
}

fn new_store(dir: &Path) -> TokenStore {
    TokenStore::load(&dir.join("tokens.toml")).unwrap()
}

/// 捕获 tracing 事件的 writer（json 到内存，供脱敏/字段集合断言）。
#[derive(Clone)]
struct CaptureWriter(Arc<Mutex<Vec<u8>>>);

impl Write for CaptureWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for CaptureWriter {
    type Writer = CaptureWriter;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// 审计字段白名单（§9.4 / T8；§4.8 失败路径另携带 auth_failed_total 快照）。
const AUDIT_FIELDS: &[&str] = &[
    "action",
    "command_id",
    "token_id",
    "result",
    "duration_ms",
    "auth_failed_total",
];

/// 在捕获 subscriber 下执行闭包，返回 (闭包结果, 捕获的日志文本)。
fn with_capture<T>(f: impl FnOnce() -> T) -> (T, String) {
    let buf = Arc::new(Mutex::new(Vec::new()));
    let sub = tracing_subscriber::fmt()
        .json()
        .with_writer(CaptureWriter(buf.clone()))
        .with_target(true)
        .finish();
    let result = tracing::subscriber::with_default(sub, f);
    let text = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
    (result, text)
}

/// 断言捕获日志：每行 JSON 的 fields 键 ⊆ 白名单，且不含 token 本体。
fn assert_audit_redacted(text: &str, tokens: &[&str]) {
    assert!(!text.is_empty(), "应产生审计事件");
    for line in text.lines() {
        let v: serde_json::Value =
            serde_json::from_str(line).unwrap_or_else(|e| panic!("审计行非 JSON: {e}: {line}"));
        if let Some(fields) = v.get("fields").and_then(|f| f.as_object()) {
            for k in fields.keys() {
                assert!(
                    AUDIT_FIELDS.contains(&k.as_str()),
                    "审计事件含白名单外字段 {k}: {line}"
                );
            }
        }
        for t in tokens {
            assert!(
                !line.contains(t),
                "审计日志泄露 token 材料: {line}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// T1 生成
// ---------------------------------------------------------------------------
#[test]
fn t1_generate() {
    let dir = tempdir().unwrap();
    let mut store = new_store(dir.path());
    assert!(store.is_empty());

    let rec = store.generate(TokenRole::Instance, "desktop-01").unwrap();
    assert_eq!(rec.token.len(), 44, "32B→base64 应为 44 字符");
    assert!(!rec.revoked);
    assert!(!rec.id.is_empty());
    assert_eq!(rec.role, TokenRole::Instance);
    assert_eq!(rec.name, "desktop-01");

    let rec2 = store.generate(TokenRole::Full, "tui").unwrap();
    assert_ne!(rec.token, rec2.token, "两次生成必须不同");
    assert_eq!(store.len(), 2);

    // 落盘 → 重载一致
    let reloaded = TokenStore::load(&dir.path().join("tokens.toml")).unwrap();
    assert_eq!(reloaded.len(), 2);
    let reloaded_rec = reloaded.list().into_iter().find(|i| i.id == rec.id).unwrap();
    assert_eq!(reloaded_rec.role, TokenRole::Instance);
    assert_eq!(reloaded_rec.name, "desktop-01");
}

// ---------------------------------------------------------------------------
// T2 校验
// ---------------------------------------------------------------------------
#[test]
fn t2_validate() {
    let dir = tempdir().unwrap();
    let mut store = new_store(dir.path());
    let rec = store.generate(TokenRole::Instance, "m1").unwrap();

    // 正确 token 通过（返回记录）
    let got = store.validate(&rec.token, TokenRole::Instance).unwrap();
    assert_eq!(got.id, rec.id);

    // 未知 → UnknownToken
    assert!(matches!(
        store.validate("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=", TokenRole::Instance),
        Err(AuthError::UnknownToken)
    ));

    // 吊销后 → RevokedToken
    store.revoke(&rec.id).unwrap();
    assert!(matches!(
        store.validate(&rec.token, TokenRole::Instance),
        Err(AuthError::RevokedToken { token_id }) if token_id == rec.id
    ));
}

// ---------------------------------------------------------------------------
// T3 宽限期轮换：新旧并存 → 逐机切换 → 吊销旧
// ---------------------------------------------------------------------------
#[test]
fn t3_grace_period_rotation() {
    let dir = tempdir().unwrap();
    let mut store = new_store(dir.path());
    let old = store.generate(TokenRole::Instance, "old").unwrap();
    // 宽限期：新旧并存均有效
    let new = store.generate(TokenRole::Instance, "new").unwrap();
    assert!(store.validate(&old.token, TokenRole::Instance).is_ok());
    assert!(store.validate(&new.token, TokenRole::Instance).is_ok());
    // 吊销旧 → 旧失效、新有效
    store.revoke(&old.id).unwrap();
    assert!(matches!(
        store.validate(&old.token, TokenRole::Instance),
        Err(AuthError::RevokedToken { .. })
    ));
    assert!(store.validate(&new.token, TokenRole::Instance).is_ok());
}

// ---------------------------------------------------------------------------
// T4 原子写
// ---------------------------------------------------------------------------
#[test]
fn t4_atomic_write() {
    let dir = tempdir().unwrap();
    let mut store = new_store(dir.path());
    store.generate(TokenRole::Instance, "m1").unwrap();
    store.generate(TokenRole::Full, "tui").unwrap();

    // persist 后文件可解析
    let reloaded = TokenStore::load(&dir.path().join("tokens.toml")).unwrap();
    assert_eq!(reloaded.len(), 2);

    // tmp 文件不残留
    let leftovers: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().contains(".tmp"))
        .collect();
    assert!(leftovers.is_empty(), "不应残留 tmp 文件");
}

#[cfg(unix)]
#[test]
fn t4_atomic_write_failure_keeps_original() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempdir().unwrap();
    let path = dir.path().join("tokens.toml");
    let mut store = TokenStore::load(&path).unwrap();
    let rec = store.generate(TokenRole::Instance, "m1").unwrap();

    // 目录只读 → 下次写失败
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o500)).unwrap();
    let err = store.generate(TokenRole::Full, "tui").unwrap_err();
    assert!(
        matches!(err, crate::auth::StoreError::Io(_) | crate::auth::StoreError::Persist(_)),
        "只读目录写失败应报错: {err}"
    );
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();

    // 原文件完好（无半文件、无脏记录）
    let reloaded = TokenStore::load(&path).unwrap();
    assert_eq!(reloaded.len(), 1);
    assert_eq!(reloaded.list()[0].id, rec.id);
}

// ---------------------------------------------------------------------------
// T5 mtime 重载：外部改写（模拟 CLI revoke）→ 下次 validate 拒绝
// ---------------------------------------------------------------------------
#[test]
fn t5_mtime_reload() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("tokens.toml");
    let mut store = TokenStore::load(&path).unwrap();
    let rec = store.generate(TokenRole::Instance, "m1").unwrap();
    assert!(store.validate(&rec.token, TokenRole::Instance).is_ok());

    // 外部（CLI）改写文件：revoked = true
    let content = format!(
        "version = 1\n\n[[tokens]]\nid = \"{}\"\nrole = \"instance\"\nname = \"m1\"\ntoken = \"{}\"\ncreated_at = \"{}\"\nrevoked = true\n",
        rec.id,
        rec.token,
        rec.created_at.to_rfc3339()
    );
    std::fs::write(&path, content).unwrap();
    // 强制 mtime 前进（部分文件系统时间戳精度低，避免同值漏检）
    let f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
    f.set_modified(SystemTime::now() + Duration::from_secs(5)).unwrap();
    drop(f);

    // 下一次 validate 拒绝被吊销 token（mtime 变化触发重载）
    assert!(matches!(
        store.validate(&rec.token, TokenRole::Instance),
        Err(AuthError::RevokedToken { .. })
    ));
}

/// T5 补充：外部把文件改坏 → 保持旧内存态（不挂服务），旧 token 仍有效。
#[test]
fn t5_mtime_reload_bad_file_keeps_old_state() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("tokens.toml");
    let mut store = TokenStore::load(&path).unwrap();
    let rec = store.generate(TokenRole::Instance, "m1").unwrap();

    std::fs::write(&path, "not valid toml {{{").unwrap();
    let f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
    f.set_modified(SystemTime::now() + Duration::from_secs(5)).unwrap();
    drop(f);

    assert!(store.validate(&rec.token, TokenRole::Instance).is_ok(), "坏文件不应导致服务中断");
}

// ---------------------------------------------------------------------------
// T6 角色映射全组合
// ---------------------------------------------------------------------------
#[test]
fn t6_role_mapping() {
    let cases = [
        (TokenRole::Instance, Role::Instance, true),
        (TokenRole::Full, Role::Client, true),
        (TokenRole::ReadOnly, Role::Client, false),
    ];
    for (role, wire, can_send) in cases {
        assert_eq!(role.wire_role(), wire, "{role:?} 线级角色");
        assert_eq!(role.can_send_action(), can_send, "{role:?} 可发 action");
        assert_eq!(role.as_str(), role.to_string());
    }
}

// ---------------------------------------------------------------------------
// T7 常量时间比较（功能正确性）
// ---------------------------------------------------------------------------
#[test]
fn t7_constant_time_compare_semantics() {
    let dir = tempdir().unwrap();
    let mut store = new_store(dir.path());
    let rec = store.generate(TokenRole::Instance, "m1").unwrap();

    // 同 token 匹配
    assert!(store.validate(&rec.token, TokenRole::Instance).is_ok());
    // 异 token 不匹配（44 字符合法 base64，未登记）
    let foreign = base64::engine::general_purpose::STANDARD.encode([7u8; 32]);
    assert!(matches!(
        store.validate(&foreign, TokenRole::Instance),
        Err(AuthError::UnknownToken)
    ));
    // 等长前缀差异不匹配（改最后一个字符）
    let (head, last) = rec.token.split_at(rec.token.len() - 1);
    let mutated = format!("{head}{}", if last == "A" { "B" } else { "A" });
    assert!(matches!(
        store.validate(&mutated, TokenRole::Instance),
        Err(AuthError::UnknownToken)
    ));
}

/// T7 长度防御：加载时非 44 字符 → Err（拒绝启动）。
#[test]
fn t7_bad_token_length_rejected_on_load() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("tokens.toml");
    std::fs::write(
        &path,
        "version = 1\n\n[[tokens]]\nid = \"x\"\nrole = \"instance\"\nname = \"bad\"\ntoken = \"short\"\ncreated_at = \"2026-08-07T00:00:00Z\"\nrevoked = false\n",
    )
    .unwrap();
    assert!(matches!(
        TokenStore::load(&path),
        Err(crate::auth::StoreError::BadTokenLength)
    ));
}

// ---------------------------------------------------------------------------
// T8 脱敏
// ---------------------------------------------------------------------------
#[test]
fn t8_redaction() {
    let dir = tempdir().unwrap();
    let mut store = new_store(dir.path());
    let rec = store.generate(TokenRole::Full, "tui").unwrap();
    let token = rec.token.clone();

    // 1. TokenInfo 无 token 字段（结构保证）：Debug 输出不含 token 本体
    let info = &store.list()[0];
    assert!(!format!("{info:?}").contains(&token), "TokenInfo 泄露 token");
    assert!(!info.to_string().contains(&token), "TokenInfo Display 泄露 token");

    // 2. AuthError Display 不含凭证材料
    let err = store.validate(&token, TokenRole::Instance).unwrap_err();
    assert!(!err.to_string().contains(&token), "AuthError Display 泄露 token");

    // 3. 审计事件（认证失败路径）字段 ⊆ 白名单且不含 token
    let mut svc = AuthService::new(store);
    let (result, log) = with_capture(|| {
        block_on(svc.authenticate_instance(
            &make_hello("totally-unknown-token-value", &new_nonce_b64()),
            peer(),
        ))
    });
    assert!(matches!(result, Err(AuthError::UnknownToken)));
    assert_audit_redacted(&log, &[&token]);

    // 4. audit() 直接调用（成功路径）字段 ⊆ 白名单
    let (_, log) = with_capture(|| {
        audit(
            "auth.client",
            Some("cmd-1"),
            Some(&rec.id),
            "ok",
            Duration::from_millis(3),
            None,
        )
    });
    assert_audit_redacted(&log, &[&token]);
}

/// §4.8：失败路径审计事件携带 auth_failed_total 快照。
#[tokio::test]
async fn failure_audit_carries_total_snapshot() {
    let dir = tempdir().unwrap();
    let mut svc = AuthService::new(new_store(dir.path()));
    let (_, log) = with_capture(|| {
        block_on(svc.authenticate_instance(
            &make_hello("totally-unknown-token-value", &new_nonce_b64()),
            peer(),
        ))
    });
    assert!(log.contains("auth_failed_total"), "失败审计应携带快照: {log}");
    let v: serde_json::Value = serde_json::from_str(log.lines().next().unwrap()).unwrap();
    let fields = v["fields"].as_object().unwrap();
    assert_eq!(fields["auth_failed_total"].as_u64(), Some(1));
}

// ---------------------------------------------------------------------------
// H1 成功路径：独立重算 MAC 验证
// ---------------------------------------------------------------------------
#[tokio::test]
async fn h1_success_path() {
    let dir = tempdir().unwrap();
    let mut svc = AuthService::new(new_store(dir.path()));
    let rec = svc.store_mut().generate(TokenRole::Instance, "m1").unwrap();
    let nonce_b64 = new_nonce_b64();

    let ok = svc
        .authenticate_instance(&make_hello(&rec.token, &nonce_b64), peer())
        .await
        .unwrap();

    // 独立重算（proto 原语）
    let token_bytes: [u8; 32] = base64::engine::general_purpose::STANDARD
        .decode(&rec.token).unwrap().try_into().unwrap();
    let nonce_bytes: [u8; 32] = base64::engine::general_purpose::STANDARD
        .decode(&nonce_b64).unwrap().try_into().unwrap();
    let ctx_bytes: [u8; 32] = base64::engine::general_purpose::STANDARD
        .decode(&ok.response.connection_context).unwrap().try_into().unwrap();

    let key = derive_mac_key(&token_bytes, "instance");
    let input = mac_input(&nonce_bytes, &ctx_bytes, &PROTOCOL_VERSION.to_string(), "instance");
    let expected = base64::engine::general_purpose::STANDARD.encode(compute_mac(&key, &input));
    assert_eq!(ok.response.hmac, expected, "auth_response.hmac 与独立重算一致");
    assert!(verify_mac(&key, &input, &ok.response.hmac).is_ok(), "verify_mac 常量时间路径通过");

    // ctx 绑定信息
    assert_eq!(ok.ctx.token_id, rec.id);
    assert_eq!(ok.ctx.role, TokenRole::Instance);
    assert_eq!(ok.ctx.hostname.as_deref(), Some("test-host"));
    assert_eq!(ok.ctx.peer, peer());
    assert_eq!(ok.ctx.wire_role(), Role::Instance);
    assert!(ok.ctx.can_send_action());
}

// ---------------------------------------------------------------------------
// H2 错误 token：UnknownToken + 审计 failed + 计数（全局与 <unknown>）
// ---------------------------------------------------------------------------
#[tokio::test]
async fn h2_unknown_token() {
    let dir = tempdir().unwrap();
    let mut svc = AuthService::new(new_store(dir.path()));
    let (result, log) = with_capture(|| {
        block_on(svc.authenticate_instance(
            &make_hello("totally-unknown-token-value", &new_nonce_b64()),
            peer(),
        ))
    });
    assert!(matches!(result, Err(AuthError::UnknownToken)));
    assert_audit_redacted(&log, &[]);
    assert!(log.contains("auth.instance"), "应有 auth.instance 审计");
    assert!(log.contains("unknown_token"));
    assert_eq!(svc.stats().total_failures(), 1);
    assert_eq!(svc.stats().failures_for(UNKNOWN_TOKEN_ID), 1);
}

// ---------------------------------------------------------------------------
// H3 重放：同 nonce 二次 hello → ReplayNonce
// ---------------------------------------------------------------------------
#[tokio::test]
async fn h3_replay_nonce() {
    let dir = tempdir().unwrap();
    let mut svc = AuthService::new(new_store(dir.path()));
    let rec = svc.store_mut().generate(TokenRole::Instance, "m1").unwrap();
    let nonce_b64 = new_nonce_b64();

    assert!(svc
        .authenticate_instance(&make_hello(&rec.token, &nonce_b64), peer())
        .await
        .is_ok());
    // 同 nonce 二次 hello → 重放拒绝（即使 token 正确）
    let (result, log) = with_capture(|| {
        block_on(svc.authenticate_instance(
            &make_hello(&rec.token, &nonce_b64),
            peer(),
        ))
    });
    assert!(matches!(result, Err(AuthError::ReplayNonce)));
    assert_audit_redacted(&log, &[]);
    assert!(log.contains("replay_nonce"));
    // 重放 nonce 时 token 未校验 → 计数归 <unknown>
    assert_eq!(svc.stats().failures_for(UNKNOWN_TOKEN_ID), 1);
}

/// H3 补充：认证失败的 nonce 同样登记——坏 token + nonce N 失败后，
/// 同 nonce N + 好 token 依旧被拒（防「失败后重放成功路径」，§4.4）。
#[tokio::test]
async fn h3_failed_nonce_still_registered() {
    let dir = tempdir().unwrap();
    let mut svc = AuthService::new(new_store(dir.path()));
    let rec = svc.store_mut().generate(TokenRole::Instance, "m1").unwrap();
    let nonce_b64 = new_nonce_b64();

    let bad = svc
        .authenticate_instance(&make_hello("wrong-token", &nonce_b64), peer())
        .await;
    assert!(matches!(bad, Err(AuthError::UnknownToken)));

    let replay = svc
        .authenticate_instance(&make_hello(&rec.token, &nonce_b64), peer())
        .await;
    assert!(matches!(replay, Err(AuthError::ReplayNonce)));
}

// ---------------------------------------------------------------------------
// H4 过期 nonce：nonce_test N2 覆盖（窗口语义在 NonceRegistry 单测断言）；
// 此处确认 AuthService 对过期 nonce 重提按新 nonce 接受（sweep 后无残留）。
// ---------------------------------------------------------------------------
#[tokio::test]
async fn h4_expired_nonce_reaccepted() {
    let dir = tempdir().unwrap();
    let mut svc = AuthService::new(new_store(dir.path()));
    let rec = svc.store_mut().generate(TokenRole::Instance, "m1").unwrap();
    let nonce_b64 = new_nonce_b64();
    assert!(svc
        .authenticate_instance(&make_hello(&rec.token, &nonce_b64), peer())
        .await
        .is_ok());

    // 推进窗口：sweep 清空过期条目（等价于 30s 后）
    let t = std::time::Instant::now() + NONCE_TTL + Duration::from_secs(1);
    svc.nonces_mut().sweep(t);
    assert!(svc.nonces_mut().is_empty());

    // 同 nonce 重提 → 按新 nonce 接受（N2 新窗口语义的 AuthService 侧）
    let ok = svc
        .authenticate_instance(&make_hello(&rec.token, &nonce_b64), peer())
        .await;
    assert!(ok.is_ok(), "过期后同 nonce 重提应按新 nonce 接受: {ok:?}");
}

// ---------------------------------------------------------------------------
// H5 nonce 编码
// ---------------------------------------------------------------------------
#[tokio::test]
async fn h5_bad_nonce_encoding() {
    let dir = tempdir().unwrap();
    let mut svc = AuthService::new(new_store(dir.path()));
    let rec = svc.store_mut().generate(TokenRole::Instance, "m1").unwrap();

    // 非 base64
    let bad1 = svc
        .authenticate_instance(&make_hello(&rec.token, "!!!"), peer())
        .await;
    assert!(matches!(bad1, Err(AuthError::BadNonceEncoding)));

    // 非 32B（base64 of 16B）
    let short = base64::engine::general_purpose::STANDARD.encode([0u8; 16]);
    let bad2 = svc
        .authenticate_instance(&make_hello(&rec.token, &short), peer())
        .await;
    assert!(matches!(bad2, Err(AuthError::BadNonceEncoding)));
}

// ---------------------------------------------------------------------------
// H6 错误角色
// ---------------------------------------------------------------------------
#[tokio::test]
async fn h6_role_mismatch() {
    let dir = tempdir().unwrap();
    let mut svc = AuthService::new(new_store(dir.path()));

    // client token 提交 instance/hello → RoleMismatch
    let client = svc.store_mut().generate(TokenRole::Full, "tui").unwrap();
    let (result, log) = with_capture(|| {
        block_on(svc.authenticate_instance(
            &make_hello(&client.token, &new_nonce_b64()),
            peer(),
        ))
    });
    assert!(matches!(
        result,
        Err(AuthError::RoleMismatch { token_id }) if token_id == client.id
    ));
    assert_audit_redacted(&log, &[]);
    assert_eq!(svc.stats().failures_for(&client.id), 1, "按 token_id 计数");

    // instance token 提交 client 认证 → RoleMismatch
    let instance = svc.store_mut().generate(TokenRole::Instance, "m1").unwrap();
    let result = svc
        .authenticate_client(&Auth { token: instance.token.clone() }, peer())
        .await;
    assert!(matches!(
        result,
        Err(AuthError::RoleMismatch { token_id }) if token_id == instance.id
    ));

    // full token 通过 client 认证（含 read-only）
    let ok = svc
        .authenticate_client(&Auth { token: client.token.clone() }, peer())
        .await
        .unwrap();
    assert_eq!(ok.wire_role(), Role::Client);
    let ro = svc.store_mut().generate(TokenRole::ReadOnly, "web").unwrap();
    let ok = svc
        .authenticate_client(&Auth { token: ro.token }, peer())
        .await
        .unwrap();
    assert_eq!(ok.role, TokenRole::ReadOnly);
    assert!(!ok.can_send_action(), "read-only 不可发 action");
}

// ---------------------------------------------------------------------------
// H7 版本绑定：机侧用错误版本重算 → verify_mac 失败（字节级，§4.8 向量 12）
// ---------------------------------------------------------------------------
#[tokio::test]
async fn h7_version_binding() {
    let dir = tempdir().unwrap();
    let mut svc = AuthService::new(new_store(dir.path()));
    let rec = svc.store_mut().generate(TokenRole::Instance, "m1").unwrap();
    let nonce_b64 = new_nonce_b64();

    let ok = svc
        .authenticate_instance(&make_hello(&rec.token, &nonce_b64), peer())
        .await
        .unwrap();

    let token_bytes: [u8; 32] = base64::engine::general_purpose::STANDARD
        .decode(&rec.token).unwrap().try_into().unwrap();
    let nonce_bytes: [u8; 32] = base64::engine::general_purpose::STANDARD
        .decode(&nonce_b64).unwrap().try_into().unwrap();
    let ctx_bytes: [u8; 32] = base64::engine::general_purpose::STANDARD
        .decode(&ok.response.connection_context).unwrap().try_into().unwrap();
    let key = derive_mac_key(&token_bytes, "instance");

    // 正确版本通过
    let input = mac_input(&nonce_bytes, &ctx_bytes, &PROTOCOL_VERSION.to_string(), "instance");
    assert!(verify_mac(&key, &input, &ok.response.hmac).is_ok());

    // 错误版本 → Mismatch（版本绑定天然拒绝，§4.5）
    let wrong_input = mac_input(&nonce_bytes, &ctx_bytes, "2", "instance");
    assert!(matches!(
        verify_mac(&key, &wrong_input, &ok.response.hmac),
        Err(acp_hub_proto::hmac::HmacError::Mismatch)
    ));

    // 错误角色 → Mismatch（角色绑定）
    let wrong_role = mac_input(&nonce_bytes, &ctx_bytes, &PROTOCOL_VERSION.to_string(), "client");
    assert!(matches!(
        verify_mac(&key, &wrong_role, &ok.response.hmac),
        Err(acp_hub_proto::hmac::HmacError::Mismatch)
    ));
}

// ---------------------------------------------------------------------------
// H8 未知身份（未登记 token）
// ---------------------------------------------------------------------------
#[tokio::test]
async fn h8_unknown_identity() {
    let dir = tempdir().unwrap();
    let mut svc = AuthService::new(new_store(dir.path()));
    let (result, log) = with_capture(|| {
        block_on(svc.authenticate_instance(
            &make_hello("0000000000000000000000000000000000000000", &new_nonce_b64()),
            peer(),
        ))
    });
    assert!(matches!(result, Err(AuthError::UnknownToken)));
    assert_audit_redacted(&log, &[]);
    assert_eq!(svc.stats().failures_for(UNKNOWN_TOKEN_ID), 1);
}

// ---------------------------------------------------------------------------
// H9 失败计数：按 token_id 递增；吊销与未知分开计数
// ---------------------------------------------------------------------------
#[tokio::test]
async fn h9_failure_counting() {
    let dir = tempdir().unwrap();
    let mut svc = AuthService::new(new_store(dir.path()));
    let rec = svc.store_mut().generate(TokenRole::Instance, "m1").unwrap();
    let client = svc.store_mut().generate(TokenRole::Full, "tui").unwrap();

    // 3 次未知 token 失败（instance + client 面）
    for _ in 0..3 {
        let _ = svc
            .authenticate_client(&Auth { token: "no-such-token".into() }, peer())
            .await;
    }
    assert_eq!(svc.stats().failures_for(UNKNOWN_TOKEN_ID), 3);

    // 角色不匹配（已知 id）
    let _ = svc
        .authenticate_instance(&make_hello(&client.token, &new_nonce_b64()), peer())
        .await;
    assert_eq!(svc.stats().failures_for(&client.id), 1);

    // 吊销后失败（已知 id，与未知分开）
    svc.store_mut().revoke(&rec.id).unwrap();
    let _ = svc
        .authenticate_instance(&make_hello(&rec.token, &new_nonce_b64()), peer())
        .await;
    assert_eq!(svc.stats().failures_for(&rec.id), 1, "吊销与未知分开计数");
    assert_eq!(svc.stats().failures_for(UNKNOWN_TOKEN_ID), 3, "未知计数不受影响");
    assert_eq!(svc.stats().total_failures(), 5);
}

// ---------------------------------------------------------------------------
// H10 关闭码：认证失败映射 CLOSE_CONFIG_FATAL(4502)
// ---------------------------------------------------------------------------
#[test]
fn h10_close_code() {
    assert_eq!(acp_hub_proto::conn::CLOSE_CONFIG_FATAL, 4502);
    // 非回环拒绝用 1011（§5 决策），与认证失败码区分。
    assert_ne!(acp_hub_proto::conn::CLOSE_CONFIG_FATAL, acp_hub_proto::conn::CLOSE_GENERIC_FAILURE);
}

// ---------------------------------------------------------------------------
// 补充：bootstrap（§4.3.4）
// ---------------------------------------------------------------------------
#[test]
fn bootstrap_instance_token() {
    let dir = tempdir().unwrap();
    let mut store = new_store(dir.path());
    // 空 store → 生成 bootstrap instance token（name = 本机缺省 id "local"，
    // 与 channel::DEFAULT_INSTANCE_ID 一致，§4.3 P5 缺省路由才可命中本机）
    let rec = store.ensure_instance_token().unwrap().expect("应生成");
    assert_eq!(rec.name, crate::channel::DEFAULT_INSTANCE_ID);
    assert_eq!(rec.name, "local");
    assert_eq!(rec.role, TokenRole::Instance);
    // 已存在 instance token → 不再生成
    assert!(store.ensure_instance_token().unwrap().is_none());
    // 只有 client token 时 → 仍生成 instance token
    let mut store2 = new_store(&dir.path().join("sub"));
    std::fs::create_dir_all(dir.path().join("sub")).unwrap();
    store2.generate(TokenRole::Full, "tui").unwrap();
    let rec2 = store2.ensure_instance_token().unwrap().expect("应生成 instance token");
    assert_eq!(rec2.role, TokenRole::Instance);
}

// ---------------------------------------------------------------------------
// 补充：client 认证上下文
// ---------------------------------------------------------------------------
#[tokio::test]
async fn client_ctx_fields() {
    let dir = tempdir().unwrap();
    let mut svc = AuthService::new(new_store(dir.path()));
    let rec = svc.store_mut().generate(TokenRole::Full, "桌面 TUI").unwrap();
    let ctx = svc
        .authenticate_client(&Auth { token: rec.token }, peer())
        .await
        .unwrap();
    assert_eq!(ctx.token_id, rec.id);
    assert_eq!(ctx.name, "桌面 TUI");
    assert_eq!(ctx.hostname, None);
    assert_eq!(ctx.peer, peer());
    assert_eq!(ctx.wire_role(), Role::Client);
    assert!(ctx.can_send_action());
}

/// ConnectionCtx 类型级：wire_role/can_send_action 委托 TokenRole 映射。
#[test]
fn ctx_delegates_to_role() {
    let ctx = |role: TokenRole| ConnectionCtx {
        token_id: "id".into(),
        role,
        name: "n".into(),
        peer: peer(),
        hostname: None,
        established_at: chrono::Utc::now(),
    };
    assert_eq!(ctx(TokenRole::Instance).wire_role(), Role::Instance);
    assert!(ctx(TokenRole::Instance).can_send_action());
    assert!(!ctx(TokenRole::ReadOnly).can_send_action());
}

/// TokenRecord 克隆一致性（TokenInfo 转换不丢字段）。
#[test]
fn token_info_from_record() {
    let dir = tempdir().unwrap();
    let mut store = new_store(dir.path());
    let rec = store.generate(TokenRole::ReadOnly, "web").unwrap();
    let info: crate::auth::TokenInfo = (&rec).into();
    assert_eq!(info.id, rec.id);
    assert_eq!(info.role, rec.role);
    assert_eq!(info.name, rec.name);
    assert_eq!(info.created_at, rec.created_at);
    assert_eq!(info.revoked, rec.revoked);
}
