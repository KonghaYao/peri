use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Mutex,
    },
};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use serde_json::json;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::Notify,
};

use super::*;
use crate::model_auth::{
    types::{StoredAccount, TokenSet},
    RESOURCE,
};

#[path = "client_trace_test.rs"]
mod trace_tests;

#[derive(Clone, Copy)]
enum Mode {
    Normal,
    InvalidGrant,
    Temporary,
    MissingScope,
    ChangedIdentity,
    Pending,
}

struct MockState {
    refreshes: AtomicUsize,
    nonce: Mutex<String>,
    challenge: Mutex<String>,
    mode: Mutex<Mode>,
    requests: Mutex<Vec<(String, String)>>,
    pending_refresh: Notify,
    pending_revoke: Notify,
    block_revoke: std::sync::atomic::AtomicBool,
}

struct MockIssuer {
    base: url::Url,
    state: Arc<MockState>,
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
        let state = Arc::new(MockState {
            refreshes: AtomicUsize::new(0),
            nonce: Mutex::new(String::new()),
            challenge: Mutex::new(String::new()),
            mode: Mutex::new(Mode::Normal),
            requests: Mutex::new(Vec::new()),
            pending_refresh: Notify::new(),
            pending_revoke: Notify::new(),
            block_revoke: std::sync::atomic::AtomicBool::new(false),
        });
        let shared = state.clone();
        let issuer = base.as_str().trim_end_matches('/').to_owned();
        let task = tokio::spawn(async move {
            let mut requests = tokio::task::JoinSet::new();
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let shared = shared.clone();
                let issuer = issuer.clone();
                requests.spawn(async move {
                    let mut bytes = Vec::new();
                    while !bytes.ends_with(b"\r\n\r\n") {
                        bytes.push(socket.read_u8().await.unwrap());
                        assert!(bytes.len() < 32 * 1024);
                    }
                    let headers = String::from_utf8(bytes).unwrap();
                    let path = headers.split_whitespace().nth(1).unwrap().to_owned();
                    let len: usize = headers.lines().filter_map(|line| line.split_once(':'))
                        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                        .map(|(_, value)| value.trim().parse().unwrap()).unwrap_or(0);
                    let mut body = vec![0; len];
                    socket.read_exact(&mut body).await.unwrap();
                    let body = String::from_utf8(body).unwrap();
                    shared.requests.lock().unwrap().push((path.clone(), body.clone()));
                    let (status, value) = match path.as_str() {
                        "/.well-known/openid-configuration" => (200, json!({"issuer":issuer,"jwks_uri":format!("{issuer}/jwks"),"revocation_endpoint":format!("{issuer}/revoke")})),
                        "/jwks" => (200, serde_json::from_str(include_str!("fixtures/test-only-jwks.json")).unwrap()),
                        "/revoke" => {
                            if shared.block_revoke.load(Ordering::SeqCst) {
                                shared.pending_revoke.notify_one();
                                std::future::pending::<()>().await;
                            }
                            (200, json!({}))
                        }
                        "/api/accounts/oauth/token" => {
                            let form: HashMap<_, _> = url::form_urlencoded::parse(body.as_bytes()).into_owned().collect();
                            assert_eq!(form.get("client_id").map(String::as_str), Some("issued-client"));
                            assert_eq!(form.get("resource").map(String::as_str), Some(RESOURCE));
                            if form["grant_type"] == "authorization_code" {
                                assert_eq!(form["code"], "synthetic-code");
                                assert_eq!(URL_SAFE_NO_PAD.encode(Sha256::digest(form["code_verifier"].as_bytes())), *shared.challenge.lock().unwrap());
                                let scope = if matches!(*shared.mode.lock().unwrap(), Mode::MissingScope) { "openid profile email" } else { crate::model_auth::SCOPES };
                                let nonce = shared.nonce.lock().unwrap().clone();
                                (200, json!({"access_token":"synthetic-access-initial","refresh_token":"synthetic-refresh-initial","id_token":sign(&issuer,"subject-one",Some(&nonce)),"token_type":"Bearer","expires_in":3600,"scope":scope}))
                            } else {
                                shared.refreshes.fetch_add(1, Ordering::SeqCst);
                                assert!(!form.contains_key("scope"));
                                let mode = *shared.mode.lock().unwrap();
                                match mode {
                                    Mode::InvalidGrant => (400, json!({"error":"invalid_grant","error_description":"synthetic OAuth provider diagnosis"})),
                                    Mode::Temporary => (503, json!({"error":"temporarily_unavailable","error_description":"synthetic OAuth provider diagnosis"})),
                                    Mode::Pending => {
                                        shared.pending_refresh.notify_one();
                                        std::future::pending::<()>().await;
                                        unreachable!()
                                    }
                                    _ => {
                                        let mut value = json!({"access_token":"synthetic-access-rotated","refresh_token":"synthetic-refresh-rotated","token_type":"Bearer","expires_in":3600});
                                        if matches!(mode, Mode::MissingScope) { value["scope"] = json!("openid"); }
                                        if matches!(mode, Mode::ChangedIdentity) { value["id_token"] = json!(sign(&issuer,"other-subject",None)); }
                                        (200, value)
                                    }
                                }
                            }
                        }
                        _ => panic!("Unexpected mock route {path}"),
                    };
                    let payload = serde_json::to_vec(&value).unwrap();
                    let response = format!("HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", payload.len());
                    let _ = socket.write_all(response.as_bytes()).await;
                    let _ = socket.write_all(&payload).await;
                });
            }
        });
        Self { base, state, task }
    }

    fn manager(&self, dir: &std::path::Path, account: &str) -> ChatGptAuthManager {
        let mut manager =
            ChatGptAuthManager::new(dir.join("credentials"), account.to_owned()).unwrap();
        let server = Arc::get_mut(&mut manager.server).unwrap();
        server.oauth = OAuth::for_test(self.base.clone());
        server.clock = Arc::new(|| 1000);
        manager.with_expected_identity("subject-one".into(), "issued-client".into())
    }

    async fn pending(&self, manager: &ChatGptAuthManager) -> (PendingLogin, url::Url) {
        let pending = manager.begin_login(CancellationToken::new()).await.unwrap();
        let url = url::Url::parse(pending.authorization_url()).unwrap();
        let query: HashMap<_, _> = url.query_pairs().into_owned().collect();
        *self.state.nonce.lock().unwrap() = query["nonce"].clone();
        *self.state.challenge.lock().unwrap() = query["code_challenge"].clone();
        let mut callback = url::Url::parse(&query["redirect_uri"]).unwrap();
        callback
            .query_pairs_mut()
            .append_pair("state", &query["state"])
            .append_pair("code", "synthetic-code")
            .append_pair("client_id", "issued-client");
        (pending, callback)
    }

    async fn finish(
        &self,
        manager: &ChatGptAuthManager,
        pending: PendingLogin,
        callback: url::Url,
    ) -> AuthResult<AccountStatus> {
        let finishing = manager.finish_login(pending, CancellationToken::new());
        let callback = async { reqwest::get(callback).await.unwrap().status() };
        let (result, status) = tokio::join!(finishing, callback);
        assert_eq!(status, 200);
        result
    }

    async fn login(&self, manager: &ChatGptAuthManager) -> AccountStatus {
        let (pending, callback) = self.pending(manager).await;
        self.finish(manager, pending, callback).await.unwrap()
    }
}

