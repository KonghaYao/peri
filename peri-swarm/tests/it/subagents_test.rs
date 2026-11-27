//! `sessions.subagents.list/retrieve` against a real Peri SQLite session store.
//!
//! The fixture writes a root tree through the real `Resources` facade
//! (`create_session` / `save_child` / `update_session_meta`), seeds the Swarm
//! row with its root identity, and then drives the real axum router over an
//! `AcpSwarmService`. No ACP host is spawned: these routes only read persisted
//! child-thread metadata.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use axum::Router;
use peri_acp_types::session_resources::{
    ChildSnapshot, FrozenSnapshotBytes, FrozenState, NewSession, NewSessionMeta, SessionMetaPatch,
    SessionResources, SessionStoreShutdownPort,
};
use peri_acp_types::thread::AgentStatus;
use peri_acp_types::workspace::{ResolvedWorkspace, SessionBinding};
use peri_resources::Resources;
use peri_swarm::actor::service::{AcpSwarmService, ActorServiceConfig};
use peri_swarm::config::{SwarmConfig, API_KEY_ENV, WORKSPACE_ROOT_ENV};
use peri_swarm::domain::{EnvironmentRef, SessionState};
use peri_swarm::http::{build_router, AppState};
use peri_swarm::store::{MemorySwarmStore, SwarmSessionRow, SwarmStore};
use serde_json::{json, Value};
use tempfile::TempDir;
use tower::ServiceExt;

const TOKEN: &str = "test-deployment-key";

const LOGICAL: &str = "agent-subagents";
const OTHER_LOGICAL: &str = "agent-other";
const EMPTY_LOGICAL: &str = "agent-empty";

const ROOT: &str = "thread-root-a";
const DIRECT: &str = "thread-child-a";
const NESTED: &str = "thread-child-b";
const CLOSED: &str = "thread-child-c";

const OTHER_ROOT: &str = "thread-root-other";
const OTHER_CHILD: &str = "thread-child-other";

const EMPTY_ROOT: &str = "thread-root-empty";

const ROOT_AT: &str = "2026-01-01T00:00:00Z";
const DIRECT_AT: &str = "2026-01-01T00:00:01Z";
const NESTED_AT: &str = "2026-01-01T00:00:02Z";
const CLOSED_AT: &str = "2026-01-01T00:00:03Z";
const OTHER_AT: &str = "2026-01-01T00:00:04Z";

struct Fixture {
    router: Router,
    store: Arc<MemorySwarmStore>,
    closed_at: i64,
    _workspace: TempDir,
    _db: TempDir,
}

fn unix(rfc3339: &str) -> i64 {
    chrono::DateTime::parse_from_rfc3339(rfc3339)
        .expect("fixture timestamp")
        .timestamp()
}

