use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use peri_acp_types::tools::{BaseTool, ToolContext};

use super::*;
use crate::protocol::{AgentRunParams, AgentRunResult, Usage};
use crate::runner::AgentExecutor;

struct CountingExecutor {
    calls: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl AgentExecutor for CountingExecutor {
    async fn execute(&self, _params: AgentRunParams) -> AgentRunResult {
        self.calls.fetch_add(1, Ordering::SeqCst);
        AgentRunResult::Ok {
            output: "unexpected".into(),
            usage: Usage { output_tokens: 1 },
            model: None,
            tool_count: None,
            token_count: None,
            phase: None,
            duration_ms: None,
        }
    }
}

fn make_tool(cwd: &str, calls: Arc<AtomicUsize>) -> (WorkflowTool, Arc<WorkflowTaskRegistry>) {
    let executor = Arc::new(CountingExecutor { calls }) as Arc<dyn AgentExecutor>;
    let runner = Arc::new(WorkflowRunner::new(executor, cwd, None));
    let (notification_tx, _) = tokio::sync::broadcast::channel(4);
    let registry = Arc::new(WorkflowTaskRegistry::new(notification_tx));
    let tool = WorkflowTool::new(
        runner,
        Arc::clone(&registry),
        Arc::new(WorkflowProgressStore::new()),
        Arc::new(WorkflowJournalStore::new(cwd)),
    );
    (tool, registry)
}

#[test]
fn workflow_schema_exposes_budget_total_integer_bounds() {
    let tmp = tempfile::TempDir::new().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let (tool, _) = make_tool(tmp.path().to_str().unwrap(), calls);

    let schema = tool.parameters();
    let budget = &schema["properties"]["budgetTotal"];

    assert_eq!(budget["type"], "integer");
    assert_eq!(budget["minimum"], 1);
    assert_eq!(budget["maximum"], MAX_SAFE_BUDGET_TOTAL);
    assert!(budget["description"]
        .as_str()
        .unwrap()
        .contains("total token budget"));
}

#[test]
fn parse_budget_total_accepts_omitted_and_safe_integer_bounds() {
    assert_eq!(parse_budget_total(&serde_json::json!({})).unwrap(), None);
    assert_eq!(
        parse_budget_total(&serde_json::json!({"budgetTotal": 1})).unwrap(),
        Some(1)
    );
    assert_eq!(
        parse_budget_total(&serde_json::json!({
            "budgetTotal": MAX_SAFE_BUDGET_TOTAL
        }))
        .unwrap(),
        Some(MAX_SAFE_BUDGET_TOTAL)
    );
}

#[test]
fn parse_budget_total_rejects_invalid_values_with_stable_range_error() {
    let invalid = [
        serde_json::json!(null),
        serde_json::json!(0),
        serde_json::json!(-1),
        serde_json::json!(1.5),
        serde_json::json!("1000"),
        serde_json::json!(true),
        serde_json::json!(MAX_SAFE_BUDGET_TOTAL + 1),
    ];

    for value in invalid {
        let error = parse_budget_total(&serde_json::json!({"budgetTotal": value})).unwrap_err();
        assert_eq!(
            error,
            format!("'budgetTotal' must be an integer between 1 and {MAX_SAFE_BUDGET_TOTAL}")
        );
    }
}

#[test]
fn parse_host_limits_rejects_invalid_values() {
    for field in ["maxAgents", "maxToolCalls", "maxElapsedMs"] {
        let input = serde_json::json!({(field): 0});
        let error = parse_bounded_integer(&input, field, None, MAX_SAFE_INTEGER).unwrap_err();
        assert!(error.contains(field));
    }
    assert!(parse_bounded_integer(
        &serde_json::json!({"maxConcurrency": 17}),
        "maxConcurrency",
        Some(3),
        MAX_CONCURRENCY_CAP,
    )
    .is_err());
}

#[tokio::test]
async fn invalid_script_fails_before_workflow_side_effects() {
    let tmp = tempfile::TempDir::new().unwrap();
    let cwd = tmp.path().to_str().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let (tool, registry) = make_tool(cwd, Arc::clone(&calls));

    let error = tool
        .invoke(
            serde_json::json!({
                "script": "export const meta = { name: 'broken', description: 'test' }; const = nope"
            }),
            ToolContext::new(&[], cwd),
        )
        .await
        .unwrap_err();

    assert!(error.to_string().contains("Workflow preflight failed"));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(registry.active_count(), 0);
    assert!(!tmp.path().join(".claude/workflow-runs").exists());
}

#[tokio::test]
async fn strict_preflight_fails_before_workflow_side_effects() {
    let tmp = tempfile::TempDir::new().unwrap();
    let cwd = tmp.path().to_str().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let (tool, registry) = make_tool(cwd, Arc::clone(&calls));

    let error = tool
        .invoke(
            serde_json::json!({
                "script": "export const meta = { name: 'strict' }",
                "strictPreflight": true
            }),
            ToolContext::new(&[], cwd),
        )
        .await
        .unwrap_err();

    assert!(error.to_string().contains("cannot be statically validated"));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(registry.active_count(), 0);
    assert!(!tmp.path().join(".claude/workflow-runs").exists());
}

#[tokio::test]
async fn invalid_write_intent_fails_before_workflow_side_effects() {
    let tmp = tempfile::TempDir::new().unwrap();
    let cwd = tmp.path().to_str().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let (tool, registry) = make_tool(cwd, Arc::clone(&calls));

    let error = tool
        .invoke(
            serde_json::json!({
                "script": "export const meta = { name: 'invalid-intent' }",
                "writeIntent": {"kind": "write", "cwd": cwd}
            }),
            ToolContext::new(&[], cwd),
        )
        .await
        .unwrap_err();

    assert!(error.to_string().contains("Invalid writeIntent"));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(registry.active_count(), 0);
    assert!(!tmp.path().join(".claude/workflow-runs").exists());
}

#[tokio::test]
async fn invalid_budget_fails_before_workflow_side_effects() {
    let tmp = tempfile::TempDir::new().unwrap();
    let cwd = tmp.path().to_str().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let (tool, registry) = make_tool(cwd, Arc::clone(&calls));
    let resume_id = uuid::Uuid::now_v7().to_string();

    let error = tool
        .invoke(
            serde_json::json!({
                "script": "export const meta = { name: 'invalid-budget', description: 'test' }; return 'ok'",
                "budgetTotal": 0,
                "resumeFromRunId": resume_id
            }),
            ToolContext::new(&[], cwd),
        )
        .await
        .unwrap_err();

    assert!(error.to_string().contains("'budgetTotal'"));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(registry.active_count(), 0);
    assert!(!tmp.path().join(".claude/workflow-runs").exists());
}
