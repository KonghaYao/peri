//! Web HTTP surface tests: bounded cookie auth, request parsing, upgrade
//! detection, static routing/cache policy and real socket responses.
//!
//! 路由断言面向 vite 构建产物（web/dist，build.rs 编译期内嵌）：页面入口
//! 固定（/、/panel.html），assets 文件名带内容 hash —— 测试遍历 ASSETS
//! 表断言 js/css 存在，不硬编码 hash 文件名。

use std::sync::Arc;

use tempfile::tempdir;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;

use crate::auth::{AuthService, TokenRole, TokenStore};
use crate::web::{
    cache_headers_for_static, cookie_value, is_json_content_type, serve_http, valid_loopback_host,
};
use crate::web::{
    content_type, header_end, is_ws_upgrade, request_path, route, serve, BrowserAuthSetup, ASSETS,
};

fn test_auth_setup(dir: &std::path::Path) -> BrowserAuthSetup {
    let mut cfg = crate::config::Config::defaults();
    cfg.config_dir = dir.to_path_buf();
    BrowserAuthSetup::from_config(&cfg)
}

async fn auth_socket_response(request: &str) -> String {
    let dir = tempdir().unwrap();
    let auth = Arc::new(Mutex::new(AuthService::new(
        TokenStore::load(&dir.path().join("tokens.toml")).unwrap(),
    )));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, peer) = listener.accept().await.unwrap();
        serve_http(stream, peer, auth, test_auth_setup(dir.path()))
            .await
            .unwrap();
    });
    let mut client = TcpStream::connect(addr).await.unwrap();
    client.write_all(request.as_bytes()).await.unwrap();
    let mut buf = Vec::new();
    client.read_to_end(&mut buf).await.unwrap();
    server.await.unwrap();
    String::from_utf8(buf).unwrap()
}

async fn auth_socket_response_fragments(parts: &[&[u8]]) -> String {
    let dir = tempdir().unwrap();
    let auth = Arc::new(Mutex::new(AuthService::new(
        TokenStore::load(&dir.path().join("tokens.toml")).unwrap(),
    )));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let setup = test_auth_setup(dir.path());
    let server = tokio::spawn(async move {
        let (stream, peer) = listener.accept().await.unwrap();
        serve_http(stream, peer, auth, setup).await.unwrap();
    });
    let mut client = TcpStream::connect(addr).await.unwrap();
    for part in parts {
        client.write_all(part).await.unwrap();
        tokio::task::yield_now().await;
    }
    let mut buf = Vec::new();
    client.read_to_end(&mut buf).await.unwrap();
    server.await.unwrap();
    String::from_utf8(buf).unwrap()
}

/// 请求行解析：常规 GET 路径。
#[test]
fn request_path_get() {
    assert_eq!(
        request_path("GET / HTTP/1.1\r\nHost: 127.0.0.1:8456\r\n\r\n"),
        Some("/")
    );
    assert_eq!(
        request_path("GET /assets/index-abc.js HTTP/1.1\r\nHost: test\r\n\r\n"),
        Some("/assets/index-abc.js")
    );
}

/// 请求行解析：query 剥离（资源名纯 ASCII，不做 URL 解码）。
#[test]
fn request_path_strips_query() {
    assert_eq!(
        request_path("GET /panel.html?v=123 HTTP/1.1\r\nHost: test\r\n\r\n"),
        Some("/panel.html")
    );
}

/// 请求行解析：非 GET 与格式非法 → None。
#[test]
fn request_path_rejects_non_get_and_garbage() {
    assert_eq!(request_path("POST / HTTP/1.1\r\nHost: test\r\n\r\n"), None);
    assert_eq!(request_path("GET"), None);
    assert_eq!(request_path(""), None);
    assert_eq!(request_path("GARBAGE\r\n\r\n"), None);
}

/// 头部结束符定位：完整头部返回 `\r\n\r\n` 下标，不完整返回 None。
#[test]
fn header_end_finds_terminator() {
    let full = b"GET / HTTP/1.1\r\nHost: test\r\n\r\n";
    assert_eq!(header_end(full), Some(26));
    assert_eq!(header_end(&full[..20]), None);
    assert_eq!(header_end(b""), None);
}

