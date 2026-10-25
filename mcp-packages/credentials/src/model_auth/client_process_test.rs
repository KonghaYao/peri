//! Real process boundaries, MCP requests and an offline HTTP issuer.
#![cfg(unix)]

use std::{
    collections::HashMap,
    process::{Child, Command, Stdio},
    sync::Mutex,
};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use serde_json::{json, Value};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

use super::*;
use crate::model_auth::{
    types::{StoredAccount, TokenSet},
    RESOURCE, SCOPES,
};

const CHILD_TEST: &str = "model_auth::client::process_tests::cross_process_auth_helper";

type RecordedRequest = (String, HashMap<String, String>);

struct MockIssuer {
    base: url::Url,
    requests: Arc<Mutex<Vec<RecordedRequest>>>,
    nonce: Arc<Mutex<String>>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for MockIssuer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl MockIssuer {
    async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = url::Url::parse(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
        let issuer = base.as_str().trim_end_matches('/').to_owned();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = requests.clone();
        let nonce = Arc::new(Mutex::new(String::new()));
        let current_nonce = nonce.clone();
        let task = tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut bytes = Vec::new();
                while !bytes.ends_with(b"\r\n\r\n") {
                    bytes.push(socket.read_u8().await.unwrap());
                    assert!(bytes.len() < 32 * 1024);
                }
                let headers = String::from_utf8(bytes).unwrap();
                let path = headers.split_whitespace().nth(1).unwrap().to_owned();
                let length: usize = headers
                    .lines()
                    .filter_map(|line| line.split_once(':'))
                    .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                    .map(|(_, value)| value.trim().parse().unwrap())
                    .unwrap_or(0);
                let mut body = vec![0; length];
                socket.read_exact(&mut body).await.unwrap();
                let form: HashMap<String, String> =
                    url::form_urlencoded::parse(&body).into_owned().collect();
                captured.lock().unwrap().push((path.clone(), form.clone()));
                let value: Value = match path.as_str() {
                    "/api/accounts/oauth/token" => {
                        assert_eq!(
                            form.get("client_id").map(String::as_str),
                            Some("process-client")
                        );
                        assert_eq!(form.get("resource").map(String::as_str), Some(RESOURCE));
                        if form.get("grant_type").map(String::as_str) == Some("authorization_code")
                        {
                            assert_eq!(
                                form.get("code").map(String::as_str),
                                Some("process-login-code")
                            );
                            json!({"access_token": "process-login-access", "refresh_token": "process-login-refresh", "id_token": sign(&issuer, &current_nonce.lock().unwrap()), "scope": SCOPES, "token_type": "Bearer", "expires_in": 3600})
                        } else {
                            assert_eq!(
                                form.get("grant_type").map(String::as_str),
                                Some("refresh_token")
                            );
                            assert_eq!(
                                form.get("refresh_token").map(String::as_str),
                                Some("process-refresh-initial")
                            );
                            assert!(!form.contains_key("scope"));
                            json!({"access_token": "process-access-rotated", "refresh_token": "process-refresh-rotated", "token_type": "Bearer", "expires_in": 3600})
                        }
                    }
                    "/.well-known/openid-configuration" => {
                        json!({"issuer": issuer, "jwks_uri": format!("{issuer}/jwks"), "revocation_endpoint": format!("{issuer}/revoke")})
                    }
                    "/jwks" => {
                        serde_json::from_str(include_str!("fixtures/test-only-jwks.json")).unwrap()
                    }
                    "/revoke" => {
                        assert_eq!(
                            form.get("token").map(String::as_str),
                            Some("process-refresh-initial")
                        );
                        assert_eq!(
                            form.get("client_id").map(String::as_str),
                            Some("process-client")
                        );
                        json!({})
                    }
                    _ => panic!("unexpected fixture route {path}"),
                };
                let bytes = serde_json::to_vec(&value).unwrap();
                let header = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", bytes.len());
                socket.write_all(header.as_bytes()).await.unwrap();
                socket.write_all(&bytes).await.unwrap();
                socket.shutdown().await.unwrap();
            }
        });
        Self {
            base,
            requests,
            nonce,
            task,
        }
    }
}

