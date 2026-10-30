//! Child timeline reads must not create executable ACP sessions.

use super::*;
use peri_acp_types::history_window::HistoryEntryId;
use peri_acp_types::session_resources::{ChildSnapshot, RewindBoundary};
use peri_acp_types::store::{serialize_persisted_payload, InheritedContext};

fn child_caps() -> PeriCaps {
    PeriCaps {
        history_window_v1: true,
        history_pagination: true,
        replay: true,
        ..Default::default()
    }
}

async fn create_child(fixture: &PageFixture, tmp: &tempfile::TempDir, parent: &str) -> String {
    create_child_of(fixture, tmp, parent, parent).await
}

async fn create_child_of(
    fixture: &PageFixture,
    tmp: &tempfile::TempDir,
    parent: &str,
    root: &str,
) -> String {
    let workspace = fixture
        .cfg
        .session_resources
        .resolve_workspace(tmp.path())
        .await
        .unwrap();
    let child = new_session_id();
    let frozen = frozen_snapshot_bytes(&fixture.cfg, parent).await.unwrap();
    let mut target = bound_input(&child, &workspace, frozen);
    target.meta.parent_thread_id = Some(parent.to_owned());
    target.meta.hidden = true;
    fixture
        .cfg
        .session_resources
        .save_child(&ChildSnapshot {
            target,
            parent_id: parent.to_owned(),
            root_id: root.to_owned(),
            inherited: InheritedContext::default(),
        })
        .await
        .unwrap();
    let payloads: Vec<_> = (0..4)
        .map(|n| PersistedPayload::Message(BaseMessage::ai(format!("child answer {n}"))))
        .collect();
    fixture
        .cfg
        .session_resources
        .append_history(&child, &payloads)
        .await
        .unwrap();
    child
}

async fn child_request(
    client: &crate::transport::mpsc::MpscClientTransport,
    parent: &str,
    child: &str,
    history: Value,
) -> Result<Value, AcpError> {
    client
        .send_request(
            "session/history/child",
            json!({
                "parentSessionId": parent, "childThreadId": child, "history": history,
            }),
        )
        .await
}

async fn child_page(
    client: &crate::transport::mpsc::MpscClientTransport,
    parent: &str,
    child: &str,
    opened: &Value,
) -> Result<Value, AcpError> {
    child_request(client, parent, child, json!({
        "kind":"page", "snapshotToken":opened["snapshotToken"], "readTicket":opened["readTicket"],
        "beforeCursor":opened["beforeCursor"], "limit":2,
    })).await
}

#[tokio::test]
#[serial]
async fn child_history_pages_without_session_state_or_execution_setup() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    let child = create_child(&fixture, &tmp, &fixture.sid).await;
    let (client, sessions, task) = spawn_loop(
        Arc::clone(&fixture.cfg),
        Arc::clone(&fixture.sessions),
        Some(child_caps()),
    )
    .await;
    let opened = child_request(
        &client,
        &fixture.sid,
        &child,
        json!({"kind":"open","direction":"before","limit":2}),
    )
    .await
    .unwrap();
    assert_eq!(opened["sessionId"], child);
    assert_eq!(opened["parentSessionId"], fixture.sid);
    assert_eq!(opened["scope"], "child");
    assert_eq!(opened["items"].as_array().unwrap().len(), 2);
    append_human_message(&fixture.cfg, &child, "appended after snapshot").await;
    let page = child_page(&client, &fixture.sid, &child, &opened)
        .await
        .unwrap();
    assert_eq!(page["items"].as_array().unwrap().len(), 2);
    assert_eq!(page["committedTail"], opened["committedTail"]);
    assert!(!page.to_string().contains("appended after snapshot"));
    assert!(page["beforeCursor"].is_null());
    let states = sessions.lock().await;
    assert_eq!(states.len(), 1);
    assert!(!states.contains_key(&child));
    drop(states);
    assert!(
        s9c_drain_notifications(&client).await.is_empty(),
        "browsing must not replay or execute"
    );
    task.abort();
}

#[tokio::test]
#[serial]
async fn child_history_authorizes_direct_parent_and_replaces_one_scope() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    let child = create_child(&fixture, &tmp, &fixture.sid).await;
    let other = create_child(&fixture, &tmp, &fixture.sid).await;
    let unrelated = create_bound_fixture(&fixture.cfg, tmp.path().to_str().unwrap(), None).await;
    let (client, _, task) = spawn_loop(
        Arc::clone(&fixture.cfg),
        Arc::clone(&fixture.sessions),
        Some(child_caps()),
    )
    .await;
    let open = json!({"kind":"open","direction":"before","limit":2});
    let first = child_request(&client, &fixture.sid, &child, open.clone())
        .await
        .unwrap();
    assert!(
        child_request(&client, &fixture.sid, &unrelated, open.clone())
            .await
            .is_err()
    );
    // A rejected open must preserve the last successfully issued scope.
    child_page(&client, &fixture.sid, &child, &first)
        .await
        .unwrap();
    child_request(&client, &fixture.sid, &other, open)
        .await
        .unwrap();
    assert!(child_page(&client, &fixture.sid, &child, &first)
        .await
        .is_err());
    assert_eq!(fixture.sessions.lock().await.len(), 1);
    task.abort();
}