/// ws 升级判定：大小写不敏感；普通 GET 不含 upgrade 头 → false。
#[test]
fn ws_upgrade_detection() {
    let ws = b"GET /instance HTTP/1.1\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\r\n";
    assert!(is_ws_upgrade(ws));
    let ws_lower = b"GET / HTTP/1.1\r\nupgrade: websocket\r\n\r\n";
    assert!(is_ws_upgrade(ws_lower));
    let plain = b"GET /index.html HTTP/1.1\r\nHost: test\r\n\r\n";
    assert!(!is_ws_upgrade(plain));
}

#[test]
fn auth_contract_host_and_cookie_parsing() {
    assert!(valid_loopback_host("127.0.0.1:8456"));
    assert!(valid_loopback_host("localhost:8456"));
    assert!(valid_loopback_host("[::1]:8456"));
    assert!(!valid_loopback_host("evil.example:8456"));
    assert_eq!(
        cookie_value("x=1; acp_hub_session=opaque; y=2", "acp_hub_session").as_deref(),
        Some("opaque")
    );
}

#[test]
fn auth_json_content_type_is_closed() {
    assert!(is_json_content_type("application/json"));
    assert!(is_json_content_type("Application/JSON; charset=utf-8"));
    assert!(!is_json_content_type("text/plain"));
    assert!(!is_json_content_type("application/json; charset=latin1"));
    assert!(!is_json_content_type("application/json; boundary=x"));
    assert!(!is_json_content_type(
        "application/json; charset=utf-8; charset=utf-8"
    ));
}

#[test]
fn browser_auth_setup_uses_authoritative_config_dir_and_shell_quotes_it() {
    let mut cfg = crate::config::Config::defaults();
    cfg.config_dir = std::path::PathBuf::from("/tmp/acp hub/operator's config");

    let setup = BrowserAuthSetup::from_parts(&cfg, "/opt/acp hub/bin/acp-hub-server");
    let json = serde_json::to_value(setup).unwrap();

    assert_eq!(
        json["tokenFile"],
        "/tmp/acp hub/operator's config/tokens.toml"
    );
    assert_eq!(
        json["generateCommand"],
        "ACP_HUB_CONFIG_DIR='/tmp/acp hub/operator'\\''s config' '/opt/acp hub/bin/acp-hub-server' token generate --name web --role full"
    );
    assert_eq!(json.as_object().unwrap().len(), 2);
}

#[tokio::test]
async fn unauthenticated_status_returns_credential_free_setup_hint() {
    let response = auth_socket_response(
        "GET /api/auth/session HTTP/1.1\r\nHost: 127.0.0.1:8456\r\nContent-Length: 0\r\n\r\n",
    )
    .await;

    assert!(response.starts_with("HTTP/1.1 401 Unauthorized\r\n"));
    assert!(response.contains("\"authenticated\":false"));
    assert!(response.contains("\"tokenFile\":"));
    assert!(response.contains("tokens.toml"));
    assert!(response.contains("\"generateCommand\":"));
    assert!(!response.contains("token_id"));
    assert!(!response.contains("tokenId"));
}

#[tokio::test]
async fn successful_login_never_reflects_the_bearer_or_token_record() {
    let dir = tempdir().unwrap();
    let token_path = dir.path().join("tokens.toml");
    let mut token_store = TokenStore::load(&token_path).unwrap();
    let record = token_store
        .generate(TokenRole::Full, "browser-secret-name")
        .unwrap();
    let bearer = record.token.clone();
    let token_id = record.id.clone();
    let auth = Arc::new(Mutex::new(AuthService::new(token_store)));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let setup = test_auth_setup(dir.path());
    let server = tokio::spawn(async move {
        let (stream, peer) = listener.accept().await.unwrap();
        serve_http(stream, peer, auth, setup).await.unwrap();
    });

    let body = serde_json::json!({ "token": bearer }).to_string();
    let request = format!(
        "POST /api/auth/session HTTP/1.1\r\nHost: 127.0.0.1:8456\r\nOrigin: http://127.0.0.1:8456\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    let mut client = TcpStream::connect(addr).await.unwrap();
    client.write_all(request.as_bytes()).await.unwrap();
    let mut bytes = Vec::new();
    client.read_to_end(&mut bytes).await.unwrap();
    server.await.unwrap();
    let response = String::from_utf8(bytes).unwrap();

    assert!(response.starts_with("HTTP/1.1 200 OK\r\n"));
    assert!(response.contains("Set-Cookie: acp_hub_session="));
    assert!(!response.contains(&record.token));
    assert!(!response.contains(&token_id));
    assert!(!response.contains("browser-secret-name"));
    let response_body = response.split_once("\r\n\r\n").unwrap().1;
    let json: serde_json::Value = serde_json::from_str(response_body).unwrap();
    let keys = json
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        keys,
        ["authenticated", "role", "setup"]
            .into_iter()
            .map(String::from)
            .collect()
    );
    assert_eq!(json["authenticated"], true);
    assert_eq!(json["role"], "full");
}