fn sign(issuer: &str, subject: &str, nonce: Option<&str>) -> String {
    let key = ring::signature::RsaKeyPair::from_pkcs8(include_bytes!("fixtures/test-only-rsa.pk8"))
        .unwrap();
    let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"RS256","kid":"peri-synthetic-test-key"}"#);
    let claims = json!({"iss":issuer,"sub":subject,"aud":"issued-client","exp":5000,"iat":900,"nonce":nonce,"email":"synthetic@example.test"});
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

async fn seed(manager: &ChatGptAuthManager, expires_at: u64) {
    let cancellation = CancellationToken::new();
    let host = manager.server.store.host_id(&cancellation).await.unwrap();
    let lease = manager.server.store.lease(&cancellation).await.unwrap();
    let mut record = StoredAccount::new(host);
    record.client_id = Some("issued-client".into());
    record.subject = Some("subject-one".into());
    record.binding = Some("synthetic-session-generation".into());
    record.tokens = Some(TokenSet {
        access_token: "synthetic-access-initial".into(),
        refresh_token: "synthetic-refresh-initial".into(),
        id_token: "synthetic-stored-id-token".into(),
        expires_at,
        scope: crate::model_auth::SCOPES.into(),
        version: "synthetic-initial-version".into(),
    });
    lease.save(record).await.unwrap();
}