#[tokio::test]
#[serial]
async fn child_history_rewind_delete_and_parent_generation_invalidate() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    let child = create_child(&fixture, &tmp, &fixture.sid).await;
    let (client, sessions, task) = spawn_loop(
        Arc::clone(&fixture.cfg),
        Arc::clone(&fixture.sessions),
        Some(child_caps()),
    )
    .await;
    let open = json!({"kind":"open","direction":"before","limit":2});
    let opened = child_request(&client, &fixture.sid, &child, open.clone())
        .await
        .unwrap();
    let ids = fixture
        .cfg
        .session_resources
        .load_session_history(&child)
        .await
        .unwrap();
    fixture
        .cfg
        .session_resources
        .rewind_history(&child, RewindBoundary::KeepThrough(ids[1].id()), &[])
        .await
        .unwrap();
    assert!(child_page(&client, &fixture.sid, &child, &opened)
        .await
        .is_err());
    let opened = child_request(
        &client,
        &fixture.sid,
        &child,
        json!({"kind":"open","direction":"before","limit":1}),
    )
    .await
    .unwrap();
    sessions
        .lock()
        .await
        .get_mut(&fixture.sid)
        .unwrap()
        .session_generation += 1;
    assert!(child_page(&client, &fixture.sid, &child, &opened)
        .await
        .is_err());
    let opened = child_request(
        &client,
        &fixture.sid,
        &child,
        json!({"kind":"open","direction":"before","limit":1}),
    )
    .await
    .unwrap();
    fixture
        .cfg
        .session_resources
        .delete_session_tree(&child)
        .await
        .unwrap();
    assert!(child_page(&client, &fixture.sid, &child, &opened)
        .await
        .is_err());
    assert!(child_request(&client, &fixture.sid, &child, open)
        .await
        .is_err());
    task.abort();
}

#[tokio::test]
#[serial]
async fn child_history_descendants_require_bounded_canonical_ancestry() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    let child = create_child(&fixture, &tmp, &fixture.sid).await;
    let descendant = create_child_of(&fixture, &tmp, &child, &fixture.sid).await;
    let foreign = create_bound_fixture(&fixture.cfg, tmp.path().to_str().unwrap(), None).await;
    let foreign_child = create_child(&fixture, &tmp, &foreign).await;
    let (client, sessions, task) = spawn_loop(
        Arc::clone(&fixture.cfg),
        Arc::clone(&fixture.sessions),
        Some(child_caps()),
    )
    .await;
    let open = json!({"kind":"open","direction":"before","limit":2});
    let opened = child_request(&client, &fixture.sid, &descendant, open.clone())
        .await
        .unwrap();
    child_page(&client, &fixture.sid, &descendant, &opened)
        .await
        .unwrap();
    assert_eq!(
        sessions.lock().await.len(),
        1,
        "ancestry reads cannot create runtimes"
    );
    assert!(
        child_request(&client, &fixture.sid, &foreign_child, open.clone())
            .await
            .is_err()
    );
    let pool = sqlx::SqlitePool::connect_with(
        sqlx::sqlite::SqliteConnectOptions::new().filename(tmp.path().join("threads.db")),
    )
    .await
    .unwrap();
    for parent in [
        &foreign,
        &descendant,
        &"deleted-ancestor".to_owned(),
        &"x".repeat(257),
    ] {
        sqlx::query("UPDATE threads SET parent_thread_id = ?1 WHERE id = ?2")
            .bind(parent)
            .bind(&child)
            .execute(&pool)
            .await
            .unwrap();
        assert!(child_page(&client, &fixture.sid, &descendant, &opened)
            .await
            .is_err());
        assert!(
            child_request(&client, &fixture.sid, &descendant, open.clone())
                .await
                .is_err()
        );
    }
    pool.close().await;
    task.abort();
}

