use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine as _;
use futures::{SinkExt, StreamExt};
use serde_json::Value;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message;

use super::*;
use crate::protocol::PageRef;
use crate::session::SessionStore;

const FRAME_BYTES: &[u8] = b"frame";
const THUMB_BYTES: &[u8] = b"thumb";

struct FakeCdp {
    address: std::net::SocketAddr,
    paths: Arc<Mutex<Vec<String>>>,
    methods: Arc<Mutex<Vec<String>>>,
    task: JoinHandle<()>,
}

impl Drop for FakeCdp {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[allow(clippy::result_large_err)]
async fn fake_cdp() -> FakeCdp {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address = listener.local_addr().expect("address");
    let paths = Arc::new(Mutex::new(Vec::new()));
    let methods = Arc::new(Mutex::new(Vec::new()));
    let server_paths = paths.clone();
    let server_methods = methods.clone();

    let task = tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            let paths = server_paths.clone();
            let methods = server_methods.clone();
            tokio::spawn(async move {
                let mut socket = match tokio_tungstenite::accept_hdr_async(
                    stream,
                    |request: &tokio_tungstenite::tungstenite::handshake::server::Request,
                     response| {
                        paths.lock().expect("paths").push(request.uri().to_string());
                        Ok(response)
                    },
                )
                .await
                {
                    Ok(socket) => socket,
                    Err(_) => return,
                };

                while let Some(Ok(Message::Text(text))) = socket.next().await {
                    let request: Value = match serde_json::from_str(text.as_str()) {
                        Ok(request) => request,
                        Err(_) => continue,
                    };
                    let id = request.get("id").cloned().unwrap_or(Value::Null);
                    let method = request
                        .get("method")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned();
                    methods.lock().expect("methods").push(method.clone());
                    let response = match method.as_str() {
                        "Page.getLayoutMetrics" => serde_json::json!({
                            "id": id,
                            "result": {"cssLayoutViewport": {"clientWidth": 640, "clientHeight": 480}}
                        }),
                        "Page.startScreencast" => {
                            let event = serde_json::json!({
                                "method": "Page.screencastFrame",
                                "params": {
                                    "data": base64::engine::general_purpose::STANDARD
                                        .encode(FRAME_BYTES),
                                    "sessionId": 1,
                                    "metadata": {
                                        "deviceWidth": 640,
                                        "deviceHeight": 480,
                                        "pageScaleFactor": 1,
                                        "scrollOffsetX": 0,
                                        "scrollOffsetY": 0
                                    }
                                }
                            });
                            if socket
                                .send(Message::Text(event.to_string().into()))
                                .await
                                .is_err()
                            {
                                return;
                            }
                            serde_json::json!({"id": id, "result": {}})
                        }
                        "Page.captureScreenshot" => serde_json::json!({
                            "id": id,
                            "result": {
                                "data": base64::engine::general_purpose::STANDARD
                                    .encode(THUMB_BYTES)
                            }
                        }),
                        _ => serde_json::json!({"id": id, "result": {}}),
                    };
                    if socket
                        .send(Message::Text(response.to_string().into()))
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
            });
        }
    });

    FakeCdp {
        address,
        paths,
        methods,
        task,
    }
}

fn page(id: &str) -> PageRef {
    PageRef {
        target_id: id.to_owned(),
        url: format!("https://example.test/{id}"),
        title: format!("Page {id}"),
    }
}

async fn registered(server: &FakeCdp, pages: Vec<PageRef>) -> Arc<SessionStore> {
    let store = Arc::new(SessionStore::for_tests(b"secret".to_vec()));
    store
        .register(
            "s1",
            "peri-abc",
            format!("ws://{}", server.address),
            pages,
            crate::protocol::AgentState::Idle,
        )
        .await;
    store
}

async fn next_frame(frames: &mut broadcast::Receiver<Arc<Frame>>) -> Arc<Frame> {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match frames.recv().await {
                Ok(frame) => return frame,
                Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => panic!("frame channel closed"),
            }
        }
    })
    .await
    .expect("frame arrives")
}

