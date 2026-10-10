//! Tests for tools_cron

use std::sync::Arc;

use parking_lot::Mutex;
use tokio::sync::mpsc;

use super::*;
use crate::CronScheduler;

fn new_tools() -> (CronRegisterTool, CronListTool, CronRemoveTool) {
    let (tx, _rx) = mpsc::unbounded_channel();
    let scheduler = Arc::new(Mutex::new(CronScheduler::new(tx)));
    (
        CronRegisterTool::new(scheduler.clone()),
        CronListTool::new(scheduler.clone()),
        CronRemoveTool::new(scheduler),
    )
}

#[tokio::test]
async fn test_register_rejects_empty_prompt() {
    let (reg, _, _) = new_tools();
    let result = reg
        .invoke(
            serde_json::json!({"expression": "* * * * *", "prompt": ""}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await;
    assert!(result.is_err(), "空 prompt 应被拒绝");
}

#[tokio::test]
async fn test_register_rejects_whitespace_prompt() {
    let (reg, _, _) = new_tools();
    let result = reg
        .invoke(
            serde_json::json!({"expression": "* * * * *", "prompt": "   "}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await;
    assert!(result.is_err(), "纯空白 prompt 应被拒绝");
}

#[tokio::test]
async fn test_register_success() {
    let (reg, list, _) = new_tools();
    let result = reg
        .invoke(
            serde_json::json!({"expression": "* * * * *", "prompt": "test task"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert!(result.contains("已注册"));

    let list_result = list
        .invoke(
            serde_json::json!({}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert!(list_result.contains("test task"));
}

/// M13：创建端必须校验可承载预算，超长任务显式拒绝并给出限制，
/// 不允许"创建成功、触发时才失败"。
#[tokio::test]
async fn test_register_rejects_prompt_over_the_carriable_budget() {
    let (reg, _, _) = new_tools();
    let prompt = "汉".repeat(peri_acp_types::cron::MAX_CRON_PROMPT_BYTES / 3 + 1);
    let result = reg
        .invoke(
            serde_json::json!({"expression": "* * * * *", "prompt": prompt}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await;
    let error = result.expect_err("超长 prompt 必须被拒绝");
    let message = error.to_string();
    assert!(
        message.contains(&peri_acp_types::cron::MAX_CRON_PROMPT_BYTES.to_string()),
        "拒绝必须给出可承载限制: {message}"
    );
}

/// 预算内的任务照常注册（拒绝策略不得误伤合法任务）。
#[tokio::test]
async fn test_register_accepts_prompt_within_the_carriable_budget() {
    let (reg, _, _) = new_tools();
    let prompt = "汉".repeat(1024);
    let result = reg
        .invoke(
            serde_json::json!({"expression": "* * * * *", "prompt": prompt}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await;
    assert!(result.is_ok(), "预算内任务必须注册成功: {result:?}");
}
