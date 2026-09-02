//! Integration tests for GitWatchMiddleware.

use std::{process::Command as StdCommand, time::Duration};

use peri_agent::{
    agent::react::{ToolCall, ToolResult},
    middleware::r#trait::Middleware,
    session::MessageQueue,
};
use tempfile::tempdir;

use super::GitWatchMiddleware;

struct TestState {
    cwd: String,
    queue: MessageQueue,
}

impl peri_agent::middleware::state::MiddlewareState for TestState {
    fn cwd(&self) -> &str {
        &self.cwd
    }

    fn messages(&self) -> &[peri_agent::messages::BaseMessage] {
        &[]
    }

    fn add_message(&mut self, _message: peri_agent::messages::BaseMessage) {}

    fn prepend_message(&mut self, _message: peri_agent::messages::BaseMessage) {}

    fn messages_mut(&mut self) -> &mut Vec<peri_agent::messages::BaseMessage> {
        panic!("not used in git_watch tests");
    }

    fn current_step(&self) -> usize {
        0
    }

    #[allow(deprecated)]
    fn set_cwd(&mut self, cwd: String) {
        self.cwd = cwd;
    }

    #[allow(deprecated)]
    fn set_current_step(&mut self, _step: usize) {}

    fn get_context(&self, _key: &str) -> Option<&str> {
        None
    }

    fn set_context(&mut self, _key: String, _value: String) {}

    fn token_tracker(&self) -> &peri_agent::agent::token::TokenTracker {
        panic!("not used");
    }

    fn token_tracker_mut(&mut self) -> &mut peri_agent::agent::token::TokenTracker {
        panic!("not used");
    }

    fn push_recall(&mut self, _item: String) {}

    fn drain_recall(&mut self) -> Vec<String> {
        vec![]
    }

    fn ancestor_len(&self) -> usize {
        0
    }

    #[allow(deprecated)]
    fn store(&self) -> Option<&std::sync::Arc<dyn peri_agent::thread::ThreadStore>> {
        None
    }

    #[allow(deprecated)]
    fn own_thread_id(&self) -> Option<&peri_agent::thread::ThreadId> {
        None
    }

    fn v2_queue(&self) -> &MessageQueue {
        &self.queue
    }
}

fn init_git_repo(path: &std::path::Path) {
    StdCommand::new("git")
        .args(["init", "-b", "main"])
        .current_dir(path)
        .output()
        .expect("git init");
    StdCommand::new("git")
        .args(["config", "user.email", "test@example.com"])
        .current_dir(path)
        .output()
        .expect("git config email");
    StdCommand::new("git")
        .args(["config", "user.name", "Test"])
        .current_dir(path)
        .output()
        .expect("git config name");
    std::fs::write(path.join("README.md"), "hi").unwrap();
    StdCommand::new("git")
        .args(["add", "README.md"])
        .current_dir(path)
        .output()
        .expect("git add");
    StdCommand::new("git")
        .args(["commit", "-m", "init"])
        .current_dir(path)
        .output()
        .expect("git commit");
}

#[tokio::test]
async fn git_watch_notifies_on_new_commit() {
    let dir = tempdir().unwrap();
    init_git_repo(dir.path());

    let mw = GitWatchMiddleware::with_throttle_for_test(Duration::ZERO);
    let queue = MessageQueue::new();
    let mut state = TestState {
        cwd: dir.path().to_string_lossy().into_owned(),
        queue: queue.clone(),
    };

    let dummy_call = ToolCall::new("0", "Read", serde_json::json!({}));
    let dummy_ok = ToolResult::success("0", "Read", "ok");

    mw.after_tool(&mut state, &dummy_call, &dummy_ok)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(queue.drain_all().is_empty(), "baseline should not notify");

    std::fs::write(dir.path().join("README.md"), "changed").unwrap();
    StdCommand::new("git")
        .args(["commit", "-am", "second"])
        .current_dir(dir.path())
        .output()
        .expect("git commit");

    let dummy_call = ToolCall::new("1", "Read", serde_json::json!({}));
    let dummy_result = ToolResult::success("1", "Read", "ok");

    mw.after_tool(&mut state, &dummy_call, &dummy_result)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    let msgs = queue.drain_all();
    assert_eq!(msgs.len(), 1, "expected one Info on HEAD change");
    let debug = format!("{:?}", msgs[0].message);
    assert!(debug.contains("[Git watch]"));
    assert!(debug.contains("HEAD"));
}

#[tokio::test]
async fn non_git_repo_stays_quiet() {
    let dir = tempdir().unwrap();
    let mw = GitWatchMiddleware::with_throttle_for_test(Duration::ZERO);
    let queue = MessageQueue::new();
    let mut state = TestState {
        cwd: dir.path().to_string_lossy().into_owned(),
        queue: queue.clone(),
    };

    mw.after_tool(
        &mut state,
        &ToolCall::new("1", "Read", serde_json::json!({})),
        &ToolResult::success("1", "Read", "ok"),
    )
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;

    assert!(queue.drain_all().is_empty());
}