fn sign(issuer: &str, nonce: &str) -> String {
    let key = ring::signature::RsaKeyPair::from_pkcs8(include_bytes!("fixtures/test-only-rsa.pk8"))
        .unwrap();
    let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"RS256","kid":"peri-synthetic-test-key"}"#);
    let claims = json!({"iss": issuer, "sub": "process-subject", "aud": "process-client", "exp": 5000, "iat": 900, "nonce": nonce});
    let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap());
    let input = format!("{header}.{payload}");
    let mut signature = vec![0; key.public().modulus_len()];
    key.sign(
        &ring::signature::RSA_PKCS1_SHA256,
        &ring::rand::SystemRandom::new(),
        input.as_bytes(),
        &mut signature,
    )
    .unwrap();
    format!("{input}.{}", URL_SAFE_NO_PAD.encode(signature))
}

fn manager(root: &std::path::Path, base: url::Url) -> ChatGptAuthManager {
    let mut value = ChatGptAuthManager::new(root.to_owned(), "one".into()).unwrap();
    let server = Arc::get_mut(&mut value.server).unwrap();
    server.oauth = OAuth::for_test(base);
    server.clock = Arc::new(|| 1000);
    value.with_expected_identity("process-subject".into(), "process-client".into())
}

async fn seed(value: &ChatGptAuthManager, expires_at: u64) {
    let cancellation = CancellationToken::new();
    let host = value.server.store.host_id(&cancellation).await.unwrap();
    let lease = value.server.store.lease(&cancellation).await.unwrap();
    let mut record = StoredAccount::new(host);
    record.client_id = Some("process-client".into());
    record.subject = Some("process-subject".into());
    record.binding = Some("process-generation".into());
    record.tokens = Some(TokenSet {
        access_token: "process-access-initial".into(),
        refresh_token: "process-refresh-initial".into(),
        id_token: "process-id-token-preserved".into(),
        expires_at,
        scope: SCOPES.into(),
        version: "process-initial-version".into(),
    });
    lease.save(record).await.unwrap();
}

struct Process(Option<Child>);

impl Process {
    fn spawn(root: &std::path::Path, issuer: &url::Url, operation: &str) -> Self {
        Self(Some(
            Command::new(std::env::current_exe().unwrap())
                .args(["--exact", CHILD_TEST, "--nocapture"])
                .env("PERI_TEST_AUTH_PROCESS_ROOT", root)
                .env("PERI_TEST_AUTH_PROCESS_ISSUER", issuer.as_str())
                .env("PERI_TEST_AUTH_PROCESS_OPERATION", operation)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap(),
        ))
    }

