//! Tests for mod_attrib

use super::*;

struct BranchReader {
    responses: Mutex<
        std::collections::VecDeque<Result<Option<String>, crate::workspace_io::WorkspaceReadError>>,
    >,
}

#[async_trait]
impl crate::workspace_io::WorkspaceFileReader for BranchReader {
    async fn read_text(
        &self,
        _path: &std::path::Path,
    ) -> Result<String, crate::workspace_io::WorkspaceReadError> {
        Err(crate::workspace_io::WorkspaceReadError::Unavailable)
    }

    async fn current_branch(
        &self,
    ) -> Result<Option<String>, crate::workspace_io::WorkspaceReadError> {
        self.responses.lock().unwrap().pop_front().unwrap()
    }
}

#[tokio::test]
async fn before_agent_observes_workspace_branch_without_host_cwd() {
    let reader = Arc::new(BranchReader {
        responses: Mutex::new(std::collections::VecDeque::from([
            Ok(Some("remote-main".into())),
            Err(crate::workspace_io::WorkspaceReadError::Unavailable),
            Ok(None),
            Ok(Some("remote-feature".into())),
        ])),
    });
    let middleware = GitAttributionMiddleware::new("test-model", reader);
    let mut state = peri_agent::agent::state::AgentState::default();
    state.cwd = "/nonexistent-host-directory".into();
    middleware.before_agent(&mut state).await.unwrap();
    assert_eq!(
        *middleware.branch_baseline.lock().unwrap(),
        Some("remote-main".into())
    );
    for _ in 0..2 {
        middleware.before_agent(&mut state).await.unwrap();
        assert_eq!(
            *middleware.branch_baseline.lock().unwrap(),
            Some("remote-main".into())
        );
    }
    middleware.before_agent(&mut state).await.unwrap();
    assert_eq!(
        *middleware.branch_baseline.lock().unwrap(),
        Some("remote-feature".into())
    );
}

#[tokio::test]
async fn default_reader_branch_is_unavailable() {
    struct TextOnlyReader;
    #[async_trait]
    impl crate::workspace_io::WorkspaceFileReader for TextOnlyReader {
        async fn read_text(
            &self,
            _path: &std::path::Path,
        ) -> Result<String, crate::workspace_io::WorkspaceReadError> {
            Ok("text".into())
        }
    }
    assert_eq!(
        crate::workspace_io::WorkspaceFileReader::current_branch(&TextOnlyReader).await,
        Err(crate::workspace_io::WorkspaceReadError::Unavailable)
    );
}

fn test_middleware() -> GitAttributionMiddleware {
    GitAttributionMiddleware::new(
        "test-model",
        Arc::new(crate::workspace_io::McpWorkspaceFileReader::new(
            None,
            None,
            &std::collections::HashSet::new(),
        )),
    )
}

#[test]
fn test_git_attribution_reset_clears_pending() {
    let mw = test_middleware();
    // 插入一些待处理内容
    mw.pending_old_content
        .lock()
        .unwrap()
        .insert("file1.rs".to_string(), "old content".to_string());
    mw.pending_old_content
        .lock()
        .unwrap()
        .insert("file2.rs".to_string(), "more content".to_string());
    assert_eq!(mw.pending_old_content.lock().unwrap().len(), 2);

    // reset 后应清空
    mw.reset();
    assert!(mw.pending_old_content.lock().unwrap().is_empty());
}

#[test]
fn test_branch_drift_reports_each_change_once() {
    let mw = test_middleware();

    assert_eq!(mw.observe_branch("main".to_string()), None);
    assert_eq!(mw.observe_branch("main".to_string()), None);
    assert_eq!(
        mw.observe_branch("feature".to_string()),
        Some(("main".to_string(), "feature".to_string()))
    );
    assert_eq!(mw.observe_branch("feature".to_string()), None);
}