#[tokio::test]
async fn auth_lock_contention_is_service_unavailable_not_bad_credentials() {
    let dir = tempdir().unwrap();
    let auth = Arc::new(Mutex::new(AuthService::new(
        TokenStore::load(&dir.path().join("tokens.toml")).unwrap(),
    )));
    let guard = auth.lock().await;
    let body = r#"{"token":"well-formed-but-not-inspected"}"#;
    let requests = [
        format!(
            "POST /api/auth/session HTTP/1.1\r\nHost: 127.0.0.1:8456\r\nOrigin: http://127.0.0.1:8456\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        ),
        "GET /api/auth/session HTTP/1.1\r\nHost: 127.0.0.1:8456\r\nCookie: acp_hub_session=opaque\r\nContent-Length: 0\r\n\r\n".to_string(),
    ];
    for request in requests {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let setup = test_auth_setup(dir.path());
        let server_auth = auth.clone();
        let server = tokio::spawn(async move {
            let (stream, peer) = listener.accept().await.unwrap();
            serve_http(stream, peer, server_auth, setup).await.unwrap();
        });
        let mut client = TcpStream::connect(addr).await.unwrap();
        client.write_all(request.as_bytes()).await.unwrap();
        let mut bytes = Vec::new();
        client.read_to_end(&mut bytes).await.unwrap();
        server.await.unwrap();
        let response = String::from_utf8(bytes).unwrap();

        assert!(response.starts_with("HTTP/1.1 503 Service Unavailable\r\n"));
        assert!(response.contains("Retry-After: 1\r\n"));
        assert!(response.contains("\"error\":\"auth_busy\""));
        assert!(response.contains("\"setup\":"));
        assert!(!response.contains("401 Unauthorized"));
    }
    drop(guard);
}

#[tokio::test]
async fn auth_http_rejects_ambiguous_request_framing() {
    let host = "127.0.0.1:8456";
    let origin = format!("http://{host}");
    let cases = [
        (
            "missing content type",
            format!(
                "POST /api/auth/session HTTP/1.1\r\nHost: {host}\r\nOrigin: {origin}\r\nContent-Length: 2\r\n\r\n{{}}"
            ),
            "415 Unsupported Media Type",
        ),
        (
            "form content type",
            format!(
                "POST /api/auth/session HTTP/1.1\r\nHost: {host}\r\nOrigin: {origin}\r\nContent-Type: application/x-www-form-urlencoded\r\nContent-Length: 3\r\n\r\na=b"
            ),
            "415 Unsupported Media Type",
        ),
        (
            "chunked body",
            format!(
                "POST /api/auth/session HTTP/1.1\r\nHost: {host}\r\nOrigin: {origin}\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n\r\n"
            ),
            "400 Bad Request",
        ),
        (
            "duplicate content length",
            format!(
                "POST /api/auth/session HTTP/1.1\r\nHost: {host}\r\nOrigin: {origin}\r\nContent-Type: application/json\r\nContent-Length: 2\r\nContent-Length: 2\r\n\r\n{{}}"
            ),
            "400 Bad Request",
        ),
        (
            "duplicate host",
            format!(
                "POST /api/auth/session HTTP/1.1\r\nHost: {host}\r\nHost: {host}\r\nOrigin: {origin}\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n{{}}"
            ),
            "400 Bad Request",
        ),
        (
            "obs-fold style header",
            format!(
                "POST /api/auth/session HTTP/1.1\r\nHost: {host}\r\n Origin: {origin}\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n{{}}"
            ),
            "400 Bad Request",
        ),
        (
            "http 1.0",
            format!(
                "POST /api/auth/session HTTP/1.0\r\nHost: {host}\r\nOrigin: {origin}\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n{{}}"
            ),
            "400 Bad Request",
        ),
        (
            "declared body shorter than bytes",
            format!(
                "POST /api/auth/session HTTP/1.1\r\nHost: {host}\r\nOrigin: {origin}\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n{{}}extra"
            ),
            "400 Bad Request",
        ),
        (
            "get body",
            format!(
                "GET /api/auth/session HTTP/1.1\r\nHost: {host}\r\nOrigin: {origin}\r\nContent-Length: 2\r\n\r\n{{}}"
            ),
            "400 Bad Request",
        ),
        (
            "invalid json",
            format!(
                "POST /api/auth/session HTTP/1.1\r\nHost: {host}\r\nOrigin: {origin}\r\nContent-Type: application/json\r\nContent-Length: 1\r\n\r\n{{"
            ),
            "400 Bad Request",
        ),
        (
            "unknown login field",
            format!(
                "POST /api/auth/session HTTP/1.1\r\nHost: {host}\r\nOrigin: {origin}\r\nContent-Type: application/json\r\nContent-Length: 23\r\n\r\n{{\"token\":\"x\",\"admin\":1}}"
            ),
            "400 Bad Request",
        ),
        (
            "oversized declared body",
            format!(
                "POST /api/auth/session HTTP/1.1\r\nHost: {host}\r\nOrigin: {origin}\r\nContent-Type: application/json\r\nContent-Length: 4097\r\n\r\n"
            ),
            "413 Payload Too Large",
        ),
    ];

    for (name, request, expected) in cases {
        let response = auth_socket_response(&request).await;
        assert!(
            response.starts_with(&format!("HTTP/1.1 {expected}\r\n")),
            "{name}: {response:?}"
        );
        assert!(response.contains("Cache-Control: no-store\r\n"), "{name}");
        assert!(response.contains("Pragma: no-cache\r\n"), "{name}");
        assert!(
            response.contains("X-Content-Type-Options: nosniff\r\n"),
            "{name}"
        );
    }
}

#[tokio::test]
async fn auth_http_accepts_fragmented_header_and_body_reads() {
    let body = b"{\"token\":\"invalid-but-well-formed\"}";
    let head = format!(
        "POST /api/auth/session HTTP/1.1\r\nHost: 127.0.0.1:8456\r\nOrigin: http://127.0.0.1:8456\r\nContent-Type: application/json; charset=utf-8\r\nContent-Length: {}\r\n\r\n",
        body.len()
    );
    let split = head.len() - 2;
    let response = auth_socket_response_fragments(&[
        &head.as_bytes()[..split],
        &head.as_bytes()[split..],
        &body[..5],
        &body[5..],
    ])
    .await;
    assert!(
        response.starts_with("HTTP/1.1 401 Unauthorized\r\n"),
        "合法分片请求应到达凭据裁决：{response:?}"
    );
}

#[tokio::test]
async fn auth_http_requires_origin_for_state_changes() {
    let host = "127.0.0.1:8456";
    for request in [
        format!(
            "POST /api/auth/session HTTP/1.1\r\nHost: {host}\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n{{}}"
        ),
        format!("DELETE /api/auth/session HTTP/1.1\r\nHost: {host}\r\nContent-Length: 0\r\n\r\n"),
    ] {
        let response = auth_socket_response(&request).await;
        assert!(
            response.starts_with("HTTP/1.1 403 Forbidden\r\n"),
            "{response:?}"
        );
    }
}

/// 路由表：面板为唯一页面（/、/index.html、旧链接 /panel.html 同源），
/// 资产表非空且含 js/css，未知路径 404。
#[test]
fn route_resolves_static_assets() {
    // Web 面板入口：/、/index.html 与 /panel.html（旧链接兼容）同源。
    let (name, ct, index) = route("/").expect("/ → index.html");
    assert_eq!(name, "index.html");
    assert_eq!(ct, "text/html; charset=utf-8");
    assert!(String::from_utf8_lossy(index).contains("acp-hub Web 面板"));
    assert_eq!(route("/index.html").map(|(n, _, _)| n), Some("index.html"));
    assert_eq!(route("/panel.html").map(|(n, _, _)| n), Some("index.html"));
    let (_, _, compat) = route("/panel.html").expect("/panel.html 兼容映射");
    assert_eq!(compat, index);

    // vite 产物：至少一个 js、一个 css（hash 文件名，不硬编码）。
    let js: Vec<_> = ASSETS.iter().filter(|a| a.url.ends_with(".js")).collect();
    let css: Vec<_> = ASSETS.iter().filter(|a| a.url.ends_with(".css")).collect();
    assert!(!js.is_empty(), "产物应含 js 资源");
    assert!(!css.is_empty(), "产物应含 css 资源");
    for a in js.iter().chain(css.iter()) {
        assert!(!a.bytes.is_empty(), "{} 应为非空", a.url);
    }

    // assets/ 前缀的产物路径可经 URL 命中（取第一个 js 走路由）。
    let first_js = js[0].url;
    let routed = route(&format!("/{}", first_js)).expect("assets 可经 URL 路由");
    assert_eq!(routed.0, first_js);

    assert_eq!(route("/instance"), None);
    assert_eq!(route("/favicon.ico"), None);
    assert_eq!(route("/visual-fixture.html"), None);
    for asset in ASSETS {
        assert!(
            !asset.url.contains("visual-fixture")
                && !String::from_utf8_lossy(asset.bytes).contains("UI 状态验收台"),
            "development fixture must not be embedded in production asset {}",
            asset.url,
        );
    }
}

/// Content-Type 按扩展名映射；未识别回 octet-stream。
#[test]
fn content_type_by_extension() {
    assert_eq!(content_type("index.html"), "text/html; charset=utf-8");
    assert_eq!(
        content_type("assets/app.js"),
        "text/javascript; charset=utf-8"
    );
    assert_eq!(content_type("assets/style.css"), "text/css; charset=utf-8");
    assert_eq!(content_type("icon.svg"), "image/svg+xml");
    assert_eq!(content_type("icon.png"), "image/png");
    assert_eq!(content_type("favicon.ico"), "image/x-icon");
    assert_eq!(content_type("font.woff2"), "font/woff2");
    assert_eq!(content_type("chunk.js.map"), "application/json");
    assert_eq!(content_type("blob.bin"), "application/octet-stream");
}

#[test]
fn static_cache_policy_separates_entry_documents_from_hashed_assets() {
    for path in ["/", "/index.html", "/panel.html", "/missing"] {
        assert_eq!(
            cache_headers_for_static(path, route(path).map(|(name, _, _)| name)),
            vec![("Cache-Control".into(), "no-store".into())],
            "entry/error path must be revalidated: {path}"
        );
    }
    let asset = ASSETS
        .iter()
        .find(|asset| asset.url.starts_with("assets/"))
        .expect("vite build should emit an asset");
    assert_eq!(
        cache_headers_for_static(&format!("/{}", asset.url), Some(asset.url)),
        vec![(
            "Cache-Control".into(),
            "public, max-age=31536000, immutable".into()
        )]
    );
    assert_eq!(
        cache_headers_for_static("/assets/missing.js", None),
        vec![("Cache-Control".into(), "no-store".into())]
    );
    for unhashed in ["assets/logo.svg", "assets/index-short.js", "public/app.css"] {
        assert_eq!(
            cache_headers_for_static(&format!("/{unhashed}"), Some(unhashed)),
            vec![("Cache-Control".into(), "no-store".into())],
            "fixed-name assets must remain upgrade-safe: {unhashed}"
        );
    }

    assert_eq!(
        cache_headers_for_static(
            "/assets/index-CBgKAe6-.css",
            Some("assets/index-CBgKAe6-.css")
        ),
        vec![(
            "Cache-Control".into(),
            "public, max-age=31536000, immutable".into()
        )],
        "Rollup URL-safe hashes may end in a dash"
    );
}

#[tokio::test]
async fn static_http_emits_upgrade_safe_cache_headers() {
    let entry = auth_socket_response("GET / HTTP/1.1\r\nHost: 127.0.0.1:8456\r\n\r\n").await;
    assert!(entry.starts_with("HTTP/1.1 200 OK\r\n"), "{entry:?}");
    assert!(entry.contains("Cache-Control: no-store\r\n"), "{entry:?}");
    assert!(
        entry.contains("X-Content-Type-Options: nosniff\r\n"),
        "{entry:?}"
    );

    let asset = ASSETS
        .iter()
        .find(|asset| asset.url.ends_with(".js"))
        .unwrap();
    let response = auth_socket_response(&format!(
        "GET /{} HTTP/1.1\r\nHost: 127.0.0.1:8456\r\n\r\n",
        asset.url
    ))
    .await;
    assert!(response.starts_with("HTTP/1.1 200 OK\r\n"), "{response:?}");
    assert!(
        response.contains("Cache-Control: public, max-age=31536000, immutable\r\n"),
        "{response:?}"
    );

    let missing =
        auth_socket_response("GET /assets/missing.js HTTP/1.1\r\nHost: 127.0.0.1:8456\r\n\r\n")
            .await;
    assert!(
        missing.starts_with("HTTP/1.1 404 Not Found\r\n"),
        "{missing:?}"
    );
    assert!(
        missing.contains("Cache-Control: no-store\r\n"),
        "{missing:?}"
    );
}

#[tokio::test]
async fn static_head_mirrors_get_headers_without_a_body() {
    let index = route("/").unwrap().2;
    let response = auth_socket_response("HEAD / HTTP/1.1\r\nHost: 127.0.0.1:8456\r\n\r\n").await;
    assert!(response.starts_with("HTTP/1.1 200 OK\r\n"), "{response:?}");
    assert!(
        response.contains(&format!("Content-Length: {}\r\n", index.len())),
        "{response:?}"
    );
    assert!(
        response.contains("Cache-Control: no-store\r\n"),
        "{response:?}"
    );
    assert!(
        response.ends_with("\r\n\r\n"),
        "HEAD must not emit a body: {response:?}"
    );

    let asset = ASSETS
        .iter()
        .find(|asset| asset.url.ends_with(".css"))
        .unwrap();
    let asset_response = auth_socket_response(&format!(
        "HEAD /{} HTTP/1.1\r\nHost: 127.0.0.1:8456\r\n\r\n",
        asset.url
    ))
    .await;
    assert!(asset_response.starts_with("HTTP/1.1 200 OK\r\n"));
    assert!(asset_response.contains(&format!("Content-Length: {}\r\n", asset.bytes.len())));
    assert!(asset_response.contains("Cache-Control: public, max-age=31536000, immutable\r\n"));
    assert!(asset_response.ends_with("\r\n\r\n"));
}

/// socket 端到端：GET / → 200 + html Content-Type + 首页内容（read_to_end
/// 依赖 `Connection: close` + shutdown 产生 EOF）。
#[tokio::test]
async fn serve_returns_index() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        serve(stream, "GET / HTTP/1.1\r\nHost: test\r\n\r\n")
            .await
            .unwrap();
    });
    let mut client = TcpStream::connect(addr).await.unwrap();
    let mut buf = Vec::new();
    client.read_to_end(&mut buf).await.unwrap();
    server.await.unwrap();

    let text = String::from_utf8(buf).unwrap();
    assert!(text.starts_with("HTTP/1.1 200 OK\r\n"), "{text:?}");
    assert!(
        text.contains("Content-Type: text/html; charset=utf-8"),
        "{text:?}"
    );
    assert!(text.contains("acp-hub Web 面板"), "{text:?}");
    assert!(text.contains("</html>"), "{text:?}");
}

