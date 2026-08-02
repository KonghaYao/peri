// ─── E2E 集成测试（需要 @peri-code/workflow 已安装）──────────────

use super::{workflow_local_dist_in, AgentExecutor, WorkflowInput, WorkflowRunner};
use crate::journal::WorkflowJournalStore;
use crate::progress::WorkflowProgressStore;
use crate::protocol::{AgentRunParams, AgentRunResult, Usage};
use std::sync::Arc;

/// Mock executor: 返回固定结果
struct MockAgentExecutor;

#[async_trait::async_trait]
impl AgentExecutor for MockAgentExecutor {
    async fn execute(&self, params: AgentRunParams) -> AgentRunResult {
        let preview = &params.prompt[..20.min(params.prompt.len())];
        AgentRunResult::Ok {
            output: format!("mock response to: {preview}").into(),
            usage: Usage { output_tokens: 10 },
            model: None,
            tool_count: None,
            token_count: None,
            phase: None,
            duration_ms: None,
        }
    }
}

#[test]
fn test_workflow_local_dist_missing() {
    let tmp = tempfile::TempDir::new().unwrap();
    assert!(workflow_local_dist_in(tmp.path()).is_none());
}

#[test]
fn test_workflow_local_dist_found() {
    let tmp = tempfile::TempDir::new().unwrap();
    let dist = tmp
        .path()
        .join("node_modules")
        .join("@peri-code")
        .join("workflow")
        .join("dist")
        .join("peri-workflow.js");
    std::fs::create_dir_all(dist.parent().unwrap()).unwrap();
    std::fs::write(&dist, "#!/usr/bin/env node\n").unwrap();
    let got = workflow_local_dist_in(tmp.path()).unwrap();
    assert_eq!(got, dist.to_string_lossy());
}

#[tokio::test]
#[ignore = "requires @peri-code/workflow installed"]
async fn test_e2e_simple_workflow() {
    let tmp = tempfile::TempDir::new().unwrap();
    let cwd = tmp.path().to_str().unwrap();

    let executor = Arc::new(MockAgentExecutor) as Arc<dyn AgentExecutor>;
    let runner = WorkflowRunner::new(executor, cwd, None);
    let journal = Arc::new(WorkflowJournalStore::new(cwd));
    let progress = Arc::new(WorkflowProgressStore::new());
    let (done_tx, mut done_rx) = tokio::sync::watch::channel(None);
    let (_kill_tx, kill_rx) = tokio::sync::oneshot::channel();

    let script = r#"
export const meta = { name: 'test-workflow', description: 'simple test' }
const result = await agent('say hello')
return { output: result }
"#;

    let input = WorkflowInput {
        script: script.to_string(),
        args: None,
        max_concurrency: 3,
        budget_total: None,
        workflow_name: "test-workflow".to_string(),
        resume_from: None,
    };

    let run_id = uuid::Uuid::now_v7().to_string();
    runner
        .run(run_id, input, progress, journal, done_tx, kill_rx)
        .await
        .unwrap();

    let _ = done_rx.changed().await; // 等待完成信号
    let result = done_rx.borrow().clone().unwrap();
    // 打印调试信息
    eprintln!("=== WORKFLOW RESULT ===");
    eprintln!("status: {}", result.status);
    eprintln!("error: {:?}", result.error);
    eprintln!("stderr_tail: {:?}", result.stderr_tail);
    eprintln!("========================");
    assert_eq!(result.status, "completed");
    // bunx 启动时会输出 "Resolving dependencies" 等正常信息到 stderr，
    // npx 不会。因此 stderr 非空也可能是正常情况。
    if let Some(ref stderr) = result.stderr_tail {
        // 仅当 stderr 不全是 bun 解析信息时才算异常
        let is_bunx_noise = stderr.lines().all(|l| {
            l.is_empty()
                || l.contains("Resolving dependencies")
                || l.contains("Resolved, downloaded and extracted")
                || l.contains("Saved lockfile")
        });
        assert!(is_bunx_noise, "stderr 含非预期的错误输出:\n{}", stderr);
    }
}
