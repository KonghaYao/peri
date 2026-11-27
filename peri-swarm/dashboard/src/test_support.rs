//! Shared fixtures for unit tests.
//!
//! Compiled only under `cfg(test)`: the shipped binary never contains demo
//! data. No fixture contains an API key other than the obviously fake
//! [`SECRET`] used to prove redaction.

use std::collections::BTreeMap;
use std::time::Instant;

use serde_json::json;

use crate::api::{ApiError, Session, SessionAgent, SessionEnvironment, SessionPage, SessionTurn};
use crate::app::{App, Effect, Intent};
use crate::config::{self, Config, ConfigStore};
use crate::worker::WorkerEvent;

/// Obviously fake key used to prove it is never rendered.
pub const SECRET: &str = "test-key-never-displayed";

/// App connected to the default local origin with a saved config.
#[must_use]
pub fn connected_app() -> (App, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = ConfigStore::at(dir.path().join("config.toml"));
    let config = Config::new(config::DEFAULT_BASE_URL, SECRET).expect("config");
    store.save(&config).expect("save");
    (App::connected(config, Vec::new(), store), dir)
}

/// One session with the fields the session log reads.
#[must_use]
pub fn session(
    id: &str,
    status: &str,
    state: &str,
    model: Option<&str>,
    last_active: i64,
) -> Session {
    Session {
        id: id.to_owned(),
        logical_id: id.to_owned(),
        root_session_id: format!("{id}-root"),
        cwd: "/tmp/work".to_owned(),
        state: state.to_owned(),
        status: status.to_owned(),
        turn: SessionTurn::default(),
        last_item_seq: 1,
        created_at: last_active.saturating_sub(10),
        last_active_at: last_active,
        agent: SessionAgent {
            model: model.map(str::to_owned),
            instructions: None,
        },
        environment: SessionEnvironment {
            kind: "none".to_owned(),
            id: None,
        },
        sandbox: None,
        required_actions: Vec::new(),
        vault_ids: Vec::new(),
        usage: None,
        error: None,
        metadata: BTreeMap::new(),
        prompt_cache_options: None,
    }
}

/// Attach metadata entries.
#[must_use]
pub fn with_metadata(mut session: Session, entries: &[(&str, &str)]) -> Session {
    for (key, value) in entries {
        session
            .metadata
            .insert((*key).to_owned(), (*value).to_owned());
    }
    session
}

/// Attach `count` pending MCP approval requests.
#[must_use]
pub fn with_approvals(mut session: Session, count: usize) -> Session {
    session.required_actions = (0..count)
        .map(|index| {
            json!({
                "type": "mcp_approval_request",
                "request_id": format!("appr_{index}"),
                "turn_id": "turn_1",
                "request": { "type": "mcp_tool_call", "name": "search" }
            })
        })
        .collect();
    session
}

/// Attach an environment id.
#[must_use]
pub fn with_environment(mut session: Session, environment_id: &str) -> Session {
    session.environment.id = Some(environment_id.to_owned());
    session
}

/// One page of sessions.
#[must_use]
pub fn page(data: Vec<Session>) -> SessionPage {
    SessionPage {
        object: Some("list".to_owned()),
        data,
        has_more: false,
        next_cursor: None,
    }
}

/// The single list request in `effects`.
#[must_use]
pub fn list_request(effects: &[Effect]) -> (u64, u64, u32) {
    match effects {
        [Effect::ListSessions {
            epoch,
            generation,
            limit,
        }] => (*epoch, *generation, *limit),
        _ => panic!("expected exactly one ListSessions effect"),
    }
}

/// Finish the in-flight list request with a page.
pub fn finish_page(app: &mut App, epoch: u64, generation: u64, limit: u32, data: Vec<Session>) {
    app.handle_worker(WorkerEvent::SessionsLoaded {
        epoch,
        generation,
        limit,
        result: Ok(page(data)),
    });
}

/// Trigger the first list fetch, then finish it with `data`.
pub fn feed_page(app: &mut App, data: Vec<Session>) {
    let effects: Vec<Effect> = app.session_list_due(Instant::now()).into_iter().collect();
    let (epoch, generation, limit) = list_request(&effects);
    finish_page(app, epoch, generation, limit, data);
}

/// Trigger a user refresh, then finish it with `data`.
pub fn refresh_page(app: &mut App, data: Vec<Session>) {
    let effects = app.apply(Intent::RefreshSessions);
    let (epoch, generation, limit) = list_request(&effects);
    finish_page(app, epoch, generation, limit, data);
}

/// Trigger the first list fetch, then fail it.
pub fn fail_first_page(app: &mut App, error: ApiError) {
    let effects: Vec<Effect> = app.session_list_due(Instant::now()).into_iter().collect();
    let (epoch, generation, limit) = list_request(&effects);
    app.handle_worker(WorkerEvent::SessionsLoaded {
        epoch,
        generation,
        limit,
        result: Err(error),
    });
}