async fn next_matching<F>(
    frames: &mut broadcast::Receiver<Arc<Frame>>,
    mut matches: F,
) -> Arc<Frame>
where
    F: FnMut(&Frame) -> bool,
{
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match frames.recv().await {
                Ok(frame) if matches(&frame) => return frame,
                Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => panic!("frame channel closed"),
            }
        }
    })
    .await
    .expect("matching frame arrives")
}

#[test]
fn a_frame_is_always_preceded_by_meta() {
    let frame = Frame {
        jpeg: Bytes::from_static(FRAME_BYTES),
        meta: FrameMeta {
            page_id: "t1".to_owned(),
            viewport_width: 1280,
            viewport_height: 720,
            device_width: 800,
            device_height: 600,
            page_scale_factor: 1.0,
            scroll_offset_x: 0.0,
            scroll_offset_y: 0.0,
        },
    };
    let value = serde_json::to_value(frame.meta_message()).unwrap();
    assert_eq!(
        value,
        serde_json::json!({
            "t": "meta",
            "pageId": "t1",
            "viewportWidth": 1280,
            "viewportHeight": 720,
            "deviceWidth": 800,
            "deviceHeight": 600,
            "pageScaleFactor": 1.0,
            "scrollOffsetX": 0.0,
            "scrollOffsetY": 0.0
        })
    );
}

#[test]
fn desired_page_requires_a_viewer_and_a_page() {
    let mut state = State {
        cdp_url: "ws://127.0.0.1:1".to_owned(),
        pages: vec![page("t1"), page("t2")],
        active: Some("t2".to_owned()),
        viewers: 0,
        thumbs_dirty: false,
    };
    assert_eq!(desired_page(&state), None);

    state.viewers = 1;
    assert_eq!(desired_page(&state).as_deref(), Some("t2"));

    state.active = Some("gone".to_owned());
    assert_eq!(desired_page(&state).as_deref(), Some("t1"));

    state.pages.clear();
    assert_eq!(desired_page(&state), None);
}

#[tokio::test]
async fn no_viewer_means_no_cdp_connection() {
    let server = fake_cdp().await;
    let _store = registered(&server, vec![page("t1")]).await;

    tokio::time::sleep(Duration::from_millis(700)).await;
    assert!(
        server.paths.lock().expect("paths").is_empty(),
        "a session without viewers must not open a CDP connection"
    );
}

#[tokio::test]
async fn the_first_viewer_starts_a_screencast_and_receives_jpeg_frames() {
    let server = fake_cdp().await;
    let store = registered(&server, vec![page("t1")]).await;
    let session = store.get("s1").await.expect("session");
    let mut frames = session.subscribe_frames();

    store.viewer_joined("s1").await;
    let frame = next_frame(&mut frames).await;

    assert_eq!(frame.jpeg.as_ref(), FRAME_BYTES);
    assert_eq!(frame.meta.page_id, "t1");
    assert_eq!(frame.meta.viewport_width, 640);
    assert_eq!(frame.meta.viewport_height, 480);
    assert_eq!(frame.meta.device_width, 640);

    let paths = server.paths.lock().expect("paths").clone();
    assert!(
        paths
            .iter()
            .any(|path| path.starts_with("/devtools/page/t1?role=viewer")),
        "viewer role is mandatory: {paths:?}"
    );
    let methods = server.methods.lock().expect("methods").clone();
    assert!(methods
        .iter()
        .any(|method| method == "Page.startScreencast"));
    assert!(methods
        .iter()
        .any(|method| method == "Page.screencastFrameAck"));
}

#[tokio::test]
async fn switching_tabs_opens_a_viewer_for_the_new_page() {
    let server = fake_cdp().await;
    let store = registered(&server, vec![page("t1"), page("t2")]).await;
    let session = store.get("s1").await.expect("session");
    let mut frames = session.subscribe_frames();

    store.viewer_joined("s1").await;
    let first = next_frame(&mut frames).await;
    assert_eq!(first.meta.page_id, "t1");

    store
        .set_active_page("s1", "t2")
        .await
        .expect("t2 is owned");
    let second = next_matching(&mut frames, |frame| frame.meta.page_id == "t2").await;
    assert_eq!(second.jpeg.as_ref(), FRAME_BYTES);

    let paths = server.paths.lock().expect("paths").clone();
    assert!(
        paths
            .iter()
            .any(|path| path.starts_with("/devtools/page/t2?role=viewer")),
        "switch attaches the new page: {paths:?}"
    );
}