async fn stored(manager: &ChatGptAuthManager) -> StoredAccount {
    manager
        .server
        .store
        .lease(&CancellationToken::new())
        .await
        .unwrap()
        .read()
        .await
        .unwrap()
        .unwrap()
}

#[tokio::test]
async fn full_pkce_login_mcp_persistence_restart_refresh_and_logout() {
    let mock = MockIssuer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let manager = mock.manager(dir.path(), "host");
    let status = mock.login(&manager).await;
    assert_eq!(status.subject, "subject-one");
    assert_eq!(status.expires_at, 4600);
    assert!(status.plan_enabled);
    let status_json = serde_json::to_string(&status).unwrap();
    assert!(!status_json.contains("access_token") && !status_json.contains("refresh_token"));
    drop(manager);
    let restarted = mock.manager(dir.path(), "host");
    let initial = restarted
        .credential(CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(initial.bearer(), "synthetic-access-initial");
    assert!(restarted
        .refresh_after_unauthorized(&initial, CancellationToken::new())
        .await
        .unwrap());
    let current = restarted
        .credential(CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(current.bearer(), "synthetic-access-rotated");
    let record = stored(&restarted).await;
    let tokens = record.tokens.unwrap();
    assert_eq!(tokens.refresh_token, "synthetic-refresh-rotated");
    assert_eq!(tokens.scope, crate::model_auth::SCOPES);
    assert!(!tokens.id_token.is_empty());
    let outcome = restarted.logout(CancellationToken::new()).await.unwrap();
    assert!(outcome.local_cleared && outcome.remote_revoked);
    assert!(stored(&restarted).await.tokens.is_none());
    assert!(restarted
        .credential(CancellationToken::new())
        .await
        .is_err());
    assert_eq!(mock.state.refreshes.load(Ordering::SeqCst), 1);
    let requests = mock.state.requests.lock().unwrap();
    let revoked = requests.iter().find(|(path, _)| path == "/revoke").unwrap();
    assert!(revoked.1.contains("synthetic-refresh-rotated"));
}

#[tokio::test]
async fn concurrent_managers_refresh_one_rotating_token_once() {
    let mock = MockIssuer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let first = mock.manager(dir.path(), "one");
    let second = mock.manager(dir.path(), "one");
    seed(&first, 0).await;
    let (one, two) = tokio::join!(
        first.credential(CancellationToken::new()),
        second.credential(CancellationToken::new())
    );
    assert_eq!(one.unwrap().bearer(), "synthetic-access-rotated");
    assert_eq!(two.unwrap().bearer(), "synthetic-access-rotated");
    assert_eq!(mock.state.refreshes.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn stale_401_version_uses_other_refresh_without_reusing_old_token() {
    let mock = MockIssuer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let first = mock.manager(dir.path(), "one");
    let second = mock.manager(dir.path(), "one");
    seed(&first, 5000).await;
    let one = first.credential(CancellationToken::new()).await.unwrap();
    let two = second.credential(CancellationToken::new()).await.unwrap();
    let (one, two) = tokio::join!(
        first.refresh_after_unauthorized(&one, CancellationToken::new()),
        second.refresh_after_unauthorized(&two, CancellationToken::new())
    );
    assert!(one.unwrap() && two.unwrap());
    assert_eq!(mock.state.refreshes.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn permanent_refresh_failure_clears_tokens_but_transient_failure_preserves_them() {
    let mock = MockIssuer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let manager = mock.manager(dir.path(), "one");
    seed(&manager, 0).await;
    *mock.state.mode.lock().unwrap() = Mode::Temporary;
    let error = manager
        .credential(CancellationToken::new())
        .await
        .unwrap_err();
    assert!(error.to_string().contains("temporarily_unavailable"));
    assert!(stored(&manager).await.tokens.is_some());
    *mock.state.mode.lock().unwrap() = Mode::InvalidGrant;
    let error = manager
        .credential(CancellationToken::new())
        .await
        .unwrap_err();
    assert!(error.to_string().contains("invalid_grant"));
    assert!(stored(&manager).await.tokens.is_none());
}

#[tokio::test]
async fn login_without_granted_plan_scope_never_persists_tokens() {
    let mock = MockIssuer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let manager = mock.manager(dir.path(), "one");
    *mock.state.mode.lock().unwrap() = Mode::MissingScope;
    let (pending, callback) = mock.pending(&manager).await;
    assert_eq!(
        mock.finish(&manager, pending, callback).await.unwrap_err(),
        AuthError::MissingScope
    );
    assert!(stored(&manager).await.tokens.is_none());
}

#[tokio::test]
async fn refresh_cannot_change_subject_and_missing_grant_invalidates_spent_tokens() {
    let mock = MockIssuer::start().await;
    let dir = tempfile::tempdir().unwrap();
    for (name, mode, message) in [
        (
            "identity",
            Mode::ChangedIdentity,
            "identity or login session changed",
        ),
        ("scope", Mode::MissingScope, "did not grant"),
    ] {
        let manager = mock.manager(dir.path(), name);
        seed(&manager, 0).await;
        *mock.state.mode.lock().unwrap() = mode;
        assert!(manager
            .credential(CancellationToken::new())
            .await
            .unwrap_err()
            .to_string()
            .contains(message));
        assert!(stored(&manager).await.tokens.is_none());
    }
}

#[tokio::test]
async fn configured_identity_prevents_alias_account_switch_before_any_http() {
    let mock = MockIssuer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let manager = mock.manager(dir.path(), "one");
    seed(&manager, 5000).await;
    let wrong = manager
        .clone()
        .with_expected_identity("different-subject".into(), "issued-client".into());
    assert!(wrong
        .credential(CancellationToken::new())
        .await
        .unwrap_err()
        .to_string()
        .contains("identity or login session changed"));
    assert!(mock.state.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn independent_logout_tombstone_prevents_late_first_login_commit() {
    let mock = MockIssuer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let signing_in = mock.manager(dir.path(), "one");
    let other_process = mock.manager(dir.path(), "one");
    let (pending, callback) = mock.pending(&signing_in).await;
    other_process
        .logout(CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(
        mock.finish(&signing_in, pending, callback)
            .await
            .unwrap_err(),
        AuthError::AccountChanged
    );
    assert!(stored(&signing_in).await.tokens.is_none());
}

#[tokio::test]
async fn cancellation_stops_pending_refresh_without_poisoning_lock() {
    let mock = MockIssuer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let manager = mock.manager(dir.path(), "one");
    seed(&manager, 0).await;
    *mock.state.mode.lock().unwrap() = Mode::Pending;
    let cancellation = CancellationToken::new();
    let work = manager.credential(cancellation.clone());
    let cancel = async {
        mock.state.pending_refresh.notified().await;
        cancellation.cancel();
    };
    let (result, ()) = tokio::join!(work, cancel);
    assert!(result.unwrap_err().is_cancelled());
    *mock.state.mode.lock().unwrap() = Mode::Normal;
    assert_eq!(
        manager
            .credential(CancellationToken::new())
            .await
            .unwrap()
            .bearer(),
        "synthetic-access-rotated"
    );
}

#[tokio::test]
async fn cancelling_remote_revocation_after_start_preserves_local_logout() {
    let mock = MockIssuer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let manager = mock.manager(dir.path(), "one");
    seed(&manager, 5000).await;
    mock.state.block_revoke.store(true, Ordering::SeqCst);
    let cancellation = CancellationToken::new();
    let work = manager.logout(cancellation.clone());
    let cancel = async {
        mock.state.pending_revoke.notified().await;
        cancellation.cancel();
    };
    let (result, ()) = tokio::join!(work, cancel);
    let outcome = result.unwrap();
    assert!(outcome.local_cleared && !outcome.remote_revoked);
    assert!(stored(&manager).await.tokens.is_none());
}

#[test]
fn credential_identity_is_stable_and_endpoint_policy_is_independent_of_profile() {
    let dir = tempfile::tempdir().unwrap();
    let manager = ChatGptAuthManager::new(dir.path().to_owned(), "one".into())
        .unwrap()
        .with_expected_identity("subject".into(), "client".into());
    assert_eq!(
        manager.credential_ref(),
        chatgpt_credential_ref("subject", "client")
    );
    assert_ne!(
        manager.credential_ref(),
        chatgpt_credential_ref("different", "client")
    );
    assert!(manager
        .validate_endpoint(&"https://api.openai.com/v1/responses".parse().unwrap())
        .is_ok());
    for endpoint in [
        "https://evil.test/v1/responses",
        "http://api.openai.com/v1/responses",
        "https://api.openai.com/v1/responses?evil=true",
        "https://chatgpt.com/backend-api/codex/responses",
    ] {
        assert!(
            manager
                .validate_endpoint(&endpoint.parse().unwrap())
                .is_err(),
            "{endpoint}"
        );
    }
}

// The only local-endpoint exception is a test wrapper. Production manager
// validate_endpoint never permits this mock inference route.
struct LocalInferenceAuth {
    manager: ChatGptAuthManager,
    endpoint: url::Url,
}

#[async_trait::async_trait]
impl RequestAuth for LocalInferenceAuth {
    fn credential_ref(&self) -> &str {
        self.manager.credential_ref()
    }
    fn validate_endpoint(&self, endpoint: &url::Url) -> ModelResult<()> {
        assert_eq!(endpoint, &self.endpoint);
        Ok(())
    }
    async fn credential(&self, cancellation: CancellationToken) -> ModelResult<RequestCredential> {
        self.manager.credential(cancellation).await
    }
    async fn refresh_after_unauthorized(
        &self,
        rejected: &RequestCredential,
        cancellation: CancellationToken,
    ) -> ModelResult<bool> {
        self.manager
            .refresh_after_unauthorized(rejected, cancellation)
            .await
    }
    fn cancellation_token(&self) -> Option<CancellationToken> {
        self.manager.cancellation_token()
    }
}

async fn inference_server(
    statuses: Vec<u16>,
) -> (url::Url, tokio::task::JoinHandle<Vec<(String, Vec<u8>)>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1/responses", listener.local_addr().unwrap())
        .parse()
        .unwrap();
    let task = tokio::spawn(async move {
        let mut requests = Vec::new();
        for status in statuses {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            while !bytes.ends_with(b"\r\n\r\n") {
                bytes.push(socket.read_u8().await.unwrap());
            }
            let headers = String::from_utf8(bytes).unwrap();
            let len: usize = headers
                .lines()
                .filter_map(|line| line.split_once(':'))
                .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                .unwrap()
                .1
                .trim()
                .parse()
                .unwrap();
            let mut body = vec![0; len];
            socket.read_exact(&mut body).await.unwrap();
            requests.push((headers, body));
            let body = if status == 200 {
                format!(
                    "event: response.completed\ndata: {}\n\n",
                    json!({"type":"response.completed","response":{"id":"synthetic-response","status":"completed","output":[{"type":"message","id":"synthetic-message","role":"assistant","content":[{"type":"output_text","text":"ok"}]}]}})
                )
            } else {
                json!({"error":{"code":"invalid_api_key","message":"synthetic rejected access token"}}).to_string()
            };
            let response = format!("HTTP/1.1 {status} Test\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
            socket.write_all(response.as_bytes()).await.unwrap();
        }
        requests
    });
    (url, task)
}

#[tokio::test]
async fn real_http_401_refresh_replays_frozen_body_with_rotated_bearer() {
    use peri_model::{
        openai_responses::{ResponsesConfig, ResponsesModel},
        Model, ModelMessage, ModelRequest, ModelRuntimeConfig, RetryConfig,
    };
    let mock = MockIssuer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let manager = mock.manager(dir.path(), "one");
    seed(&manager, 5000).await;
    let (endpoint, server) = inference_server(vec![401, 200]).await;
    let auth = Arc::new(LocalInferenceAuth {
        manager,
        endpoint: endpoint.clone(),
    });
    let config = ResponsesConfig::new(endpoint, "", "synthetic-model")
        .with_auth(auth)
        .with_runtime(
            ModelRuntimeConfig::default().with_retry(
                RetryConfig::default()
                    .with_max_attempts(2)
                    .with_base_delay(Duration::ZERO)
                    .with_jitter(false),
            ),
        );
    let model = ResponsesModel::new(config);
    let response = peri_time::timeout(
        Duration::from_secs(5),
        model.complete(
            ModelRequest::new(vec![ModelMessage::user_text("frozen prompt")]),
            CancellationToken::new(),
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(response.assistant_text().as_deref(), Some("ok"));
    let sent = server.await.unwrap();
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[0].1, sent[1].1);
    assert!(sent[0].0.contains("Bearer synthetic-access-initial"));
    assert!(sent[1].0.contains("Bearer synthetic-access-rotated"));
    assert_eq!(mock.state.refreshes.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn second_real_http_401_is_terminal_and_refresh_does_not_expand_attempt_budget() {
    use peri_model::{
        openai_responses::{ResponsesConfig, ResponsesModel},
        Model, ModelRequest, ModelRuntimeConfig, RetryConfig,
    };
    for (attempts, statuses) in [(1, vec![401]), (4, vec![401, 401])] {
        let mock = MockIssuer::start().await;
        let dir = tempfile::tempdir().unwrap();
        let manager = mock.manager(dir.path(), "one");
        seed(&manager, 5000).await;
        let expected_sends = statuses.len();
        let (endpoint, server) = inference_server(statuses).await;
        let auth = Arc::new(LocalInferenceAuth {
            manager,
            endpoint: endpoint.clone(),
        });
        let config = ResponsesConfig::new(endpoint, "", "synthetic-model")
            .with_auth(auth)
            .with_runtime(
                ModelRuntimeConfig::default().with_retry(
                    RetryConfig::default()
                        .with_max_attempts(attempts)
                        .with_base_delay(Duration::ZERO)
                        .with_jitter(false),
                ),
            );
        let result = peri_time::timeout(
            Duration::from_secs(5),
            ResponsesModel::new(config).complete(ModelRequest::default(), CancellationToken::new()),
        )
        .await
        .unwrap();
        assert!(result.is_err());
        assert_eq!(server.await.unwrap().len(), expected_sends);
        assert_eq!(mock.state.refreshes.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn cancelled_begin_login_releases_the_account_lock() {
    let dir = tempfile::tempdir().unwrap();
    let manager =
        ChatGptAuthManager::new(dir.path().join("credentials"), "default".into()).unwrap();
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    assert_eq!(
        manager.begin_login(cancellation).await.unwrap_err(),
        AuthError::Cancelled
    );
    // A cancelled request must not leave the account lease or pending state behind.
    let pending = manager.begin_login(CancellationToken::new()).await.unwrap();
    assert!(pending
        .authorization_url()
        .starts_with("https://auth.openai.com"));
}

#[tokio::test]
async fn cancelled_status_while_lock_is_held_can_retry_after_release() {
    let dir = tempfile::tempdir().unwrap();
    let manager =
        ChatGptAuthManager::new(dir.path().join("credentials"), "default".into()).unwrap();
    let lease = manager
        .server
        .store
        .lease(&CancellationToken::new())
        .await
        .unwrap();
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    assert_eq!(
        manager.status(cancellation).await.unwrap_err(),
        AuthError::Cancelled
    );
    drop(lease);
    assert!(manager
        .status(CancellationToken::new())
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn cancelled_begin_login_while_lock_is_held_does_not_poison_retry() {
    let dir = tempfile::tempdir().unwrap();
    let manager =
        ChatGptAuthManager::new(dir.path().join("credentials"), "default".into()).unwrap();
    let lease = manager
        .server
        .store
        .lease(&CancellationToken::new())
        .await
        .unwrap();
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    assert_eq!(
        manager.begin_login(cancellation).await.unwrap_err(),
        AuthError::Cancelled
    );
    drop(lease);
    assert!(manager.begin_login(CancellationToken::new()).await.is_ok());
}

#[tokio::test]
async fn cancelling_finish_login_releases_the_loopback_port() {
    let dir = tempfile::tempdir().unwrap();
    let manager =
        ChatGptAuthManager::new(dir.path().join("credentials"), "default".into()).unwrap();
    let pending = manager.begin_login(CancellationToken::new()).await.unwrap();
    let authorize = url::Url::parse(pending.authorization_url()).unwrap();
    let query: HashMap<_, _> = authorize.query_pairs().into_owned().collect();
    let redirect = url::Url::parse(&query["redirect_uri"]).unwrap();
    let port = redirect.port().expect("loopback callback port");

    let cancellation = CancellationToken::new();
    let finishing = manager.finish_login(pending, cancellation.clone());
    let cancel = async {
        tokio::task::yield_now().await;
        cancellation.cancel();
    };
    let (result, ()) = tokio::join!(finishing, cancel);
    assert_eq!(result.unwrap_err(), AuthError::Cancelled);

    let rebound = peri_time::timeout(Duration::from_secs(5), async {
        loop {
            match tokio::net::TcpListener::bind(("127.0.0.1", port)).await {
                Ok(listener) => return listener,
                Err(_) => tokio::time::sleep(Duration::from_millis(10)).await,
            }
        }
    })
    .await
    .expect("cancelled finish must drop the callback listener");
    drop(rebound);
}
