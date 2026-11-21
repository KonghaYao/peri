use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::UnixStream;
use tokio::task::JoinHandle;
use tokio::time::{sleep, timeout, Instant};
use uuid::Uuid;

use super::*;
use crate::protocol::{AgentState, BrowserCommand, GatewayMessage, PageRef, YieldReason};
use crate::token;

const SECRET: &[u8] = b"secret";

struct SocketGuard(PathBuf);

impl SocketGuard {
    fn new(name: &str) -> Self {
        // Keep the path far below the 104-byte sun_path limit.
        Self(PathBuf::from(format!(
            "/tmp/peri-bgw-{name}-{}.sock",
            Uuid::new_v4()
        )))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for SocketGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

struct TestRegistry {
    _guard: SocketGuard,
    sessions: Arc<SessionStore>,
    handle: Arc<RegistryHandle>,
    server: JoinHandle<()>,
}

impl TestRegistry {
    async fn start(name: &str) -> Self {
        let guard = SocketGuard::new(name);
        let listener = UnixListener::bind(guard.path()).expect("bind test socket");
        let handle = Arc::new(RegistryHandle::new());
        // The ownership driver answers yield/reclaim through this handle, so the
        // session table must share it with the fixture.
        let sessions = Arc::new(SessionStore::for_tests_with(
            SECRET.to_vec(),
            handle.clone(),
        ));
        let context = RegistryContext {
            sessions: sessions.clone(),
            handle: handle.clone(),
            monitor_base: "http://127.0.0.1:7391".to_owned(),
        };
        let server = tokio::spawn(serve(listener, context));
        Self {
            _guard: guard,
            sessions,
            handle,
            server,
        }
    }

    async fn connect(&self) -> (OwnedReadHalf, OwnedWriteHalf) {
        UnixStream::connect(self._guard.path())
            .await
            .expect("connect test socket")
            .into_split()
    }
}

impl Drop for TestRegistry {
    fn drop(&mut self) {
        self.server.abort();
    }
}

fn page(id: &str) -> PageRef {
    PageRef {
        target_id: id.to_owned(),
        url: format!("https://example.test/{id}"),
        title: format!("Page {id}"),
    }
}

fn register_message(pages: Vec<PageRef>, state: AgentState) -> RegistryMessage {
    RegistryMessage::Register {
        session_id: "s1".to_owned(),
        name: "peri-abc".to_owned(),
        cdp_url: "ws://127.0.0.1:8827".to_owned(),
        pages,
        state,
    }
}

async fn write_message(writer: &mut OwnedWriteHalf, message: &RegistryMessage) {
    let line = encode_line(message).expect("encode message");
    writer
        .write_all(line.as_bytes())
        .await
        .expect("write registry line");
}

async fn wait_for_session(sessions: &SessionStore, id: &str) -> crate::session::Session {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if let Some(session) = sessions.get(id).await {
            return session;
        }
        assert!(Instant::now() < deadline, "session {id} was not registered");
        sleep(Duration::from_millis(10)).await;
    }
}

async fn wait_for<F, Fut, T>(mut probe: F) -> T
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Option<T>>,
{
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if let Some(value) = probe().await {
            return value;
        }
        assert!(Instant::now() < deadline, "condition was not met");
        sleep(Duration::from_millis(10)).await;
    }
}

async fn send_when_bound(handle: &RegistryHandle, message: &GatewayMessage) {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match handle.send("s1", message.clone()).await {
            Ok(()) => return,
            Err(error) => {
                assert!(Instant::now() < deadline, "registry never bound: {error}");
                sleep(Duration::from_millis(10)).await;
            }
        }
    }
}

async fn read_message(lines: &mut tokio::io::Lines<BufReader<OwnedReadHalf>>) -> String {
    timeout(Duration::from_secs(2), lines.next_line())
        .await
        .expect("gateway message timed out")
        .expect("read registry line")
        .expect("registry connection closed")
}

#[test]
fn codec_roundtrips_registry_and_gateway_messages() {
    let message = register_message(vec![page("t1")], AgentState::Idle);
    let line = encode_line(&message).expect("encode");
    assert!(line.ends_with('\n'));
    assert_eq!(line.trim_end(), serde_json::to_string(&message).unwrap());

    let decoded: RegistryMessage = serde_json::from_str(line.trim_end()).unwrap();
    assert_eq!(decoded, message);

    let gateway = GatewayMessage::Command {
        session_id: "s1".to_owned(),
        request_id: "r1".to_owned(),
        page_id: "t1".to_owned(),
        command: BrowserCommand::History { delta: -1 },
    };
    let decoded: GatewayMessage =
        serde_json::from_str(encode_line(&gateway).unwrap().trim_end()).unwrap();
    assert_eq!(decoded, gateway);
}

#[tokio::test]
async fn register_creates_the_session_and_routes_gateway_messages() {
    let registry = TestRegistry::start("register").await;
    let (read, mut write) = registry.connect().await;

    write_message(
        &mut write,
        &register_message(vec![page("t1")], AgentState::Idle),
    )
    .await;
    let session = wait_for_session(&registry.sessions, "s1").await;
    assert_eq!(session.token, token::derive_token(SECRET, "s1"));
    assert_eq!(session.pages.len(), 1);

    let request = GatewayMessage::YieldRequest {
        session_id: "s1".to_owned(),
        page_id: "t1".to_owned(),
        reason: YieldReason::Takeover,
        timeout_ms: 60_000,
    };
    send_when_bound(&registry.handle, &request).await;

    let mut lines = BufReader::new(read).lines();
    let line = read_message(&mut lines).await;
    let received: GatewayMessage = serde_json::from_str(&line).expect("gateway frame is JSON");
    assert_eq!(received, request);
}

