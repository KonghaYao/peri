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

fn new_tools_with_scheduler() -> (
    Arc<Mutex<CronScheduler>>,
    CronRegisterTool,
    CronListTool,
    CronRemoveTool,
) {
    let (tx, _rx) = mpsc::unbounded_channel();
    let scheduler = Arc::new(Mutex::new(CronScheduler::new(tx)));
    (
        scheduler.clone(),
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
    assert!(result.contains("下次触发:"), "{result}");
    assert!(result.contains("UTC"), "{result}");
    assert!(result.contains(" | prompt: test task"), "{result}");

    let list_result = list
        .invoke(
            serde_json::json!({}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert!(list_result.contains("test task"));
}

#[test]
fn register_and_list_descriptions_state_utc() {
    let (reg, list, _) = new_tools();
    assert!(
        reg.description().contains("UTC"),
        "注册描述必须说明调度按 UTC: {}",
        reg.description()
    );
    let expression = reg.parameters()["properties"]["expression"]["description"]
        .as_str()
        .expect("expression 必须有字符串描述")
        .to_string();
    assert!(
        expression.contains("UTC"),
        "expression 参数描述必须说明按 UTC 解释: {expression}"
    );
    assert!(
        list.description().contains("UTC"),
        "列表描述必须说明下次触发时间按 UTC: {}",
        list.description()
    );
}

#[tokio::test]
async fn register_receipt_reports_scheduler_next_fire() {
    let (scheduler, reg, _, _) = new_tools_with_scheduler();
    let receipt = reg
        .invoke(
            serde_json::json!({"expression": "*/5 * * * *", "prompt": "next-fire"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();

    let (id, next_fire) = {
        let guard = scheduler.lock();
        let tasks = guard.list_tasks();
        let task = tasks.first().expect("任务必须已注册");
        (task.id.clone(), task.next_fire)
    };
    assert!(receipt.contains(&id), "回执必须含完整 ID: {receipt}");
    assert!(
        receipt.contains(&format_next_fire(next_fire)),
        "回执必须含 scheduler 计算出的下次触发时间: {receipt}"
    );
}

/// F08：列表行首 token 必须是完整 UUID，且能原样交给 `cron_remove` 删除。
#[tokio::test]
async fn list_output_ids_are_full_and_remove_every_task() {
    let (reg, list, remove) = new_tools();
    for index in 0..12 {
        reg.invoke(
            serde_json::json!({
                "expression": "*/5 * * * *",
                "prompt": format!("task-{index}")
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .expect("任务注册必须成功");
    }

    let listed = list
        .invoke(
            serde_json::json!({}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    let ids: Vec<String> = listed
        .lines()
        .map(|line| {
            let rest = line
                .strip_prefix("- ")
                .unwrap_or_else(|| panic!("列表行必须以 '- ' 开头: {line}"));
            rest.split_whitespace()
                .next()
                .expect("列表行必须含 ID")
                .to_string()
        })
        .collect();
    assert_eq!(ids.len(), 12, "列表必须返回全部任务: {listed}");

    let mut unique = ids.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(unique.len(), ids.len(), "列表 ID 必须互不相同: {listed}");
    for id in &ids {
        assert_eq!(id.len(), 36, "行首必须是完整 UUID: {id}");
        assert!(
            uuid::Uuid::parse_str(id).is_ok(),
            "行首必须是合法 UUID: {id}"
        );
    }

    for id in &ids {
        remove
            .invoke(
                serde_json::json!({ "id": id }),
                peri_agent::tools::ToolContext::new(&[], "."),
            )
            .await
            .unwrap_or_else(|error| panic!("列表 ID {id} 必须能直接删除: {error}"));
    }
    assert_eq!(
        list.invoke(
            serde_json::json!({}),
            peri_agent::tools::ToolContext::new(&[], ".")
        )
        .await
        .unwrap(),
        "无定时任务"
    );
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
