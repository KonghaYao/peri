//! WP-006：HTTP 身份、凭证与状态归属（R-007 / FC-HTTP-01 / FC-STATE-01）。
//!
//! 断言重点：
//! - bearer token 缺失/错误一律 401，且拒绝路径不回显提交值；
//! - 可信 principal 只由「token 校验成功」这一事实产生，`_meta.clientInfo` 等自报字段无效；
//! - 连接实例每条 TCP 连接唯一、同连接内稳定，且随请求到达 MCP core（状态归属依据）；
//! - token 值不出现在任何响应头/响应体/错误文本中；
//! - token 来源不可用时传输拒绝启动（fail closed）。

mod http_support;

use std::sync::Mutex;

use serde_json::json;

use http_support::*;
use local_mcp_server::auth::{AuthError, Authenticator, WWW_AUTHENTICATE_VALUE};
use local_mcp_server::config::{AuthConfig, TokenSource};
use local_mcp_server::wire::META_KEY_CLIENT_INFO;

/// 进程级环境变量写入必须串行化（TEST-HERMETIC-001）。
static ENV_LOCK: Mutex<()> = Mutex::new(());

/// 临时注入环境变量并在 Drop 时恢复原值。
struct ScopedEnv {
    key: String,
    previous: Option<String>,
    _guard: std::sync::MutexGuard<'static, ()>,
}

impl ScopedEnv {
    fn set(key: &str, value: &str) -> Self {
        let guard = ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let previous = std::env::var(key).ok();
        std::env::set_var(key, value);
        Self {
            key: key.to_string(),
            previous,
            _guard: guard,
        }
    }
}

impl Drop for ScopedEnv {
    fn drop(&mut self) {
        match &self.previous {
            Some(value) => std::env::set_var(&self.key, value),
            None => std::env::remove_var(&self.key),
        }
    }
}

// ──────────────────────────────── bearer 认证 ────────────────────────────────

/// 缺失/错误/畸形凭证一律 401，且带 `WWW-Authenticate`；正确凭证放行。
#[test]
fn bearer_token_gate_rejects_missing_and_wrong_credentials() {
    let token_file = TokenFile::random();
    let mut config = base_config();
    config.auth = token_file.auth_config();
    let server = TestServer::start_with(config);

    let missing = server.connect().send(&modern_tools_list(1));
    assert_eq!(missing.status, 401, "缺失 Authorization 必须 401");
    assert_eq!(
        missing.header("www-authenticate"),
        Some(WWW_AUTHENTICATE_VALUE)
    );
    assert!(
        !missing.body_text().contains("secret"),
        "401 文案不得包含任何凭证材料"
    );

    let wrong = server
        .connect()
        .send(&modern_tools_list(2).header("Authorization", "Bearer wrong-token"));
    assert_eq!(wrong.status, 401, "错误 token 必须 401");
    assert_eq!(
        wrong.body_text(),
        missing.body_text(),
        "401 不得区分「缺失」与「错误」，避免成为猜测 oracle"
    );

    let malformed = server
        .connect()
        .send(&modern_tools_list(3).header("Authorization", "Basic dXNlcjpwYXNz"));
    assert_eq!(malformed.status, 401, "非 Bearer 方案必须 401");

    let mut authorized = server.connect();
    let ok = authorized.send(
        &modern_tools_list(4).header("Authorization", &format!("Bearer {}", token_file.token())),
    );
    assert_eq!(ok.status, 200, "正确 token 必须放行");

    // 认证先于 body 读取：未认证的超大 body 得到 401 而不是 413。
    let oversized_unauthenticated = server.connect().send(&modern_tools_call(
        "Read",
        json!({ "file_path": "x".repeat(64 * 1024) }),
        5,
    ));
    assert_eq!(
        oversized_unauthenticated.status, 401,
        "暴露面门禁必须早于 body 读取"
    );
}