#[tokio::test]
async fn malformed_and_unknown_lines_do_not_close_the_connection() {
    let registry = TestRegistry::start("tolerant").await;
    let (_read, mut write) = registry.connect().await;

    write.write_all(b"this is not json\n").await.unwrap();
    write
        .write_all(b"{\"type\":\"from_a_newer_mcp\"}\n")
        .await
        .unwrap();
    write.write_all(b"\n").await.unwrap();
    write_message(
        &mut write,
        &register_message(vec![page("t1")], AgentState::Idle),
    )
    .await;

    let session = wait_for_session(&registry.sessions, "s1").await;
    assert_eq!(session.name, "peri-abc");
}

#[tokio::test]
async fn update_pages_and_state_reach_the_store() {
    let registry = TestRegistry::start("updates").await;
    let (_read, mut write) = registry.connect().await;

    write_message(
        &mut write,
        &register_message(vec![page("t1")], AgentState::Idle),
    )
    .await;
    wait_for_session(&registry.sessions, "s1").await;

    write_message(
        &mut write,
        &RegistryMessage::UpdatePages {
            session_id: "s1".to_owned(),
            pages: vec![page("t2")],
        },
    )
    .await;
    wait_for(|| async {
        registry.sessions.get("s1").await.filter(|session| {
            session
                .pages
                .first()
                .is_some_and(|page| page.target_id == "t2")
        })
    })
    .await;

    write_message(
        &mut write,
        &RegistryMessage::State {
            session_id: "s1".to_owned(),
            state: AgentState::WaitingForHuman,
        },
    )
    .await;
    wait_for(|| async {
        registry
            .sessions
            .get("s1")
            .await
            .filter(|session| session.agent_state == AgentState::WaitingForHuman)
    })
    .await;
}

#[tokio::test]
async fn reclaim_is_acknowledged() {
    let registry = TestRegistry::start("reclaim").await;
    let (read, mut write) = registry.connect().await;
    let mut lines = BufReader::new(read).lines();

    write_message(
        &mut write,
        &register_message(vec![page("t1")], AgentState::Idle),
    )
    .await;
    wait_for_session(&registry.sessions, "s1").await;
    write_message(
        &mut write,
        &RegistryMessage::Reclaim {
            session_id: "s1".to_owned(),
            page_id: "t1".to_owned(),
        },
    )
    .await;

    let received: GatewayMessage =
        serde_json::from_str(&read_message(&mut lines).await).expect("ack is JSON");
    assert_eq!(
        received,
        GatewayMessage::Reclaimed {
            session_id: "s1".to_owned(),
            page_id: "t1".to_owned(),
            ok: true,
        }
    );
}

#[tokio::test]
async fn an_old_connection_cannot_unbind_a_reconnected_session() {
    let registry = TestRegistry::start("rebind").await;
    let (read_old, mut write_old) = registry.connect().await;
    write_message(
        &mut write_old,
        &register_message(vec![page("t1")], AgentState::Idle),
    )
    .await;
    wait_for_session(&registry.sessions, "s1").await;

    let (read_new, mut write_new) = registry.connect().await;
    write_message(
        &mut write_new,
        &register_message(vec![page("t2")], AgentState::Idle),
    )
    .await;
    wait_for(|| async {
        registry.sessions.get("s1").await.filter(|session| {
            session
                .pages
                .first()
                .is_some_and(|page| page.target_id == "t2")
        })
    })
    .await;

    let request = GatewayMessage::YieldRequest {
        session_id: "s1".to_owned(),
        page_id: "t2".to_owned(),
        reason: YieldReason::Takeover,
        timeout_ms: 60_000,
    };
    // The rebind happens just after the store update, so probe until the new
    // connection actually receives a frame. Frames routed to the old writer are
    // discarded with it.
    let mut new_lines = BufReader::new(read_new).lines();
    let mut bound_to_new = false;
    for _ in 0..100 {
        send_when_bound(&registry.handle, &request).await;
        match timeout(Duration::from_millis(50), new_lines.next_line()).await {
            Ok(Ok(Some(_))) => {
                bound_to_new = true;
                break;
            }
            _ => tokio::time::sleep(Duration::from_millis(10)).await,
        }
    }
    assert!(
        bound_to_new,
        "the reconnected session never reached the new connection"
    );

    drop(write_old);
    drop(read_old);
    tokio::time::sleep(Duration::from_millis(50)).await;

    // The old connection's cleanup must not remove the newer binding.
    send_when_bound(&registry.handle, &request).await;
    let _ = read_message(&mut new_lines).await;

    drop(write_new);
    drop(new_lines);
    wait_for(|| async {
        if registry.handle.send("s1", request.clone()).await.is_err() {
            Some(())
        } else {
            None
        }
    })
    .await;
}

#[tokio::test]
async fn unregister_removes_the_session() {
    let registry = TestRegistry::start("unregister").await;
    let (read, mut write) = registry.connect().await;
    let lines = BufReader::new(read).lines();

    write_message(
        &mut write,
        &register_message(vec![page("t1")], AgentState::Idle),
    )
    .await;
    wait_for_session(&registry.sessions, "s1").await;
    write_message(
        &mut write,
        &RegistryMessage::Unregister {
            session_id: "s1".to_owned(),
        },
    )
    .await;

    wait_for(|| async {
        if registry.sessions.is_empty().await {
            Some(())
        } else {
            None
        }
    })
    .await;
    assert!(registry.sessions.get("s1").await.is_none());
    // The connection stays usable for the next session.
    drop(lines);
    write_message(&mut write, &register_message(vec![], AgentState::Idle)).await;
    wait_for_session(&registry.sessions, "s1").await;
}