fn git(root: &Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", root)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .expect("git fixture");
    assert!(
        output.status.success(),
        "Git fixture failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn repository() -> TempDir {
    let directory = tempfile::tempdir().expect("repository");
    git(directory.path(), &["init", "-q"]);
    git(
        directory.path(),
        &[
            "-c",
            "user.name=fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--allow-empty",
            "-qm",
            "base",
        ],
    );
    directory
}

fn root_session(id: &str, workspace: &ResolvedWorkspace, at: &str) -> NewSession {
    NewSession {
        thread_id: id.to_owned(),
        created_at: at.to_owned(),
        meta: NewSessionMeta {
            title: Some(format!("root {id}")),
            cwd: workspace.cwd.to_string_lossy().into_owned(),
            parent_thread_id: None,
            hidden: false,
            cancel_policy: Default::default(),
            snapshot_at_message_id: None,
        },
        binding: SessionBinding::from_workspace(workspace),
        frozen: FrozenSnapshotBytes::new(format!(r#"{{"v":1,"id":"{id}"}}"#)),
    }
}

async fn save_child(
    resources: &dyn SessionResources,
    root: &str,
    parent: &str,
    id: &str,
    title: Option<&str>,
    at: &str,
    workspace: &ResolvedWorkspace,
) {
    let root_frozen = match resources
        .load_session_snapshot(&root.to_owned())
        .await
        .expect("root snapshot")
        .frozen
    {
        FrozenState::Present(frozen) => frozen,
        other => panic!("root frozen must be present: {other:?}"),
    };
    let target = NewSession {
        thread_id: id.to_owned(),
        created_at: at.to_owned(),
        meta: NewSessionMeta {
            title: title.map(str::to_owned),
            cwd: workspace.cwd.to_string_lossy().into_owned(),
            parent_thread_id: Some(parent.to_owned()),
            // Every child is hidden from `sessions.list`; the official
            // subagent list must still include it.
            hidden: true,
            cancel_policy: Default::default(),
            snapshot_at_message_id: None,
        },
        binding: SessionBinding::from_workspace(workspace),
        frozen: root_frozen,
    };
    resources
        .save_child(&ChildSnapshot {
            target,
            parent_id: parent.to_owned(),
            root_id: root.to_owned(),
            inherited: Default::default(),
        })
        .await
        .expect("save child");
}

async fn seed_session(
    store: &MemorySwarmStore,
    logical_id: &str,
    root_session_id: &str,
    cwd: &Path,
) {
    store
        .swarm_upsert_session(SwarmSessionRow {
            logical_id: logical_id.to_owned(),
            root_session_id: root_session_id.to_owned(),
            actor_key: format!("agent:{logical_id}"),
            cwd: cwd.to_string_lossy().into_owned(),
            state: SessionState::Idle,
            model: None,
            instructions: None,
            mcp_tools: Vec::new(),
            metadata: Default::default(),
            prompt_cache_options: None,
            environment: EnvironmentRef::none(),
            created_at: ROOT_AT.to_owned(),
            last_activity_at: ROOT_AT.to_owned(),
            last_item_seq: 0,
        })
        .await
        .expect("seed swarm session");
}

async fn fixture() -> Fixture {
    let repo = repository();
    let workspace_root = tempfile::tempdir().expect("workspace root");
    let db = tempfile::tempdir().expect("session store");
    let db_path: PathBuf = db.path().join("threads.db");

    let resources = Resources::open_with(Some(db_path.clone()))
        .await
        .expect("open session store");
    let (facade, owner) = resources.into_parts();
    let workspace = facade
        .resolve_workspace(repo.path())
        .await
        .expect("resolve workspace");

    // Root A: direct child, nested grandchild (workflow-style), closed child.
    facade
        .create_session(&root_session(ROOT, &workspace, ROOT_AT))
        .await
        .expect("root A");
    save_child(
        facade.as_ref(),
        ROOT,
        ROOT,
        DIRECT,
        Some("explorer"),
        DIRECT_AT,
        &workspace,
    )
    .await;
    save_child(
        facade.as_ref(),
        ROOT,
        DIRECT,
        NESTED,
        Some("workflow: review the patch"),
        NESTED_AT,
        &workspace,
    )
    .await;
    save_child(
        facade.as_ref(),
        ROOT,
        ROOT,
        CLOSED,
        Some("closed helper"),
        CLOSED_AT,
        &workspace,
    )
    .await;
    facade
        .update_session_meta(
            &CLOSED.to_owned(),
            &SessionMetaPatch {
                status: Some(AgentStatus::Done),
                ..SessionMetaPatch::default()
            },
        )
        .await
        .expect("close child");
    let closed_meta = facade
        .load_session_meta(&CLOSED.to_owned())
        .await
        .expect("closed meta");
    assert!(!closed_meta.agent_status.is_active());
    let closed_at = closed_meta.updated_at.timestamp();

    // Root B: one child that must not be visible from session A.
    facade
        .create_session(&root_session(OTHER_ROOT, &workspace, ROOT_AT))
        .await
        .expect("root B");
    save_child(
        facade.as_ref(),
        OTHER_ROOT,
        OTHER_ROOT,
        OTHER_CHILD,
        Some("other session child"),
        OTHER_AT,
        &workspace,
    )
    .await;

    // Root C: no children, so a known session has an empty page.
    facade
        .create_session(&root_session(EMPTY_ROOT, &workspace, ROOT_AT))
        .await
        .expect("root C");

    owner.shutdown().await.expect("close fixture store");
    drop(facade);

    let store = Arc::new(MemorySwarmStore::new());
    seed_session(&store, LOGICAL, ROOT, &workspace.cwd).await;
    seed_session(&store, OTHER_LOGICAL, OTHER_ROOT, &workspace.cwd).await;
    seed_session(&store, EMPTY_LOGICAL, EMPTY_ROOT, &workspace.cwd).await;

    let env = HashMap::from([
        (API_KEY_ENV.to_owned(), TOKEN.to_owned()),
        (
            WORKSPACE_ROOT_ENV.to_owned(),
            workspace_root.path().to_string_lossy().into_owned(),
        ),
    ]);
    let config = SwarmConfig::from_env_map(|name| env.get(name).cloned()).expect("config");
    let service = Arc::new(AcpSwarmService::new(
        Arc::clone(&store) as Arc<dyn SwarmStore>,
        ActorServiceConfig {
            session_store_path: Some(db_path),
            ..ActorServiceConfig::default()
        },
    ));

    Fixture {
        router: build_router(AppState::new(service, Arc::new(config))),
        store,
        closed_at,
        _workspace: workspace_root,
        _db: db,
    }
}

fn request(method: Method, uri: &str, token: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(token) = token {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    builder.body(Body::empty()).expect("request")
}

async fn get(router: &Router, uri: &str) -> (StatusCode, Value) {
    let response = router
        .clone()
        .oneshot(request(Method::GET, uri, Some(TOKEN)))
        .await
        .expect("response");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or_else(|_| {
            panic!(
                "non-JSON response ({status}): {}",
                String::from_utf8_lossy(&bytes)
            )
        })
    };
    (status, value)
}

fn ids(page: &Value) -> Vec<String> {
    page["data"]
        .as_array()
        .expect("data array")
        .iter()
        .map(|item| item["id"].as_str().expect("id").to_owned())
        .collect()
}

#[tokio::test]
async fn subagents_list_projects_direct_nested_and_closed_children() {
    let fixture = fixture().await;
    let (status, page) = get(
        &fixture.router,
        &format!("/v1/agents/sessions/{LOGICAL}/subagents"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(page["object"], json!("list"));
    assert_eq!(page["has_more"], json!(false));
    assert_eq!(page["hasMore"], json!(false));
    assert!(page["nextCursor"].is_null(), "{page}");
    // Default desc, over (opened_at, id); the root itself is absent.
    assert_eq!(ids(&page), vec![CLOSED, NESTED, DIRECT]);
    assert!(!ids(&page).contains(&ROOT.to_owned()));

    let direct = &page["data"][2];
    assert_eq!(direct["id"], json!(DIRECT));
    assert_eq!(direct["object"], json!("agent.session.subagent"));
    assert_eq!(direct["session_id"], json!(LOGICAL));
    assert_eq!(direct["parent_agent_id"], json!("root"));
    assert_eq!(direct["name"], json!("explorer"));
    assert_eq!(direct["instructions"], Value::Null);
    assert_eq!(direct["opened_at"], json!(unix(DIRECT_AT)));
    assert!(direct["closed_at"].is_null());
    assert_eq!(direct["status"], json!("active"));

    let nested = &page["data"][1];
    assert_eq!(nested["id"], json!(NESTED));
    assert_eq!(nested["parent_agent_id"], json!(DIRECT));
    assert_eq!(nested["name"], json!("workflow: review the patch"));
    assert!(nested["closed_at"].is_null());
    assert_eq!(nested["status"], json!("active"));

    let closed = &page["data"][0];
    assert_eq!(closed["id"], json!(CLOSED));
    assert_eq!(closed["parent_agent_id"], json!("root"));
    assert_eq!(closed["status"], json!("closed"));
    assert_eq!(closed["opened_at"], json!(unix(CLOSED_AT)));
    assert_eq!(closed["closed_at"], json!(fixture.closed_at));
    assert!(fixture.closed_at >= unix(CLOSED_AT));

    // Ascending order reverses the same stable total order.
    let (status, ascending) = get(
        &fixture.router,
        &format!("/v1/agents/sessions/{LOGICAL}/subagents?order=asc"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{ascending}");
    assert_eq!(ids(&ascending), vec![DIRECT, NESTED, CLOSED]);

    // Retrieve returns the same single object.
    let (status, single) = get(
        &fixture.router,
        &format!("/v1/agents/sessions/{LOGICAL}/subagents/{NESTED}"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{single}");
    assert_eq!(single, nested.clone());

    // Hidden children are included; the root is not a subagent.
    let (status, root) = get(
        &fixture.router,
        &format!("/v1/agents/sessions/{LOGICAL}/subagents/{ROOT}"),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{root}");
    assert_eq!(root["error"]["code"], json!("unknown_agent"));
}

#[tokio::test]
async fn subagents_list_paginates_and_rejects_unknown_cursors() {
    let fixture = fixture().await;
    let base = format!("/v1/agents/sessions/{LOGICAL}/subagents");

    let (status, first) = get(&fixture.router, &format!("{base}?limit=2")).await;
    assert_eq!(status, StatusCode::OK, "{first}");
    assert_eq!(ids(&first), vec![CLOSED, NESTED]);
    assert_eq!(first["has_more"], json!(true));
    assert_eq!(first["nextCursor"], json!(NESTED));

    let (status, second) = get(&fixture.router, &format!("{base}?limit=2&after={NESTED}")).await;
    assert_eq!(status, StatusCode::OK, "{second}");
    assert_eq!(ids(&second), vec![DIRECT]);
    assert_eq!(second["has_more"], json!(false));
    assert!(second["nextCursor"].is_null(), "{second}");

    let (status, ascending) = get(
        &fixture.router,
        &format!("{base}?order=asc&limit=1&after={DIRECT}"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{ascending}");
    assert_eq!(ids(&ascending), vec![NESTED]);
    assert_eq!(ascending["has_more"], json!(true));
    assert_eq!(ascending["nextCursor"], json!(NESTED));

    let root_cursor = format!("after={ROOT}");
    for query in [
        "after=does-not-exist",
        root_cursor.as_str(),
        "limit=0",
        "limit=501",
        "order=sideways",
    ] {
        let (status, error) = get(&fixture.router, &format!("{base}?{query}")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{query}: {error}");
        assert_eq!(error["error"]["code"], json!("invalid_query"), "{query}");
        if query.starts_with("after=") || query.starts_with("limit=") {
            assert!(
                error["error"]["details"]["field"].is_string(),
                "{query}: {error}"
            );
        }
    }

    // A known session with no children is an empty page, not a 404.
    let (status, empty) = get(
        &fixture.router,
        &format!("/v1/agents/sessions/{EMPTY_LOGICAL}/subagents"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{empty}");
    assert_eq!(empty["data"], json!([]));
    assert_eq!(empty["has_more"], json!(false));
    assert!(empty["nextCursor"].is_null());
}

#[tokio::test]
async fn subagents_unknown_session_and_cross_session_are_404() {
    let fixture = fixture().await;

    let (status, error) = get(
        &fixture.router,
        "/v1/agents/sessions/no-such-agent/subagents",
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{error}");
    assert_eq!(error["error"]["code"], json!("unknown_agent"));

    let (status, error) = get(
        &fixture.router,
        "/v1/agents/sessions/no-such-agent/subagents/thread-child-a",
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{error}");
    assert_eq!(error["error"]["code"], json!("unknown_agent"));

    let (status, error) = get(
        &fixture.router,
        &format!("/v1/agents/sessions/{LOGICAL}/subagents/{OTHER_CHILD}"),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{error}");
    assert_eq!(error["error"]["code"], json!("unknown_agent"));

    let (status, error) = get(
        &fixture.router,
        &format!("/v1/agents/sessions/{OTHER_LOGICAL}/subagents/{DIRECT}"),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{error}");
    assert_eq!(error["error"]["code"], json!("unknown_agent"));

    let (status, error) = get(
        &fixture.router,
        &format!("/v1/agents/sessions/{EMPTY_LOGICAL}/subagents/{DIRECT}"),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{error}");
    assert_eq!(error["error"]["code"], json!("unknown_agent"));
}

#[tokio::test]
async fn subagents_routes_require_bearer_and_unimplemented_children_stay_404() {
    let fixture = fixture().await;
    let base = format!("/v1/agents/sessions/{LOGICAL}/subagents");

    for uri in [base.clone(), format!("{base}/{DIRECT}")] {
        let response = fixture
            .router
            .clone()
            .oneshot(request(Method::GET, &uri, None))
            .await
            .expect("response");
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "the Bearer gate must run first: {uri}"
        );
    }

    for (uri, resource) in [
        (format!("{base}/{DIRECT}/items"), "sessions.subagents.items"),
        (format!("{base}/{DIRECT}/turns"), "sessions.subagents.turns"),
        (
            format!("{base}/{DIRECT}/turns/turn_1"),
            "sessions.subagents.turns",
        ),
        (
            format!("{base}/{DIRECT}/turns/turn_1/items"),
            "sessions.subagents.turns.items",
        ),
    ] {
        let (status, error) = get(&fixture.router, &uri).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{uri}: {error}");
        assert_eq!(
            error["error"]["code"],
            json!("unsupported_contract"),
            "{uri}"
        );
        assert_eq!(error["error"]["details"]["resource"], json!(resource));
    }

    // The fixture's unrelated Swarm row stays untouched by read-only routes.
    let row = fixture
        .store
        .swarm_get_session(LOGICAL)
        .await
        .expect("store read")
        .expect("row");
    assert_eq!(row.root_session_id, ROOT);
}