/// token 来源不可用（env 缺失 / 文件不可读 / 空文件）时传输必须拒绝启动。
#[test]
fn unusable_token_source_fails_closed_at_startup() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");

    let missing_env = format!("LOCAL_MCP_TEST_MISSING_{}", std::process::id());
    let mut config = base_config();
    config.auth = AuthConfig {
        token: Some(TokenSource::Env {
            var: missing_env.clone(),
        }),
    };
    let result = runtime.block_on(local_mcp_server::transport::http::serve_http(
        &config,
        FixtureHandler::factory_for(observation_sink()),
    ));
    match result {
        Err(local_mcp_server::transport::http::HttpTransportError::Auth(
            AuthError::EnvUnavailable { var },
        )) => assert_eq!(var, missing_env),
        Ok(_) => panic!("env 来源缺失必须 fail closed，但服务端启动了"),
        Err(other) => panic!("env 来源缺失必须 fail closed，实际错误: {other}"),
    }

    let dir = tempfile::tempdir().expect("tempdir");
    let missing_file = dir.path().join("nope");
    let mut file_config = base_config();
    file_config.auth = AuthConfig {
        token: Some(TokenSource::File { path: missing_file }),
    };
    let result = runtime.block_on(local_mcp_server::transport::http::serve_http(
        &file_config,
        FixtureHandler::factory_for(observation_sink()),
    ));
    assert!(
        matches!(
            result,
            Err(local_mcp_server::transport::http::HttpTransportError::Auth(
                AuthError::FileUnreadable { .. }
            ))
        ),
        "文件来源不可读必须 fail closed"
    );

    let empty_file = dir.path().join("empty");
    std::fs::write(&empty_file, "\n").expect("write");
    let mut empty_config = base_config();
    empty_config.auth = AuthConfig {
        token: Some(TokenSource::File { path: empty_file }),
    };
    let result = runtime.block_on(local_mcp_server::transport::http::serve_http(
        &empty_config,
        FixtureHandler::factory_for(observation_sink()),
    ));
    assert!(
        matches!(
            result,
            Err(local_mcp_server::transport::http::HttpTransportError::Auth(
                AuthError::EmptyToken { .. }
            ))
        ),
        "空 token 必须 fail closed"
    );
}

/// env 来源可用时认证成功，且 env 变量名与值都不进入 response。
#[test]
fn env_token_source_authenticates_without_leaking() {
    let var = format!("LOCAL_MCP_TEST_TOKEN_{}", std::process::id());
    let token = format!("env-token-{}", std::process::id());
    let _scoped = ScopedEnv::set(&var, &token);

    let mut config = base_config();
    config.auth = AuthConfig {
        token: Some(TokenSource::Env { var: var.clone() }),
    };
    let server = TestServer::start_with(config);

    let mut client = server.connect();
    let response = client.send(
        &modern_tools_call("Read", json!({ "file_path": "a.txt" }), 1)
            .header("Authorization", &format!("Bearer {token}")),
    );
    assert_eq!(response.status, 200);
    assert!(!response.body_text().contains(&token));
    assert!(!response.body_text().contains(&var));
}

// ─────────────────────────────── principal 来源 ───────────────────────────────

/// 可信 principal 由 token 校验产生；客户端自报的 `clientInfo` 不影响它。
#[test]
fn principal_comes_from_token_not_from_client_claims() {
    let token_file = TokenFile::random();
    let mut config = base_config();
    config.auth = token_file.auth_config();
    let server = TestServer::start_with(config);

    let auth_header = format!("Bearer {}", token_file.token());
    let mut client = server.connect();

    let mut first = modern_tools_call("Read", json!({ "file_path": "a.txt" }), 1)
        .header("Authorization", &auth_header);
    // 自报身份尝试：伪造 clientInfo。
    first = first.body_bytes(
        serde_json::to_vec(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "Read",
                "arguments": { "file_path": "a.txt" },
                "_meta": {
                    "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                    "io.modelcontextprotocol/clientCapabilities": {},
                    META_KEY_CLIENT_INFO: { "name": "attacker-chosen-principal", "version": "9.9.9" },
                }
            }
        }))
        .expect("serialize"),
    );
    let response = client.send(&first);
    assert_eq!(response.status, 200);
    let identity = identity_of(&response);
    let principal = identity.principal.expect("必须绑定 principal");
    assert!(
        principal.starts_with("bearer-"),
        "principal 必须由认证层铸造: {principal}"
    );
    assert_ne!(
        principal, "attacker-chosen-principal",
        "客户端自报 clientInfo 不得成为 principal"
    );
    assert_eq!(
        identity.client_info_name.as_deref(),
        Some("attacker-chosen-principal"),
        "clientInfo 只用于显示，仍会原样出现在上下文里（对照）"
    );

    // 同一 token 的后续请求得到同一 principal。
    let second = client.send(
        &modern_tools_call("Read", json!({ "file_path": "a.txt" }), 2)
            .header("Authorization", &auth_header),
    );
    assert_eq!(
        identity_of(&second).principal.as_deref(),
        Some(principal.as_str()),
        "同一 token 必须映射同一 principal"
    );
}

