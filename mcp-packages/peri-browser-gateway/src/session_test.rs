use super::*;
use crate::protocol::{Caps, ServerMessage};
use crate::stream::FrameMeta;
use bytes::Bytes;

const SECRET: &[u8] = b"test-secret";

fn page(id: &str) -> PageRef {
    PageRef {
        target_id: id.to_owned(),
        url: format!("https://example.test/{id}"),
        title: format!("Page {id}"),
    }
}

fn store() -> SessionStore {
    SessionStore::for_tests(SECRET.to_vec())
}

async fn register(store: &SessionStore, pages: Vec<PageRef>) -> Session {
    store
        .register(
            "s1",
            "peri-abc",
            "ws://127.0.0.1:8827",
            pages,
            AgentState::Idle,
        )
        .await
}

#[tokio::test]
async fn register_stores_the_session_with_a_derived_token() {
    let store = store();
    let session = register(&store, vec![page("t1")]).await;

    assert_eq!(session.id, "s1");
    assert_eq!(session.name, "peri-abc");
    assert_eq!(session.cdp_ws, "ws://127.0.0.1:8827");
    assert_eq!(session.token, token::derive_token(SECRET, "s1"));
    assert_eq!(session.agent_state, AgentState::Idle);
    assert_eq!(store.len().await, 1);

    let stored = store.get("s1").await.expect("session is stored");
    assert_eq!(stored.token, session.token);
    assert!(store.get("missing").await.is_none());
}

#[tokio::test]
async fn lookup_by_token_accepts_only_the_derived_token() {
    let store = store();
    let session = register(&store, vec![page("t1")]).await;

    let found = store
        .lookup_by_token(&session.token)
        .await
        .expect("token resolves");
    assert_eq!(found.id, "s1");

    assert!(store.lookup_by_token("nope").await.is_none());
    assert!(store.lookup_by_token("").await.is_none());
    assert!(store.lookup_by_token(&session.token[..21]).await.is_none());
}

#[tokio::test]
async fn re_register_keeps_the_token_and_open_viewers() {
    let store = store();
    let session = register(&store, vec![page("t1")]).await;
    let mut viewer = session.subscribe();

    let again = store
        .register(
            "s1",
            "peri-renamed",
            "ws://127.0.0.1:9999",
            vec![page("t2")],
            AgentState::Driving,
        )
        .await;

    assert_eq!(again.token, session.token);
    assert_eq!(again.name, "peri-renamed");
    assert_eq!(again.cdp_ws, "ws://127.0.0.1:9999");
    assert_eq!(again.agent_state, AgentState::Driving);

    let update = viewer.try_recv().expect("re-register publishes tabs");
    match update {
        ServerMessage::Tabs { tabs } => {
            assert_eq!(tabs.len(), 1);
            assert_eq!(tabs[0].id, "t2");
            assert!(tabs[0].active);
        }
        other => panic!("unexpected message: {other:?}"),
    }
}

#[tokio::test]
async fn update_pages_repairs_the_active_page() {
    let store = store();
    let session = register(&store, vec![page("t1"), page("t2")]).await;
    assert_eq!(session.effective_active(), Some("t1"));

    store
        .set_active_page("s1", "t2")
        .await
        .expect("t2 is owned");
    assert_eq!(
        store.get("s1").await.unwrap().effective_active(),
        Some("t2")
    );

    let updated = store
        .update_pages("s1", vec![page("t3")])
        .await
        .expect("session exists");
    assert_eq!(updated.effective_active(), Some("t3"));

    let tabs = updated.tabs();
    assert_eq!(tabs.len(), 1);
    assert!(tabs[0].active);
}

#[tokio::test]
async fn update_pages_for_unknown_session_is_none() {
    let store = store();
    assert!(store.update_pages("nope", vec![]).await.is_none());
    assert!(store.set_state("nope", AgentState::Driving).await.is_none());
}

#[tokio::test]
async fn set_state_publishes_to_viewers() {
    let store = store();
    let session = register(&store, vec![]).await;
    let mut viewer = session.subscribe();

    let updated = store
        .set_state("s1", AgentState::WaitingForHuman)
        .await
        .expect("session exists");
    assert_eq!(updated.agent_state, AgentState::WaitingForHuman);

    match viewer.try_recv().expect("state is published") {
        ServerMessage::Agent { state } => assert_eq!(state, AgentState::WaitingForHuman),
        other => panic!("unexpected message: {other:?}"),
    }
}

#[tokio::test]
async fn set_active_page_rejects_foreign_pages() {
    let store = store();
    register(&store, vec![page("t1")]).await;
    assert!(store.set_active_page("s1", "other").await.is_none());
    assert!(store.set_active_page("missing", "t1").await.is_none());
}