#[tokio::test]
#[serial]
async fn child_history_filters_descriptors_and_rechecks_body_audience() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    let child = create_child(&fixture, &tmp, &fixture.sid).await;
    let hidden = PersistedPayload::Message(BaseMessage::system("hidden child system".repeat(100)));
    let hidden_id = hidden.id();
    fixture
        .cfg
        .session_resources
        .append_history(&child, &[hidden])
        .await
        .unwrap();
    let (client, _, task) = spawn_loop(
        Arc::clone(&fixture.cfg),
        Arc::clone(&fixture.sessions),
        Some(child_caps()),
    )
    .await;
    for byte_budget in [1, 512 * 1024] {
        let hidden = child_request(&client, &fixture.sid, &child, json!({
            "kind":"open", "direction":"before", "limit":1, "byteBudget":byte_budget,
            "anchor":{"kind":"entry","entryId":HistoryEntryId::new(&child, hidden_id).encode(),"source":"own"},
        })).await.unwrap();
        assert_eq!(hidden["items"], json!([]));
        assert!(!hidden.to_string().contains("hidden child system"));
    }
    let visible = BaseMessage::ai("visible child body".repeat(100));
    let visible_id = visible.id();
    fixture
        .cfg
        .session_resources
        .append_history(&child, &[PersistedPayload::Message(visible)])
        .await
        .unwrap();
    let opened = child_request(
        &client,
        &fixture.sid,
        &child,
        json!({"kind":"open","direction":"before","limit":1,"byteBudget":1}),
    )
    .await
    .unwrap();
    assert!(opened["items"][0]["body"].is_object());
    let body = json!({
        "parentSessionId":fixture.sid, "childThreadId":child, "entryId":opened["items"][0]["entryId"],
        "snapshotToken":opened["snapshotToken"], "readTicket":opened["readTicket"], "offset":0, "maxBytes":64,
    });
    let chunk = client
        .send_request("session/history/child/body", body.clone())
        .await
        .unwrap();
    assert_eq!(chunk["format"], "display_text");
    assert_eq!(chunk["scope"], "child");
    assert_eq!(chunk["committedTail"], opened["committedTail"]);
    assert!(chunk["bytes"].as_array().unwrap().len() <= 64);
    assert!(chunk["nextOffset"].is_u64());
    fixture
        .sessions
        .lock()
        .await
        .get_mut(&fixture.sid)
        .unwrap()
        .history_window
        .force_oversize_overflow();
    let overflow_chunk = client
        .send_request("session/history/child/body", body.clone())
        .await
        .unwrap();
    assert_eq!(
        overflow_chunk, chunk,
        "descriptor revalidation must keep the original small budget"
    );
    let pool = sqlx::SqlitePool::connect_with(
        sqlx::sqlite::SqliteConnectOptions::new().filename(tmp.path().join("threads.db")),
    )
    .await
    .unwrap();
    let replacement = PersistedPayload::Message(BaseMessage::System {
        id: visible_id,
        content: "now private".into(),
    });
    sqlx::query("UPDATE messages SET content = ?1 WHERE message_id = ?2")
        .bind(serialize_persisted_payload(&replacement).unwrap())
        .bind(visible_id.as_uuid().to_string())
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;
    assert!(client
        .send_request("session/history/child/body", body)
        .await
        .is_err());
    task.abort();
}

#[tokio::test]
#[serial]
async fn child_history_requires_capability_and_rejects_setup_fields() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    let child = create_child(&fixture, &tmp, &fixture.sid).await;
    let (client, _, task) = spawn_loop(
        Arc::clone(&fixture.cfg),
        Arc::clone(&fixture.sessions),
        None,
    )
    .await;
    assert!(child_request(
        &client,
        &fixture.sid,
        &child,
        json!({"kind":"open","direction":"before"})
    )
    .await
    .is_err());
    task.abort();
    let (client, _, task) = spawn_loop(
        Arc::clone(&fixture.cfg),
        Arc::clone(&fixture.sessions),
        Some(child_caps()),
    )
    .await;
    let rejected = client.send_request("session/history/child", json!({
        "parentSessionId":fixture.sid, "childThreadId":child, "history":{"kind":"open","direction":"before"}, "cwd":"/tmp",
    })).await;
    assert_eq!(rejected.unwrap_err().code, -32602);
    assert_eq!(fixture.sessions.lock().await.len(), 1);
    task.abort();
}