/// 未配置 token 时使用回环主体，且仍是显式 principal（不是 None）。
#[test]
fn loopback_mode_binds_an_explicit_local_principal() {
    let server = TestServer::start();
    let mut client = server.connect();
    let response = client.send(&modern_tools_call(
        "Read",
        json!({ "file_path": "a.txt" }),
        1,
    ));
    assert_eq!(response.status, 200);
    let identity = identity_of(&response);
    assert_eq!(
        identity.principal.as_deref(),
        Some(local_mcp_server::auth::LOOPBACK_PRINCIPAL)
    );
}

// ─────────────────────────────── 连接实例归属 ───────────────────────────────

/// 连接实例每条 TCP 连接唯一、同连接内稳定（状态归属的最小前提）。
#[test]
fn client_instance_is_per_connection_and_stable_within_it() {
    let server = TestServer::start();

    let mut first = server.connect();
    let a = identity_of(&first.send(&modern_tools_call(
        "Read",
        json!({ "file_path": "a.txt" }),
        1,
    )));
    let b = identity_of(&first.send(&modern_tools_call(
        "Read",
        json!({ "file_path": "b.txt" }),
        2,
    )));
    assert_eq!(
        a.client_instance, b.client_instance,
        "同一连接内的 modern 请求必须共享实例（否则任务句柄无法跨请求使用）"
    );
    assert_ne!(a.client_instance, None, "实例必须存在（不接受无身份请求）");

    let mut second = server.connect();
    let c = identity_of(&second.send(&modern_tools_call(
        "Read",
        json!({ "file_path": "a.txt" }),
        3,
    )));
    assert_ne!(
        a.client_instance, c.client_instance,
        "不同连接必须是不同实例"
    );
    assert!(
        a.client_instance
            .as_deref()
            .is_some_and(|value| value.starts_with("http-conn-")),
        "实例 id 应为不可猜随机值: {:?}",
        a.client_instance
    );
}

/// legacy 会话把实例固定到会话：同一主体在另一条连接上继续会话时实例不变。
#[test]
fn legacy_session_keeps_the_originating_instance() {
    let server = TestServer::start();

    let mut opener = server.connect();
    let initialize = opener.send(&legacy_initialize(1));
    let session = initialize
        .header("mcp-session-id")
        .expect("会话 id")
        .to_string();
    let opened_identity = identity_of(&opener.send(&legacy_tools_call(
        "Read",
        json!({ "file_path": "a.txt" }),
        2,
        &session,
    )));

    // 另一条连接（新实例）在同一主体下继续同一会话：实例归一到会话创建者。
    let mut resumer = server.connect();
    let resumed = resumer.send(&legacy_tools_call(
        "Read",
        json!({ "file_path": "a.txt" }),
        3,
        &session,
    ));
    assert_eq!(resumed.status, 200);
    let resumed_identity = identity_of(&resumed);
    assert_eq!(
        resumed_identity.client_instance, opened_identity.client_instance,
        "legacy 会话归属由会话决定，跨连接保持一致（legacy session owner）"
    );

    // 无会话头的现代请求在新连接上得到该连接自己的实例。
    let modern = resumer.send(&modern_tools_call(
        "Read",
        json!({ "file_path": "a.txt" }),
        4,
    ));
    let modern_identity = identity_of(&modern);
    assert_ne!(
        modern_identity.client_instance, opened_identity.client_instance,
        "modern 请求不使用会话绑定"
    );
}

/// 未知会话 id 直接 404，且不会因此获得任何身份。
#[test]
fn unknown_session_is_404_and_grants_no_identity() {
    let server = TestServer::start();
    let mut client = server.connect();
    let response = client.send(&legacy_tools_call(
        "Read",
        json!({ "file_path": "a.txt" }),
        1,
        "0f0f0f0f-0000-4000-8000-000000000000",
    ));
    assert_eq!(response.status, 404);
    assert!(response.body_text().contains("Session not found"));
    assert!(
        server.observations().is_empty(),
        "未知会话不得进入 MCP handler"
    );
}

/// 会话 id 不在另一台服务实例上生效（跨进程不共享状态，也不泄露存在性）。
#[test]
fn session_ids_are_scoped_to_one_server_instance() {
    let first_server = TestServer::start();
    let mut opener = first_server.connect();
    let session = opener
        .send(&legacy_initialize(1))
        .header("mcp-session-id")
        .expect("会话 id")
        .to_string();

    let second_server = TestServer::start();
    let mut other = second_server.connect();
    let foreign = other.send(&legacy_tools_call(
        "Read",
        json!({ "file_path": "a.txt" }),
        2,
        &session,
    ));
    assert_eq!(foreign.status, 404, "会话 id 不得跨服务实例生效");
}