#[tokio::test]
async fn set_active_page_publishes_the_tab_strip() {
    let store = store();
    let session = register(&store, vec![page("t1"), page("t2")]).await;
    let mut viewer = session.subscribe();

    store
        .set_active_page("s1", "t2")
        .await
        .expect("t2 is owned");

    match viewer.try_recv().expect("switch is published") {
        ServerMessage::Tabs { tabs } => {
            assert_eq!(tabs.len(), 2);
            assert!(!tabs[0].active);
            assert!(tabs[1].active);
        }
        other => panic!("unexpected message: {other:?}"),
    }
}

#[tokio::test]
async fn unregister_removes_the_session_and_its_token() {
    let store = store();
    let session = register(&store, vec![page("t1")]).await;
    let removed = store.unregister("s1").await.expect("session existed");
    assert_eq!(removed.id, "s1");
    assert!(store.get("s1").await.is_none());
    assert!(store.lookup_by_token(&session.token).await.is_none());
    assert!(store.unregister("s1").await.is_none());
}

#[tokio::test]
async fn list_is_sorted_by_session_id() {
    let store = store();
    for id in ["s3", "s1", "s2"] {
        store
            .register(id, id, "ws://127.0.0.1:8827", vec![], AgentState::Idle)
            .await;
    }
    let ids: Vec<String> = store.list().await.into_iter().map(|s| s.id).collect();
    assert_eq!(ids, ["s1", "s2", "s3"]);
}

#[tokio::test]
async fn hello_describes_the_session() {
    let store = store();
    let session = store
        .register(
            "s1",
            "peri-abc",
            "ws://127.0.0.1:8827",
            vec![page("t1")],
            AgentState::Idle,
        )
        .await;

    match session.hello_message() {
        ServerMessage::Hello {
            session: id,
            name,
            agent,
            interactive,
            caps,
        } => {
            assert_eq!(id, "s1");
            assert_eq!(name, "peri-abc");
            assert_eq!(agent, AgentState::Idle);
            // The MCP owns the page until it yields: the monitor is read-only.
            assert!(!interactive);
            assert_eq!(
                caps,
                Caps {
                    new_tab: true,
                    diagnostics: true
                }
            );
        }
        other => panic!("unexpected message: {other:?}"),
    }
}

#[tokio::test]
async fn tabs_mark_exactly_one_active_page() {
    let store = store();
    let mut session = store
        .register(
            "s1",
            "peri-abc",
            "ws://127.0.0.1:8827",
            vec![page("t1"), page("t2")],
            AgentState::Idle,
        )
        .await;

    let active_count = session.tabs().iter().filter(|tab| tab.active).count();
    assert_eq!(active_count, 1);
    assert!(session.tabs()[0].active);

    assert!(session.set_active_page("t2"));
    match session.tabs_message() {
        ServerMessage::Tabs { tabs } => {
            assert_eq!(tabs.len(), 2);
            assert!(!tabs[0].active);
            assert!(tabs[1].active);
        }
        other => panic!("unexpected message: {other:?}"),
    }
    assert!(!session.set_active_page("t3"));
}

#[tokio::test]
async fn cached_frames_are_keyed_by_page_and_shared_by_snapshots() {
    let store = store();
    let session = store
        .register(
            "s1",
            "peri-abc",
            "ws://127.0.0.1:8827",
            vec![page("t1"), page("t2")],
            AgentState::Idle,
        )
        .await;
    let frame = |page_id: &str| {
        Arc::new(Frame {
            jpeg: Bytes::from_static(b"jpeg"),
            meta: FrameMeta {
                page_id: page_id.to_owned(),
                viewport_width: 640,
                viewport_height: 480,
                device_width: 640,
                device_height: 480,
                page_scale_factor: 1.0,
                scroll_offset_x: 0.0,
                scroll_offset_y: 0.0,
            },
        })
    };

    assert!(session.cached_frame("t1").is_none());

    // The supervisor writes through the `Arc` it received from `frame_cache`;
    // viewers read through a session snapshot, so both must share one map.
    session
        .frame_cache()
        .lock()
        .expect("cache")
        .insert("t1".to_owned(), frame("t1"));
    let snapshot = session.clone();
    assert_eq!(
        snapshot
            .cached_frame("t1")
            .expect("t1 is cached")
            .meta
            .page_id,
        "t1"
    );
    assert!(snapshot.cached_frame("t2").is_none());

    session
        .frame_cache()
        .lock()
        .expect("cache")
        .insert("t2".to_owned(), frame("t2"));
    assert_eq!(
        snapshot
            .cached_frame("t2")
            .expect("t2 is cached")
            .meta
            .page_id,
        "t2"
    );
}

#[tokio::test]
async fn debug_hides_the_token() {
    let store = SessionStore::for_tests(b"secret".to_vec());
    let token = token::derive_token(b"secret", "s1");
    let session = store
        .register(
            "s1",
            "peri-abc",
            "ws://127.0.0.1:8827",
            vec![],
            AgentState::Idle,
        )
        .await;
    let rendered = format!("{session:?}");
    assert!(rendered.contains("[redacted]"));
    assert!(!rendered.contains(&token));
}