#[tokio::test]
#[serial]
async fn child_history_projects_text_reasoning_and_tools_without_provider_state() {
    use peri_acp_types::messages::ContentBlock;

    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    let child = create_child(&fixture, &tmp, &fixture.sid).await;
    let payload = PersistedPayload::Message(BaseMessage::ai_from_blocks(vec![
        ContentBlock::text("readable answer"),
        ContentBlock::reasoning("readable reasoning"),
        ContentBlock::tool_use("child-tool", "Read", json!({"path":"child.rs"})),
        ContentBlock::ProviderState {
            provider: "openai_responses".into(),
            data: json!({"version":1,"items":[{"encrypted_content":"private-provider-state"}]}),
        },
    ]));
    fixture
        .cfg
        .session_resources
        .append_history(&child, &[payload])
        .await
        .unwrap();
    let (client, _, task) = spawn_loop(
        Arc::clone(&fixture.cfg),
        Arc::clone(&fixture.sessions),
        Some(child_caps()),
    )
    .await;
    let opened = child_request(
        &client,
        &fixture.sid,
        &child,
        json!({"kind":"open","direction":"before","limit":1}),
    )
    .await
    .unwrap();
    let wire = opened.to_string();
    for expected in [
        "readable answer",
        "readable reasoning",
        "child-tool",
        "child.rs",
    ] {
        assert!(
            wire.contains(expected),
            "missing readable projection: {expected}"
        );
    }
    assert!(!wire.contains("private-provider-state"));
    assert!(!wire.contains("provider_state"));
    task.abort();
}

#[tokio::test]
#[serial]
async fn child_history_new_child_live_state_and_structure_changes_invalidate_scope() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    let child = create_child(&fixture, &tmp, &fixture.sid).await;
    let (client, sessions, task) = spawn_loop(
        Arc::clone(&fixture.cfg),
        Arc::clone(&fixture.sessions),
        Some(child_caps()),
    )
    .await;
    let open = json!({"kind":"open","direction":"before","limit":2});
    let opened = child_request(&client, &fixture.sid, &child, open.clone())
        .await
        .unwrap();
    // Assemble a new live owner without invoking the read route's execution path.
    let live_tmp = tempfile::TempDir::new().unwrap();
    let live_fixture = PageFixture::new(&live_tmp).await;
    let mut child_state = live_fixture
        .sessions
        .lock()
        .await
        .remove(&live_fixture.sid)
        .unwrap();
    child_state.session_id = child.clone();
    child_state.thread_id = child.clone();
    sessions.lock().await.insert(child.clone(), child_state);
    assert!(child_page(&client, &fixture.sid, &child, &opened)
        .await
        .is_err());
    let opened = child_request(&client, &fixture.sid, &child, open)
        .await
        .unwrap();
    sessions
        .lock()
        .await
        .get_mut(&child)
        .unwrap()
        .structure_version += 1;
    assert!(child_page(&client, &fixture.sid, &child, &opened)
        .await
        .is_err());
    task.abort();
}

#[tokio::test]
#[serial]
async fn child_history_corrupt_large_reminder_is_rejected_before_descriptor_and_body() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    let child = create_child(&fixture, &tmp, &fixture.sid).await;
    let (client, _, task) = spawn_loop(
        Arc::clone(&fixture.cfg),
        Arc::clone(&fixture.sessions),
        Some(child_caps()),
    )
    .await;
    let opened = child_request(
        &client,
        &fixture.sid,
        &child,
        json!({"kind":"open","direction":"before","limit":1,"byteBudget":1}),
    )
    .await
    .unwrap();
    let entry = opened["items"][0]["entryId"].as_str().unwrap();
    let message = entry.rsplit(':').next().unwrap();
    let pool = sqlx::SqlitePool::connect_with(
        sqlx::sqlite::SqliteConnectOptions::new().filename(tmp.path().join("threads.db")),
    )
    .await
    .unwrap();
    for metadata in [false, true] {
        // Deliberately bypass the producer limits to model corrupt/old persisted input.
        let mut bad = json!({"version":1,"type":"system_reminder","id":message,"reminder":{
            "version":1,"category":"task","source":"test","kind":"notice","severity":"info",
            "delivery":"required","audiences":["tui"],"body":"","metadata":{}
        }});
        if metadata {
            bad["reminder"]["metadata"] = json!({"large":"x".repeat(3 * 1024 * 1024)});
        } else {
            bad["reminder"]["body"] = json!("x".repeat(3 * 1024 * 1024));
        }
        sqlx::query("UPDATE messages SET content = ?1 WHERE thread_id = ?2 AND message_id = ?3")
            .bind(bad.to_string())
            .bind(&child)
            .bind(message)
            .execute(&pool)
            .await
            .unwrap();
        drop(bad);
        let body = client.send_request("session/history/child/body", json!({
            "parentSessionId":fixture.sid,"childThreadId":child,"entryId":entry,
            "snapshotToken":opened["snapshotToken"],"readTicket":opened["readTicket"],"offset":0,"maxBytes":64,
        })).await;
        assert!(
            body.is_err(),
            "invalid large reminder must not be rendered through an old descriptor"
        );
        let page = child_request(
            &client,
            &fixture.sid,
            &child,
            json!({"kind":"open","direction":"before","limit":1,"byteBudget":1}),
        )
        .await;
        assert!(
            page.is_err(),
            "invalid large reminder must not receive a new descriptor"
        );
    }
    pool.close().await;
    task.abort();
}