// ─────────────────────────────── 凭证不泄露 ───────────────────────────────

/// 横扫所有交互产生的响应头与响应体：token 值、`Authorization` 头一律不得出现。
#[test]
fn token_never_appears_in_any_response_surface() {
    let token_file = TokenFile::random();
    let token = token_file.token().to_string();
    let mut config = base_config();
    config.auth = token_file.auth_config();
    let server = TestServer::start_with(config);

    let auth_header = format!("Bearer {token}");
    let mut samples: Vec<RawResponse> = Vec::new();
    for id in 0..3u64 {
        samples.push(server.connect().send(&modern_tools_list(id)));
        samples.push(
            server
                .connect()
                .send(&modern_tools_list(id).header("Authorization", "Bearer not-the-token")),
        );
        samples.push(server.connect().send(&modern_tools_call(
            "Read",
            json!({ "file_path": "a.txt" }),
            id,
        )));
        samples.push(
            server.connect().send(
                &modern_tools_call("Read", json!({ "file_path": "a.txt" }), id)
                    .header("Authorization", &auth_header),
            ),
        );
    }
    samples.push(
        server
            .connect()
            .send(&legacy_initialize(9).header("Authorization", &auth_header)),
    );

    for response in &samples {
        let body = response.body_text();
        assert!(!body.contains(&token), "响应体泄露 token: {body}");
        for (name, value) in &response.headers {
            assert!(!value.contains(&token), "响应头 {name} 泄露 token: {value}");
            assert!(
                !name.eq_ignore_ascii_case("authorization"),
                "响应不得回显 Authorization 头"
            );
        }
    }
}

/// 认证器自身的 Debug 表示不含 token（结构性保证 + 断言）。
#[test]
fn authenticator_debug_redacts_token() {
    let token_file = TokenFile::random();
    let authenticator =
        Authenticator::from_config(&token_file.auth_config()).expect("authenticator");
    let rendered = format!("{authenticator:?}");
    assert!(!rendered.contains(token_file.token()));
    assert!(rendered.contains("redacted"));
    assert!(!authenticator.method().as_str().is_empty());
}

/// 捕获写入器：把所有诊断输出收进内存缓冲区。
#[derive(Clone)]
struct CaptureWriter(std::sync::Arc<Mutex<Vec<u8>>>);

impl std::io::Write for CaptureWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// 诊断日志（最严格：TRACE 全量捕获）不得出现 token 值或 Authorization 头内容。
#[test]
fn token_never_appears_in_server_diagnostics() {
    let buffer = std::sync::Arc::new(Mutex::new(Vec::<u8>::new()));
    let writer = CaptureWriter(buffer.clone());
    let installed = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .try_init()
        .is_ok();

    let token_file = TokenFile::random();
    let token = token_file.token().to_string();
    let mut config = base_config();
    config.auth = token_file.auth_config();
    let mut server = TestServer::start_with(config);

    let auth_header = format!("Bearer {token}");
    let mut authorized = server.connect();
    authorized.send(
        &modern_tools_call("Read", json!({ "file_path": "a.txt" }), 1)
            .header("Authorization", &auth_header),
    );
    authorized.send(
        &modern_tools_call("Read", json!({ "file_path": "forbidden-name.txt" }), 2)
            .header("Authorization", "Bearer orion-canary-value-xyz"),
    );
    server.connect().send(&modern_tools_list(3));
    server
        .connect()
        .send(&modern_tools_list(4).header("Host", "evil.test"));
    server.shutdown();

    let captured = String::from_utf8_lossy(
        &buffer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()),
    )
    .to_string();

    assert!(
        !captured.contains(&token),
        "诊断日志泄露 token（捕获 {} 字节）",
        captured.len()
    );
    assert!(
        !captured.contains("orion-canary-value-xyz"),
        "诊断日志泄露提交的凭证"
    );
    assert!(
        !captured
            .to_ascii_lowercase()
            .contains("authorization: bearer"),
        "诊断日志不得打印 Authorization 头"
    );
    if installed {
        // 反向对照：捕获确实工作（否则上面的断言可能是空的）。
        assert!(
            captured.contains("Streamable HTTP 传输已启动")
                || captured.contains("拒绝未通过认证的请求")
                || captured.contains("rejected"),
            "日志捕获为空，无法证明未泄露：{}",
            &captured[..captured.len().min(400)]
        );
    }
}