/// socket 端到端：未知路径 → 404（含 Content-Length，体为纯文本）。
#[tokio::test]
async fn serve_returns_404() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        serve(
            stream,
            "GET /no-such-resource HTTP/1.1\r\nHost: test\r\n\r\n",
        )
        .await
        .unwrap();
    });
    let mut client = TcpStream::connect(addr).await.unwrap();
    let mut buf = Vec::new();
    client.read_to_end(&mut buf).await.unwrap();
    server.await.unwrap();

    let text = String::from_utf8(buf).unwrap();
    assert!(text.starts_with("HTTP/1.1 404 Not Found\r\n"), "{text:?}");
    assert!(text.contains("Content-Length: 14"), "{text:?}");
    assert!(text.ends_with("404 Not Found\n"), "{text:?}");
}

/// socket 端到端：query 路径同样命中（浏览器缓存失效参数）。
#[tokio::test]
async fn serve_handles_query() {
    // 取资产表中第一个 js 作为探测目标（hash 文件名不硬编码）。
    let js = ASSETS
        .iter()
        .find(|a| a.url.ends_with(".js"))
        .expect("有 js 产物");
    let path = format!("/{}?t=1", js.url);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        serve(
            stream,
            &format!("GET {path} HTTP/1.1\r\nHost: test\r\n\r\n"),
        )
        .await
        .unwrap();
    });
    let mut client = TcpStream::connect(addr).await.unwrap();
    let mut buf = Vec::new();
    client.read_to_end(&mut buf).await.unwrap();
    server.await.unwrap();

    let text = String::from_utf8(buf).unwrap();
    assert!(text.starts_with("HTTP/1.1 200 OK\r\n"), "{text:?}");
    assert!(
        text.contains("Content-Type: text/javascript; charset=utf-8"),
        "{text:?}"
    );
    // 响应体与内嵌字节一致（Content-Length 精确匹配）。
    let body_start = text.find("\r\n\r\n").map(|i| i + 4).expect("有头部结束符");
    assert_eq!(&text.as_bytes()[body_start..], js.bytes);
}