#[tokio::test]
async fn frames_are_cached_per_page_until_the_tab_is_closed() {
    let server = fake_cdp().await;
    let store = registered(&server, vec![page("t1"), page("t2")]).await;
    let session = store.get("s1").await.expect("session");
    let mut frames = session.subscribe_frames();

    store.viewer_joined("s1").await;
    let first = next_frame(&mut frames).await;
    let cached = session.cached_frame("t1").expect("t1 frame is cached");
    assert_eq!(cached.jpeg, first.jpeg);
    assert!(session.cached_frame("t2").is_none());

    // Switching away must not evict the frame: coming back to a static tab has
    // to paint from the cache instead of waiting for damage.
    store
        .set_active_page("s1", "t2")
        .await
        .expect("t2 is owned");
    let second = next_matching(&mut frames, |frame| frame.meta.page_id == "t2").await;
    assert_eq!(second.meta.page_id, "t2");
    assert_eq!(
        session
            .cached_frame("t1")
            .expect("t1 frame survives the switch")
            .jpeg,
        first.jpeg
    );

    // A closed target is unreachable, so its frame must not linger.
    store
        .update_pages("s1", vec![page("t2")])
        .await
        .expect("session exists");
    tokio::time::timeout(Duration::from_secs(2), async {
        while session.cached_frame("t1").is_some() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("closed page is evicted from the frame cache");
}

#[test]
fn thumbnail_mime_follows_the_magic_bytes() {
    use base64::engine::general_purpose::STANDARD;

    // `about:blank` comes back as a PNG even though JPEG was requested.
    let png = STANDARD.encode([0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]);
    let jpeg = STANDARD.encode([0xff, 0xd8, 0xff, 0xe0, 0x00, 0x10]);

    assert_eq!(thumbnail_mime(&png), "image/png");
    assert_eq!(thumbnail_mime(&jpeg), "image/jpeg");
    assert_eq!(thumbnail_mime(""), "image/jpeg");
    assert_eq!(thumbnail_mime("!!"), "image/jpeg");
}

#[tokio::test]
async fn inactive_tabs_get_thumbnail_captures() {
    let server = fake_cdp().await;
    let store = registered(&server, vec![page("t1"), page("t2")]).await;
    let session = store.get("s1").await.expect("session");
    let mut messages = session.subscribe();

    store.viewer_joined("s1").await;

    let thumbnail = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match messages.recv().await {
                Ok(ServerMessage::Thumb { page_id, data }) => return (page_id, data),
                Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => panic!("message channel closed"),
            }
        }
    })
    .await
    .expect("thumbnail arrives");

    assert_eq!(thumbnail.0, "t2");
    assert_eq!(
        thumbnail.1,
        format!(
            "data:image/jpeg;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(THUMB_BYTES)
        )
    );
}

#[tokio::test]
async fn unregister_stops_the_stream_supervisor() {
    let server = fake_cdp().await;
    let store = registered(&server, vec![page("t1")]).await;
    let session = store.get("s1").await.expect("session");
    let mut frames = session.subscribe_frames();
    store.viewer_joined("s1").await;
    let _ = next_frame(&mut frames).await;

    store.unregister("s1").await.expect("session existed");
    assert!(store.get("s1").await.is_none());

    // No new command can reach the supervisor after unregister; the frame
    // channel stays open because the test still holds the session snapshot.
    let before = server.methods.lock().expect("methods").len();
    tokio::time::sleep(Duration::from_millis(500)).await;
    let after = server.methods.lock().expect("methods").len();
    assert!(
        after <= before + 1,
        "supervisor should be idle after unregister: {before} -> {after}"
    );
}