    async fn success(mut self) {
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                if self.0.as_mut().unwrap().try_wait().unwrap().is_some() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("auth subprocess must finish");
        let output = self.0.take().unwrap().wait_with_output().unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "auth child failed: {stdout}\n{stderr}"
        );
        assert!(
            stdout.contains("1 passed"),
            "child filter must execute one real helper: {stdout}"
        );
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        if let Some(child) = &mut self.0 {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

#[tokio::test]
async fn two_real_processes_rotate_one_expired_refresh_token_exactly_once() {
    let issuer = MockIssuer::start().await;
    let directory = tempfile::tempdir().unwrap();
    let store = directory.path().join("credentials");
    let parent = manager(&store, issuer.base.clone());
    seed(&parent, 0).await;
    let first = Process::spawn(&store, &issuer.base, "credential");
    let second = Process::spawn(&store, &issuer.base, "credential");
    tokio::join!(first.success(), second.success());
    {
        let requests = issuer.requests.lock().unwrap();
        assert_eq!(
            requests.len(),
            1,
            "only one process may spend the old rotating refresh token"
        );
        assert_eq!(requests[0].0, "/api/accounts/oauth/token");
    }
    let record = parent
        .server
        .store
        .lease(&CancellationToken::new())
        .await
        .unwrap()
        .read()
        .await
        .unwrap()
        .unwrap();
    let tokens = record.tokens.unwrap();
    assert_eq!(tokens.access_token, "process-access-rotated");
    assert_eq!(tokens.refresh_token, "process-refresh-rotated");
    assert_eq!(tokens.id_token, "process-id-token-preserved");
    assert_eq!(tokens.scope, SCOPES);
}

#[tokio::test]
async fn logout_in_another_process_invalidates_next_bound_credential_request() {
    let issuer = MockIssuer::start().await;
    let directory = tempfile::tempdir().unwrap();
    let store = directory.path().join("credentials");
    let parent = manager(&store, issuer.base.clone());
    seed(&parent, 5000).await;
    assert_eq!(
        parent
            .credential(CancellationToken::new())
            .await
            .unwrap()
            .bearer(),
        "process-access-initial"
    );
    Process::spawn(&store, &issuer.base, "logout")
        .success()
        .await;
    let error = parent
        .credential(CancellationToken::new())
        .await
        .unwrap_err();
    assert!(error.is_terminal_provider_failure());
    let record = parent
        .server
        .store
        .lease(&CancellationToken::new())
        .await
        .unwrap()
        .read()
        .await
        .unwrap()
        .unwrap();
    assert!(record.tokens.is_none());
    let requests = issuer.requests.lock().unwrap();
    assert_eq!(
        requests
            .iter()
            .filter(|(path, _)| path == "/revoke")
            .count(),
        1
    );
    assert_eq!(
        requests
            .iter()
            .filter(|(path, _)| path == "/api/accounts/oauth/token")
            .count(),
        0
    );
}

#[tokio::test]
async fn logout_in_another_process_fences_an_already_pending_first_login() {
    let issuer = MockIssuer::start().await;
    let directory = tempfile::tempdir().unwrap();
    let store = directory.path().join("credentials");
    let parent = manager(&store, issuer.base.clone());
    let pending = parent.begin_login().await.unwrap();
    let authorize = url::Url::parse(pending.authorization_url()).unwrap();
    let fields: HashMap<_, _> = authorize.query_pairs().into_owned().collect();
    *issuer.nonce.lock().unwrap() = fields["nonce"].clone();
    Process::spawn(&store, &issuer.base, "logout")
        .success()
        .await;
    let mut callback = url::Url::parse(&fields["redirect_uri"]).unwrap();
    callback
        .query_pairs_mut()
        .append_pair("state", &fields["state"])
        .append_pair("code", "process-login-code")
        .append_pair("client_id", "process-client");
    let (result, response) = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::join!(
            parent.finish_login(pending, CancellationToken::new()),
            reqwest::get(callback)
        )
    })
    .await
    .unwrap();
    assert_eq!(response.unwrap().status(), 200);
    assert_eq!(result.unwrap_err(), AuthError::AccountChanged);
    let record = parent
        .server
        .store
        .lease(&CancellationToken::new())
        .await
        .unwrap()
        .read()
        .await
        .unwrap()
        .unwrap();
    assert!(
        record.tokens.is_none(),
        "logout tombstone must survive late successful OAuth exchange"
    );
    let requests = issuer.requests.lock().unwrap();
    assert_eq!(
        requests
            .iter()
            .filter(|(path, _)| path == "/api/accounts/oauth/token")
            .count(),
        1,
        "exercise exchange and verified JWT before the store CAS rejects the stale login"
    );
    assert_eq!(
        requests.iter().filter(|(path, _)| path == "/jwks").count(),
        1
    );
}

#[tokio::test]
async fn cross_process_auth_helper() {
    let Some(root) = std::env::var_os("PERI_TEST_AUTH_PROCESS_ROOT") else {
        return;
    };
    let base = std::env::var("PERI_TEST_AUTH_PROCESS_ISSUER")
        .unwrap()
        .parse()
        .unwrap();
    let value = manager(std::path::Path::new(&root), base);
    match std::env::var("PERI_TEST_AUTH_PROCESS_OPERATION")
        .unwrap()
        .as_str()
    {
        "credential" => assert_eq!(
            value
                .credential(CancellationToken::new())
                .await
                .unwrap()
                .bearer(),
            "process-access-rotated"
        ),
        "logout" => {
            let outcome = value.logout(CancellationToken::new()).await.unwrap();
            assert!(outcome.local_cleared && outcome.remote_revoked);
        }
        operation => panic!("unknown fixture operation {operation}"),
    }
}
